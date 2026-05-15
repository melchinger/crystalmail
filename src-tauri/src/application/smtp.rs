// Outbound mail via SMTP submission. Uses the same keyring entry as IMAP
// (most providers use identical credentials for both) — once we support
// separate SMTP passwords we'll split the entry.
//
// TLS: `smtp_tls = true` → implicit TLS (port 465 typical).
//      `smtp_tls = false` → STARTTLS upgrade (port 587 typical).
//
// Because the user-configured flag is frequently wrong (the "port 587 +
// implicit TLS" misconfig is extremely common), if the first attempt fails
// with a TLS-negotiation symptom we automatically retry using the other
// variant and tell the caller which one worked.

use lettre::{
    message::{header::ContentType, Attachment, Body, Mailbox, Message, MessageBuilder, MultiPart, SinglePart},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
};
use serde::Deserialize;

use crate::domain::account::AccountId;
use crate::infrastructure::db::DbHandle;
use crate::infrastructure::imap_client;
use crate::infrastructure::queries::{self, AccountSummary};

const KEYRING_SERVICE: &str = "crystalmail";

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FromIdentity {
    pub email: String,
    pub from_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMailRequest {
    pub account_id: AccountId,
    /// Override the From: header (e.g. send as an alias like support@…).
    /// When `None`, the account's default identity is used. Authentication
    /// always uses the account's login credentials regardless.
    #[serde(default)]
    pub from: Option<FromIdentity>,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    pub subject: String,
    /// Plain-text body. Always sent (even when HTML is present) as the
    /// text/plain alternative so long-standing minimal clients still read it.
    pub body: String,
    /// Optional HTML version. When present, the outgoing message becomes
    /// multipart/alternative and, if attachments are provided, also
    /// multipart/mixed around that.
    #[serde(default)]
    pub body_html: Option<String>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    /// Absolute paths to files that should be attached. Reads happen on the
    /// (blocking) thread lettre's builder runs on; we don't bother streaming
    /// because MTA submission rewrites the whole message anyway.
    #[serde(default)]
    pub attachments: Vec<AttachmentSpec>,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentSpec {
    pub path: String,
    /// Override the filename presented in the MIME header. Falls back to the
    /// basename of `path`.
    #[serde(default)]
    pub filename: Option<String>,
    /// Override the Content-Type. Falls back to `application/octet-stream`
    /// unless we can guess from the extension.
    #[serde(default)]
    pub mime_type: Option<String>,
    /// Inline attachment (e.g. a clipboard image pasted into the HTML body).
    /// When `true`, the part is emitted with `Content-Disposition: inline`
    /// inside a `multipart/related` sibling of the HTML, and `content_id`
    /// is the value referenced as `<img src="cid:…">` in the HTML body.
    /// Recipients with rich-mail clients see the image embedded; the
    /// attachment is also still listed in the MIME tree, so plain clients
    /// or "save attachment" flows continue to work.
    #[serde(default)]
    pub is_inline: bool,
    /// CID used for `<img src="cid:…">` references in the HTML body.
    /// Only meaningful when `is_inline = true`. Required to be unique
    /// across the message; we trust the frontend to mint UUIDs.
    #[serde(default)]
    pub content_id: Option<String>,
}

/// Hard cap on `to + cc + bcc` total recipients. The number is generous
/// enough for any realistic personal or small-business send (a department
/// distribution list rarely exceeds 30) while still bounding the memory and
/// SMTP-DATA work an attacker — or a confused script — can trigger via the
/// Tauri command. Above this we refuse before opening the SMTP socket.
const MAX_RECIPIENTS_TOTAL: usize = 100;

/// Per-address length sanity. RFC 5321 §4.5.3.1 caps local-part at 64 chars
/// and domain at 255, plus the `@`, plus angle-brackets and a display-name
/// in lettre's `Mailbox` parser — 320 covers any compliant address with
/// generous slack and rejects crafted megastrings designed to choke parser
/// or transport.
const MAX_ADDRESS_LEN: usize = 320;

/// Bounce malformed or oversized recipient lists at the command boundary,
/// before keyring lookup or any SMTP roundtrip. Lettre's own `.parse()` is
/// the source of truth for *syntactic* validity (and runs later in
/// `build_message`); this layer just enforces a count cap and a per-string
/// length cap so neither path is exposed to pathological inputs.
fn validate_recipients(req: &SendMailRequest) -> Result<(), String> {
    let total = req.to.len() + req.cc.len() + req.bcc.len();
    if req.to.is_empty() {
        return Err("Mindestens ein Empfänger (An:) wird benötigt.".into());
    }
    if total > MAX_RECIPIENTS_TOTAL {
        return Err(format!(
            "Zu viele Empfänger: {total} (Limit {MAX_RECIPIENTS_TOTAL}). \
             Bitte als getrennte Mails versenden."
        ));
    }
    for (label, list) in [("An", &req.to), ("Cc", &req.cc), ("Bcc", &req.bcc)] {
        for addr in list {
            if addr.len() > MAX_ADDRESS_LEN {
                return Err(format!(
                    "{label}: Adresse überschreitet {MAX_ADDRESS_LEN} Zeichen \
                     ({} Zeichen) — vermutlich fehlerhaft formatiert.",
                    addr.len()
                ));
            }
        }
    }
    Ok(())
}

pub async fn send(db: &DbHandle, req: SendMailRequest) -> Result<(), String> {
    validate_recipients(&req)?;

    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, &req.account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?
    };

    let entry_name = format!("imap::{}", account.id.0);
    let password = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get_password: {e}"))?;

    let primary_implicit = account.smtp_tls;

    // First try: whatever the account is configured to use.
    let send_result = match try_send(&account, &password, &req, primary_implicit).await {
        Ok(rfc822) => {
            tracing::info!(
                implicit_tls = primary_implicit,
                host = %account.smtp_host,
                port = account.smtp_port,
                "SMTP send: primary mode succeeded"
            );
            Ok(rfc822)
        }
        Err(primary_err) => {
            tracing::warn!(
                implicit_tls = primary_implicit,
                error = %primary_err,
                "SMTP send: primary mode failed, trying fallback"
            );
            // Only retry if the symptom looks like a TLS negotiation mismatch.
            if !looks_like_tls_mismatch(&primary_err) {
                return Err(primary_err);
            }

            match try_send(&account, &password, &req, !primary_implicit).await {
                Ok(rfc822) => {
                    tracing::warn!(
                        fallback_implicit_tls = !primary_implicit,
                        "SMTP send: fallback mode succeeded — update the account's TLS setting to match"
                    );
                    Ok(rfc822)
                }
                Err(fallback_err) => Err(format!(
                    "SMTP beide TLS-Varianten fehlgeschlagen — Account-Einstellungen oder Provider prüfen.\n\
                     {prim_label}: {primary_err}\n\
                     {fb_label}: {fallback_err}",
                    prim_label = if primary_implicit { "Implicit TLS" } else { "STARTTLS" },
                    fb_label = if !primary_implicit { "Implicit TLS" } else { "STARTTLS" },
                )),
            }
        }
    };

    let rfc822 = send_result?;

    // Best-effort: drop a copy of the outgoing message into the account's
    // Sent folder via IMAP APPEND. Most providers don't do this server-side
    // for SMTP submission, so every other mail client also does this.
    // Failures are logged but don't fail the overall send — the message
    // already left the building; the user shouldn't see an error for a
    // cosmetic storage issue.
    //
    // Skip the APPEND wenn der Server das ohnehin automatisch macht
    // (Gmail, Office 365, Zoho.eu in der Praxis) — sonst hätten wir
    // doppelte Einträge im Sent-Ordner. Der Wert wurde beim Account-
    // Setup via Probe-Mail ermittelt und kann manuell überschrieben
    // werden.
    if account.server_stores_sent {
        tracing::info!(
            account = %account.address,
            sent_folder = %account.sent_folder,
            "APPEND skipped: server-stores-sent flag set"
        );
    } else if account.sent_folder.trim().is_empty() {
        tracing::warn!(
            account = %account.address,
            "APPEND skipped: account has no sent_folder configured"
        );
    } else {
        tracing::info!(
            account = %account.address,
            sent_folder = %account.sent_folder,
            bytes = rfc822.len(),
            "APPEND to Sent: starting"
        );
        match append_to_sent(&account, &password, &rfc822).await {
            Ok(()) => {
                tracing::info!(
                    account = %account.address,
                    sent_folder = %account.sent_folder,
                    "APPEND to Sent: ok"
                );
            }
            Err(e) => {
                tracing::warn!(
                    account = %account.address,
                    sent_folder = %account.sent_folder,
                    error = %e,
                    "APPEND to Sent failed (mail was sent successfully; folder name may be wrong, try Auto-Discovery)"
                );
            }
        }
    }

    // Compose-Send-Side-Effect: typed-in Empfänger sofort in die
    // address_history pushen damit das Autocomplete sie kennt, ohne
    // auf den nächsten IMAP-Sync der Sent-Mail warten zu müssen.
    // Bewusst ohne own-Filter: User darf an sich selbst schreiben,
    // dann soll die Adresse auch im Autocomplete auftauchen.
    let mut typed_recipients: Vec<crate::domain::message::Address> = Vec::new();
    let push_str = |out: &mut Vec<crate::domain::message::Address>, s: &str| {
        // Lettre-Mailbox akzeptiert "Name <email>" oder bare email —
        // wir parsen das hier robust, weil Compose UI komma-getrennte
        // freitext-Listen liefert.
        let parsed = parse_addr(s);
        if let Some(addr) = parsed {
            out.push(addr);
        }
    };
    for s in &req.to {
        push_str(&mut typed_recipients, s);
    }
    for s in &req.cc {
        push_str(&mut typed_recipients, s);
    }
    for s in &req.bcc {
        push_str(&mut typed_recipients, s);
    }
    if !typed_recipients.is_empty() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Err(e) = db
            .writer
            .send(crate::infrastructure::db::WriteCmd::RecordOutgoingAddresses {
                addresses: typed_recipients,
                ack: tx,
            })
            .await
        {
            tracing::warn!(error = %e, "send: history side-effect channel closed");
        } else {
            // Fire-and-forget genug, aber wir warten kurz auf Ack damit
            // wir Fehler beim Insert mit-loggen können (DB-Krempel
            // sollte nicht silent sterben).
            if let Ok(Err(e)) = rx.await {
                tracing::warn!(error = %e, "send: history side-effect db error");
            }
        }
    }

    Ok(())
}

/// Robust-Parser für freie Adresslisten-Items aus Compose. Unterstützt
/// `"Name" <email>`, `Name <email>`, und bare `email`. Liefert None
/// wenn nichts E-Mail-haftiges drin steht.
fn parse_addr(s: &str) -> Option<crate::domain::message::Address> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Format mit `<email>`-Klammer.
    if let Some(open) = s.rfind('<') {
        if let Some(close) = s.rfind('>') {
            if close > open + 1 {
                let email = s[open + 1..close].trim();
                let name_part = s[..open].trim().trim_matches('"').trim();
                if email.contains('@') {
                    return Some(crate::domain::message::Address {
                        name: if name_part.is_empty() {
                            None
                        } else {
                            Some(name_part.to_string())
                        },
                        email: email.to_string(),
                    });
                }
            }
        }
    }
    // Bare email.
    if s.contains('@') {
        return Some(crate::domain::message::Address {
            name: None,
            email: s.to_string(),
        });
    }
    None
}

/// IMAP APPEND the just-sent RFC822 bytes into the configured Sent folder,
/// marked `\Seen` (no sense showing sent mails as unread in one's own folder).
async fn append_to_sent(
    account: &AccountSummary,
    password: &str,
    rfc822: &[u8],
) -> Result<(), String> {
    append_to_folder(account, password, &account.sent_folder, "(\\Seen)", rfc822).await
}

/// Generischer APPEND-Helper. Übernimmt Connect+Login+APPEND+Logout für
/// einen beliebigen Ordner mit beliebigen Flag-String. Für die Drafts-
/// Ordner-Variante setzt der Caller `(\\Draft \\Seen)`. Logging ist
/// granular, damit Production-Fehler auf den genauen Schritt zeigen
/// (connect / login / append).
async fn append_to_folder(
    account: &AccountSummary,
    password: &str,
    folder: &str,
    flags: &str,
    rfc822: &[u8],
) -> Result<(), String> {
    tracing::debug!(
        host = %account.imap_host,
        port = account.imap_port,
        folder = %folder,
        "APPEND: TLS connect"
    );
    let client = imap_client::connect_tls(&account.imap_host, account.imap_port)
        .await
        .map_err(|e| format!("IMAP connect {}: {e}", account.imap_host))?;

    tracing::debug!(user = %account.address, folder = %folder, "APPEND: LOGIN");
    let mut session = client
        .login(&account.address, password)
        .await
        .map_err(|(e, _)| format!("IMAP LOGIN: {e}"))?;

    tracing::debug!(
        folder = %folder,
        bytes = rfc822.len(),
        flags = %flags,
        "APPEND: sending command"
    );
    let res = session
        .append(folder, Some(flags), None, rfc822)
        .await
        .map_err(|e| format!("APPEND \"{folder}\": {e}"));

    // Always try to log out cleanly, even on append error.
    let _ = session.logout().await;

    res
}

/// Speichert die zusammengesetzte Mail als Entwurf im Drafts-Ordner des
/// Accounts (IMAP APPEND mit Flags `\Draft \Seen`). Anders als beim Senden
/// gibt es hier keine Validate-Phase für Empfänger — Drafts dürfen unfertig
/// sein, das ist ja gerade ihr Sinn.
///
/// Nutzt denselben `build_message`-Pfad wie der echte Send, sodass der
/// Entwurf strukturell identisch zur späteren Sendung ist (gleiche
/// Header, gleiche Anhänge, gleiche Body-Variante).
pub async fn save_as_draft(db: &DbHandle, req: SendMailRequest) -> Result<(), String> {
    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, &req.account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?
    };

    if account.drafts_folder.trim().is_empty() {
        return Err(
            "Account hat keinen Drafts-Ordner konfiguriert. Bitte unter \"Konten\" \
             den Drafts-Ordner setzen oder per Auto-Erkennung ermitteln lassen."
                .to_string(),
        );
    }

    let entry_name = format!("imap::{}", account.id.0);
    let password = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get_password: {e}"))?;

    let message = build_message(&account, &req)?;
    let rfc822 = message.formatted();

    append_to_folder(
        &account,
        &password,
        &account.drafts_folder,
        "(\\Draft \\Seen)",
        &rfc822,
    )
    .await
}

/// Schickt eine selbst-adressierte Test-Mail mit eindeutiger Message-Id,
/// wartet 5s, prüft den Sent-Ordner: wenn die Mail dort liegt, hat der
/// SMTP-Server sie automatisch eingelegt (→ unser send-Pfad muss seinen
/// eigenen APPEND skippen). Räumt anschließend Sent + INBOX auf, sodass
/// die Probe keine Spuren im Postfach hinterlässt.
///
/// Konkrete Provider-Beobachtungen:
/// - Gmail, Office 365, Zoho.eu: speichern automatisch → Probe sieht die Mail in Sent
/// - Zoho.com, Fastmail (Default), die meisten selbst-gehosteten: Probe findet
///   die Mail nicht in Sent (nur in der INBOX wegen Self-Send)
///
/// Fail-Modi:
/// - SMTP-Send schlägt fehl: Probe gibt Err zurück, Caller fällt auf Default
///   `false` zurück (= APPEND aktiv lassen, status quo).
/// - IMAP-Suche schlägt fehl: Wir nehmen "kein Auto-Save" an und loggen nur.
/// - Cleanup schlägt fehl: nicht-fatal, der Subject erklärt sich selbst.
pub async fn probe_server_stores_sent(
    account: &AccountSummary,
    password: &str,
) -> Result<bool, String> {
    let probe_id_local = format!("crystalmail-probe-{}", uuid::Uuid::new_v4());

    // Minimal Self-Mail bauen. Lettre wrapped die Message-Id selbst in <>.
    let from_mailbox: Mailbox =
        format!("{} <{}>", account.from_name, account.address)
            .parse()
            .map_err(|e| format!("Probe-Absender ungültig: {e}"))?;
    let to_mailbox: Mailbox = account
        .address
        .parse()
        .map_err(|e| format!("Probe-Empfänger ungültig: {e}"))?;
    let message = Message::builder()
        .from(from_mailbox)
        .to(to_mailbox)
        .subject("CrystalMail Setup-Test (wird automatisch gelöscht)")
        .message_id(Some(format!("{probe_id_local}@crystalmail.local")))
        .header(ContentType::TEXT_PLAIN)
        .body(String::from(
            "Dies ist eine automatische Test-Mail von CrystalMail beim \
             Einrichten deines Kontos. Sie wird nach kurzer Zeit \
             automatisch gelöscht. Du kannst sie ignorieren.\r\n",
        ))
        .map_err(|e| format!("Probe-Message: {e}"))?;

    // SMTP submit. Ein einzelner Versuch — keine TLS-Mode-Fallback-Logik
    // wie in `try_send`, weil die Account-Anlage ohnehin schon den
    // login-Test via `imap_client::test_login` durchläuft. Wenn SMTP
    // hier scheitert, ist die Account-Anlage suspekt und der Caller
    // fällt sowieso auf den `false`-Default zurück.
    let creds = Credentials::new(account.address.clone(), password.to_string());
    let mailer = if account.smtp_tls {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&account.smtp_host)
            .map_err(|e| format!("SMTP relay: {e}"))?
            .port(account.smtp_port)
            .credentials(creds)
            .build()
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&account.smtp_host)
            .map_err(|e| format!("SMTP STARTTLS: {e}"))?
            .port(account.smtp_port)
            .credentials(creds)
            .build()
    };
    mailer
        .send(message)
        .await
        .map_err(|e| format!("Probe SMTP send: {e}"))?;

    // Server brauchen einen Moment zum Indizieren — 5s reicht in der
    // Praxis bei Gmail/O365/Zoho. Weniger ist riskant (false-negative),
    // mehr blockiert die Account-Anlage spürbar.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // IMAP-Verbindung für Suche + Cleanup.
    let client = imap_client::connect_tls(&account.imap_host, account.imap_port)
        .await
        .map_err(|e| format!("Probe IMAP connect: {e}"))?;
    let mut session = client
        .login(&account.address, password)
        .await
        .map_err(|(e, _)| format!("Probe IMAP LOGIN: {e}"))?;

    // Sent-Folder prüfen. `HEADER Message-Id ...` macht in IMAP einen
    // Substring-Match — die UUID-Komponente alleine reicht zur eindeutigen
    // Identifikation, ohne dass wir uns mit Anführungszeichen oder
    // Angle-Brackets in der Search-Syntax verheddern.
    let server_stored = match probe_check_in(&mut session, &account.sent_folder, &probe_id_local).await {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(
                account = %account.address,
                error = %e,
                "Probe: Sent-Folder-Check fehlgeschlagen — nehme an Server speichert NICHT automatisch"
            );
            false
        }
    };

    // Sent-Cleanup falls dort gefunden.
    if server_stored {
        let _ = probe_cleanup_in(&mut session, &account.sent_folder, &probe_id_local).await;
    }

    // INBOX-Cleanup: Self-Send wird IMMER an die Inbox zugestellt,
    // unabhängig vom Auto-Save-Verhalten. Hier räumen wir die Probe-
    // Mail aus der Inbox raus, damit der User morgens keinen "Setup-
    // Test"-Eintrag findet.
    if let Err(e) = probe_cleanup_in(&mut session, "INBOX", &probe_id_local).await {
        tracing::warn!(
            account = %account.address,
            error = %e,
            "Probe: INBOX-Cleanup fehlgeschlagen (Subject macht klar dass die Mail manuell gelöscht werden kann)"
        );
    }

    let _ = session.logout().await;
    Ok(server_stored)
}

/// SELECT folder + UID SEARCH HEADER. Liefert true wenn die Probe-Mail
/// drinliegt. Reine Inspektion, kein Cleanup.
async fn probe_check_in(
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    folder: &str,
    probe_id_local: &str,
) -> Result<bool, String> {
    session
        .select(folder)
        .await
        .map_err(|e| format!("SELECT {folder}: {e}"))?;
    // RFC 3501: HEADER takes a field-name + value, beides als atom oder
    // string. Substring-Match — die eindeutige UUID identifiziert nur
    // unsere Probe-Mail.
    let criterion = format!("HEADER Message-Id \"{probe_id_local}\"");
    let uids: std::collections::HashSet<u32> = session
        .uid_search(&criterion)
        .await
        .map_err(|e| format!("UID SEARCH ({folder}): {e}"))?;
    Ok(!uids.is_empty())
}

/// Findet die Probe-Mail im genannten Folder und entsorgt sie via
/// `\Deleted` + EXPUNGE. Best-effort — Fehler werden propagiert aber
/// die Caller loggen sie nur.
async fn probe_cleanup_in(
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    folder: &str,
    probe_id_local: &str,
) -> Result<(), String> {
    use futures_util::StreamExt;

    session
        .select(folder)
        .await
        .map_err(|e| format!("SELECT {folder}: {e}"))?;
    let criterion = format!("HEADER Message-Id \"{probe_id_local}\"");
    let uids: std::collections::HashSet<u32> = session
        .uid_search(&criterion)
        .await
        .map_err(|e| format!("UID SEARCH ({folder}): {e}"))?;

    if uids.is_empty() {
        return Ok(());
    }

    // \Deleted setzen pro UID, jeden uid_store-Stream bis zum Ende drainen
    // (sonst bleibt das Pipelining hängen und der nächste Befehl liest
    // alte Server-Antworten als seine eigenen). UID-Set ist klein
    // (≤ 1 Probe-Mail), keine Batch-Optimierung nötig.
    for uid in &uids {
        match session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
            .await
        {
            Ok(stream) => {
                tokio::pin!(stream);
                while stream.next().await.is_some() {}
            }
            Err(e) => {
                tracing::debug!(uid = uid, error = %e, "Probe-UID-STORE fehlgeschlagen");
            }
        }
    }

    // EXPUNGE finalisiert die Löschung. Stream drainen, Einzelfehler nur
    // loggen — die Mail ist mit \Deleted markiert und wird beim nächsten
    // EXPUNGE / SELECT vom Server entfernt.
    match session.expunge().await {
        Ok(stream) => {
            tokio::pin!(stream);
            while let Some(r) = stream.next().await {
                if let Err(e) = r {
                    tracing::debug!("Probe-EXPUNGE Stream-Eintrag: {e}");
                }
            }
        }
        Err(e) => {
            return Err(format!("EXPUNGE {folder}: {e}"));
        }
    }
    Ok(())
}

/// Returns the serialized RFC822 bytes on success so the caller can reuse
/// them for the IMAP APPEND to Sent (avoids rebuilding the message twice).
async fn try_send(
    account: &AccountSummary,
    password: &str,
    req: &SendMailRequest,
    use_implicit_tls: bool,
) -> Result<Vec<u8>, String> {
    let message = build_message(account, req)?;
    let rfc822 = message.formatted();

    let creds = Credentials::new(account.address.clone(), password.to_string());
    let mailer = if use_implicit_tls {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&account.smtp_host)
            .map_err(|e| format!("SMTP relay: {e}"))?
            .port(account.smtp_port)
            .credentials(creds)
            .build()
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&account.smtp_host)
            .map_err(|e| format!("SMTP STARTTLS: {e}"))?
            .port(account.smtp_port)
            .credentials(creds)
            .build()
    };

    mailer
        .send(message)
        .await
        .map(|_| rfc822)
        .map_err(|e| format!("{e}"))
}

fn build_message(account: &AccountSummary, req: &SendMailRequest) -> Result<Message, String> {
    let (from_name, from_email) = match &req.from {
        Some(id) if !id.email.trim().is_empty() => (id.from_name.clone(), id.email.clone()),
        _ => (account.from_name.clone(), account.address.clone()),
    };
    let from_mailbox: Mailbox = format!("{from_name} <{from_email}>")
        .parse()
        .map_err(|e| format!("Absender ungültig: {e}"))?;

    let mut builder: MessageBuilder = Message::builder()
        .from(from_mailbox)
        .subject(&req.subject);

    for addr in &req.to {
        let mb: Mailbox = addr
            .parse()
            .map_err(|e| format!("Empfänger '{addr}' ungültig: {e}"))?;
        builder = builder.to(mb);
    }
    for addr in &req.cc {
        let mb: Mailbox = addr
            .parse()
            .map_err(|e| format!("Cc '{addr}' ungültig: {e}"))?;
        builder = builder.cc(mb);
    }
    for addr in &req.bcc {
        let mb: Mailbox = addr
            .parse()
            .map_err(|e| format!("Bcc '{addr}' ungültig: {e}"))?;
        builder = builder.bcc(mb);
    }

    if let Some(parent_id) = &req.in_reply_to {
        builder = builder.in_reply_to(ensure_angle_brackets(parent_id));
    }
    if !req.references.is_empty() {
        let refs = req
            .references
            .iter()
            .map(|s| ensure_angle_brackets(s))
            .collect::<Vec<_>>()
            .join(" ");
        builder = builder.references(refs);
    }

    // Body shape:
    //   attachments + html  → mixed(alternative(plain, html), <file...>)
    //   attachments + plain → mixed(plain, <file...>)
    //   html (no atts)      → alternative(plain, html)
    //   plain (no atts)     → single text/plain
    //   iMIP (RFC 6047)     → alternative(plain, text/calendar)
    //                         — overrides the above when the only attachment
    //                         is `text/calendar` with a `method=` parameter.
    let has_attachments = !req.attachments.is_empty();
    let has_html = req.body_html.as_deref().map(|s| !s.is_empty()).unwrap_or(false);

    if !has_attachments && !has_html {
        return builder
            .header(ContentType::TEXT_PLAIN)
            .body(req.body.clone())
            .map_err(|e| format!("Message bauen: {e}"))
    }

    // Diagnostic-first: log every dispatch so we can verify which path the
    // outgoing message went through without guessing from RFC822 dumps.
    let first_mime = req
        .attachments
        .first()
        .and_then(|a| a.mime_type.as_deref())
        .unwrap_or("(none)");
    let imip_match = req.attachments.len() == 1 && is_imip_attachment(&req.attachments[0]);
    tracing::info!(
        attachments = req.attachments.len(),
        first_mime = %first_mime,
        has_html = has_html,
        imip = imip_match,
        "send: build_message dispatch"
    );

    // iMIP detection: a single text/calendar attachment carrying a `method=`
    // parameter is the signature of a calendar invitation reply/request. In
    // that case the spec (RFC 6047 §2.4) and field practice both expect the
    // calendar body to be a multipart/alternative sibling of the text body —
    // not wrapped in a multipart/mixed as an attachment. Outlook and several
    // groupware servers (Zoho among them) only auto-process the iMIP message
    // when they find the calendar payload at the alternative level. Falling
    // through to multipart/mixed produces a mail that arrives but is
    // displayed as a normal message with an .ics attachment instead.
    //
    // We allow `has_html` because Compose's rich editor always emits an HTML
    // body even for plain user input — Outlook/Apple Mail also produce a
    // three-part alternative (plain + html + calendar) for invitation
    // replies, so we match that shape rather than dropping the html part.
    if imip_match {
        return build_imip_alternative(builder, req, has_html);
    }

    // Partition attachments: inline-with-content-id (referenced as
    // `<img src="cid:…">` from the HTML body) need to sit inside a
    // multipart/related sibling of the HTML part, so the recipient's
    // MUA links the cid: reference to the image data. Everything else
    // continues to land at multipart/mixed level as a file attachment.
    let (inline_specs, regular_specs): (Vec<&AttachmentSpec>, Vec<&AttachmentSpec>) =
        req.attachments.iter().partition(|a| {
            a.is_inline && a.content_id.is_some() && has_html
        });
    let has_regular = !regular_specs.is_empty();
    let has_inline = !inline_specs.is_empty();

    let text_part = SinglePart::builder()
        .header(ContentType::TEXT_PLAIN)
        .body(req.body.clone());

    // Build the "content half" of the message — the user's body proper,
    // possibly wrapped with inline images via multipart/related.
    let content_part = if has_html {
        let html_part = SinglePart::builder()
            .header(ContentType::TEXT_HTML)
            .body(req.body_html.clone().unwrap_or_default());
        if has_inline {
            // multipart/related wraps the HTML + each inline image. The
            // HTML must be the *first* part so RFC-conforming clients
            // pick it as the start of the related group.
            let mut related = MultiPart::related().singlepart(html_part);
            for spec in &inline_specs {
                related = related.singlepart(build_inline_part(spec)?);
            }
            MultiPart::alternative()
                .singlepart(text_part)
                .multipart(related)
        } else {
            MultiPart::alternative()
                .singlepart(text_part)
                .singlepart(html_part)
        }
    } else {
        // We still wrap in MultiPart::mixed below if attachments are present;
        // for that we need a MultiPart, so promote to a trivial alternative
        // with just the text part.
        MultiPart::alternative().singlepart(text_part)
    };

    if !has_regular {
        return builder
            .multipart(content_part)
            .map_err(|e| format!("Message bauen: {e}"));
    }

    let mut mixed = MultiPart::mixed().multipart(content_part);
    for spec in &regular_specs {
        mixed = mixed.singlepart(build_regular_attachment_part(spec)?);
    }

    builder
        .multipart(mixed)
        .map_err(|e| format!("Message bauen: {e}"))
}

/// Build a `multipart/related` inline part with a `Content-ID` header
/// so the HTML body's `<img src="cid:…">` reference resolves. lettre's
/// `Attachment::new_inline(cid)` produces a SinglePart with
/// `Content-Disposition: inline; filename=…` and the matching
/// `Content-ID: <cid>` header — exactly what's needed.
fn build_inline_part(spec: &AttachmentSpec) -> Result<SinglePart, String> {
    let path = std::path::Path::new(&spec.path);
    let bytes = std::fs::read(path)
        .map_err(|e| format!("Inline-Bild lesen ({}): {e}", spec.path))?;
    let mime = spec
        .mime_type
        .clone()
        .unwrap_or_else(|| guess_mime(path));
    let content_type = ContentType::parse(&mime)
        .map_err(|e| format!("ungültiger MIME-Typ '{mime}': {e}"))?;
    // `content_id` is guaranteed Some by the caller's partition gate,
    // but be defensive — clone it out without panicking if the gate
    // ever changes.
    let cid = spec
        .content_id
        .clone()
        .ok_or_else(|| "Inline-Attachment ohne content_id".to_string())?;
    // lettre's `new_inline` sets `Content-Disposition: inline` + the
    // matching `Content-ID: <cid>` header. Inline parts don't carry a
    // filename — the HTML `<img>` tag's `alt` text is what surfaces in
    // a recipient client, not the part filename.
    Ok(Attachment::new_inline(cid).body(Body::new(bytes), content_type))
}

/// Existing path for normal file attachments — disposition: attachment,
/// no Content-ID, no inline embedding. Pulled out into its own helper
/// so the inline branch can reuse the bytes-read / filename-fallback
/// logic without duplicating it inline.
fn build_regular_attachment_part(spec: &AttachmentSpec) -> Result<SinglePart, String> {
    let path = std::path::Path::new(&spec.path);
    let bytes = std::fs::read(path)
        .map_err(|e| format!("Datei lesen ({}): {e}", spec.path))?;
    let filename = spec
        .filename
        .clone()
        .or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "attachment.bin".into());
    let mime = spec
        .mime_type
        .clone()
        .unwrap_or_else(|| guess_mime(path));
    let content_type = ContentType::parse(&mime)
        .map_err(|e| format!("ungültiger MIME-Typ '{mime}': {e}"))?;
    let body = Body::new(bytes);
    Ok(Attachment::new(filename).body(body, content_type))
}

/// True when the attachment looks like an iMIP calendar payload — i.e. the
/// caller wants the SMTP path to embed it as a multipart/alternative body
/// part, not wrap it as a multipart/mixed attachment. We detect on the
/// `method=` Content-Type parameter, which RFC 6047 mandates and which
/// non-iMIP plain-`text/calendar` files (e.g. exported .ics from a calendar
/// view) won't carry.
fn is_imip_attachment(spec: &AttachmentSpec) -> bool {
    let Some(mime) = spec.mime_type.as_deref() else {
        return false;
    };
    let lower = mime.to_ascii_lowercase();
    let mut parts = lower.split(';').map(|s| s.trim());
    let Some(top) = parts.next() else {
        return false;
    };
    if top != "text/calendar" {
        return false;
    }
    parts.any(|p| p.starts_with("method="))
}

/// Build an iMIP-shaped message: top-level `multipart/alternative` of
///   text/plain + (optional) text/html + text/calendar
///
/// The text/calendar part carries the full Content-Type including `method=`.
/// We route it through `SinglePart::builder()` rather than the regular
/// `Attachment::new(...)` helper so we can stamp the exact Content-Type and
/// avoid the `Content-Disposition: attachment` that `Attachment` would
/// impose — receiving calendar servers (Zoho, Outlook, Google) want the
/// payload as an alternative body, not as a downloadable attachment.
fn build_imip_alternative(
    builder: MessageBuilder,
    req: &SendMailRequest,
    include_html: bool,
) -> Result<Message, String> {
    let spec = &req.attachments[0];
    let path = std::path::Path::new(&spec.path);
    let bytes = std::fs::read(path)
        .map_err(|e| format!("iMIP-ICS lesen ({}): {e}", spec.path))?;
    let ics_text = String::from_utf8(bytes)
        .map_err(|e| format!("iMIP-ICS ist kein UTF-8: {e}"))?;
    let mime = spec
        .mime_type
        .as_deref()
        .ok_or("iMIP-Anhang ohne Content-Type")?;
    let calendar_ct = ContentType::parse(mime)
        .map_err(|e| format!("iMIP Content-Type ungültig '{mime}': {e}"))?;

    let text_part = SinglePart::builder()
        .header(ContentType::TEXT_PLAIN)
        .body(req.body.clone());
    let calendar_part = SinglePart::builder()
        .header(calendar_ct)
        .body(ics_text);

    let mut alternative = MultiPart::alternative().singlepart(text_part);
    if include_html {
        let html_body = req.body_html.clone().unwrap_or_default();
        let html_part = SinglePart::builder()
            .header(ContentType::TEXT_HTML)
            .body(html_body);
        alternative = alternative.singlepart(html_part);
    }
    alternative = alternative.singlepart(calendar_part);

    builder
        .multipart(alternative)
        .map_err(|e| format!("iMIP Message bauen: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(mime: Option<&str>) -> AttachmentSpec {
        AttachmentSpec {
            path: "/tmp/x.ics".into(),
            filename: None,
            mime_type: mime.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn imip_detect_positive_with_method() {
        assert!(is_imip_attachment(&spec(Some(
            "text/calendar; method=REPLY; charset=utf-8"
        ))));
    }

    #[test]
    fn imip_detect_positive_with_method_lowercase_first() {
        assert!(is_imip_attachment(&spec(Some(
            "text/calendar; charset=utf-8; method=request"
        ))));
    }

    #[test]
    fn imip_detect_negative_without_method() {
        // A plain calendar export — should ride the regular attachment path.
        assert!(!is_imip_attachment(&spec(Some(
            "text/calendar; charset=utf-8"
        ))));
    }

    #[test]
    fn imip_detect_negative_other_mime() {
        assert!(!is_imip_attachment(&spec(Some("application/pdf"))));
    }

    #[test]
    fn imip_detect_negative_no_mime() {
        assert!(!is_imip_attachment(&spec(None)));
    }
}

fn guess_mime(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("zip") => "application/zip",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn ensure_angle_brackets(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        trimmed.to_string()
    } else {
        format!("<{trimmed}>")
    }
}

/// Heuristic: did this error look like "client did TLS, server expected
/// plaintext (or vice versa)"? Used to decide whether flipping the TLS
/// mode is worth trying — we don't want to double-authenticate on, say,
/// a wrong-password error.
fn looks_like_tls_mismatch(msg: &str) -> bool {
    let needles = [
        "InvalidContentType",
        "corrupt message",
        "received corrupt",
        "PeerIncompatible",
        "HandshakeFailure",
        "UnexpectedMessage",
        "tls handshake",
        "TLS handshake",
        "starttls",
        "STARTTLS",
        "unexpected eof",
        "BadRecordMac",
    ];
    needles.iter().any(|n| msg.contains(n))
}
