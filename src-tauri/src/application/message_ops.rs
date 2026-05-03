// Message move operations: archive + delete (= move to trash).
//
// IMAP move strategy:
//   * Try UID MOVE first (RFC 6851) — atomic, server removes from source in
//     one shot. Supported by Dovecot, Cyrus, Gmail, Outlook.com, most others.
//   * Fallback to UID COPY + UID STORE +FLAGS (\Deleted) + UID EXPUNGE
//     (RFC 4315 UIDPLUS) — works on servers without MOVE.
//   * Ultimate fallback: EXPUNGE without UID — also expunges any *other*
//     messages in the folder that were already \Deleted, which is the
//     standard IMAP semantic the user implicitly asked for.
//
// After a successful server-side move we drop the envelope from our local
// DB so the UI updates immediately. The next folder sync picks the message
// up in its new home.

use tokio::sync::oneshot;

use crate::domain::message::MessageId;
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::imap_client;
use crate::infrastructure::queries::{self, AccountSummary, EnvelopeDetail};

const KEYRING_SERVICE: &str = "crystalmail";

pub async fn archive(db: &DbHandle, message_id: MessageId) -> Result<(), String> {
    let (envelope, account, src_folder) = load_context(db, &message_id).await?;
    if account.archive_folder.trim().is_empty() {
        return Err("Kein Archiv-Ordner konfiguriert.".into());
    }
    if src_folder == account.archive_folder {
        return Err("Nachricht liegt bereits im Archiv.".into());
    }
    move_message(db, &envelope, &account, &src_folder, &account.archive_folder).await
}

/// Move a message to an arbitrary folder on the same account. Validates that
/// the destination exists for the account (via the `folders` table) so a
/// typo from the UI can't silently land the mail in a folder the server
/// has to auto-create. The server-side operation itself runs through the
/// same UID MOVE → COPY+\Deleted+EXPUNGE path used by archive/delete.
pub async fn move_to(
    db: &DbHandle,
    message_id: MessageId,
    dst_folder: String,
) -> Result<(), String> {
    let (envelope, account, src_folder) = load_context(db, &message_id).await?;
    let trimmed = dst_folder.trim();
    if trimmed.is_empty() {
        return Err("Kein Zielordner angegeben.".into());
    }
    if src_folder == trimmed {
        return Err("Die Nachricht liegt bereits in diesem Ordner.".into());
    }

    // Sanity check: the destination must be a folder we know for this
    // account. list_account_folders drives the sidebar expander, so if the
    // folder is in the list, it's real on the server.
    let is_valid = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let folders = queries::list_account_folders(&conn, &account.id)
            .map_err(|e| e.to_string())?;
        folders.iter().any(|f| f.name == trimmed)
    };
    if !is_valid {
        return Err(format!(
            "Zielordner '{trimmed}' nicht bekannt für dieses Konto."
        ));
    }

    move_message(db, &envelope, &account, &src_folder, trimmed).await
}

pub async fn delete(db: &DbHandle, message_id: MessageId) -> Result<(), String> {
    let (envelope, account, src_folder) = load_context(db, &message_id).await?;
    if account.trash_folder.trim().is_empty() {
        return Err("Kein Papierkorb-Ordner konfiguriert.".into());
    }
    if src_folder == account.trash_folder {
        // Second "delete" from Trash → permanent delete. Uses the same
        // EXPUNGE path the COPY fallback takes, just without the copy.
        return permanent_delete(db, &envelope, &account, &src_folder).await;
    }
    move_message(db, &envelope, &account, &src_folder, &account.trash_folder).await
}

async fn load_context(
    db: &DbHandle,
    message_id: &MessageId,
) -> Result<(EnvelopeDetail, AccountSummary, String), String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let envelope = queries::get_envelope(&conn, message_id)
        .map_err(|e| e.to_string())?
        .ok_or("Nachricht nicht gefunden.")?;
    let account = queries::get_account(&conn, &envelope.account_id)
        .map_err(|e| e.to_string())?
        .ok_or("Konto existiert nicht mehr.")?;
    let src_folder = envelope.folder_name.clone();
    Ok((envelope, account, src_folder))
}

async fn move_message(
    db: &DbHandle,
    envelope: &EnvelopeDetail,
    account: &AccountSummary,
    src_folder: &str,
    dst_folder: &str,
) -> Result<(), String> {
    let password = keyring_password(&account.id.0.to_string())?;
    let client = imap_client::connect_tls(&account.imap_host, account.imap_port).await?;
    let mut session = client
        .login(&account.address, &password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;
    session
        .select(src_folder)
        .await
        .map_err(|e| format!("SELECT {src_folder}: {e}"))?;

    let uid_str = envelope.imap_uid.to_string();

    // Pre-check: does this UID even still exist on the server? If not,
    // skip the destructive IMAP operation entirely and just clean up
    // the local row. Doing the check *before* any UID MOVE/COPY/STORE
    // sidesteps a class of failure modes where a NO-response on the
    // first command leaves the session in a state where post-hoc
    // verification (UID SEARCH) is unreliable. Costs one extra IMAP
    // round-trip on the happy path; trivially fast.
    //
    // Classic ghost-UID scenario: user already moved/deleted the mail
    // via CrystalMail, the client crashed before the local DELETE
    // landed — server-side state is ahead of local. If the message
    // is in fact still alive in some other folder, the next sync of
    // that folder picks it up.
    if confirm_uid_gone(&mut session, &uid_str).await {
        tracing::info!(
            src = %src_folder,
            uid = %uid_str,
            "UID bereits am Server weg — überspringe IMAP-Move, nur lokales Cleanup"
        );
        let _ = session.logout().await;
        return purge_envelope_local(db, envelope).await;
    }

    // Try mit dem konfigurierten Folder-Namen; bei einem namespace-
    // Fehler retry mit `INBOX.`-Prefix. Manche Server (Zoho.eu, Dovecot
    // mit personal-namespace = `INBOX.`) erlauben SELECT auf "Trash"
    // aber verlangen "INBOX.Trash" für UID COPY/MOVE — Server-Bug aus
    // unserer Sicht, aber nicht selten genug um zu ignorieren.
    //
    // `resolved_dst` ist der Name, der tatsächlich funktioniert. Wenn er
    // sich vom konfigurierten Namen unterscheidet, persistieren wir den
    // unten in den Account-Settings, damit die nächste Operation gleich
    // den richtigen Namen nimmt.
    let resolved_dst: String =
        match try_move_or_copy(&mut session, &uid_str, dst_folder).await {
            Ok(()) => dst_folder.to_string(),
            Err(e) if looks_like_namespace_error(&e) && !dst_folder.starts_with("INBOX.") => {
                let prefixed = format!("INBOX.{dst_folder}");
                tracing::warn!(
                    original = %dst_folder,
                    retry = %prefixed,
                    "namespace error, retry mit INBOX-Prefix"
                );
                try_move_or_copy(&mut session, &uid_str, &prefixed).await?;
                prefixed
            }
            Err(e) => return Err(e),
        };

    let _ = session.logout().await;

    // Folder-Name in der Account-Tabelle korrigieren, falls der Retry
    // mit Prefix gegriffen hat. Best-Effort — Fehler werden geloggt aber
    // nicht propagiert, weil die eigentliche Move/Delete-Operation ja
    // schon erfolgreich war und der User den Wert auch manuell in den
    // Settings korrigieren kann.
    if resolved_dst != dst_folder {
        if let Err(e) =
            persist_folder_name_correction(db, account, dst_folder, &resolved_dst).await
        {
            tracing::warn!(error = %e, "konnte korrigierten Folder-Namen nicht persistieren");
        }
    }

    // Local DB: drop the envelope. A later sync will pull it from dst_folder.
    purge_envelope_local(db, envelope).await
}

async fn permanent_delete(
    db: &DbHandle,
    envelope: &EnvelopeDetail,
    account: &AccountSummary,
    src_folder: &str,
) -> Result<(), String> {
    let password = keyring_password(&account.id.0.to_string())?;
    let client = imap_client::connect_tls(&account.imap_host, account.imap_port).await?;
    let mut session = client
        .login(&account.address, &password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;
    session
        .select(src_folder)
        .await
        .map_err(|e| format!("SELECT {src_folder}: {e}"))?;

    let uid_str = envelope.imap_uid.to_string();

    // Pre-check (same as move_message): if the UID is already gone
    // server-side, skip STORE+EXPUNGE and just drop the local row.
    if confirm_uid_gone(&mut session, &uid_str).await {
        tracing::info!(
            src = %src_folder,
            uid = %uid_str,
            "permanent_delete: UID bereits am Server weg — nur lokales Cleanup"
        );
        let _ = session.logout().await;
        return purge_envelope_local(db, envelope).await;
    }

    {
        let mut st = session
            .uid_store(&uid_str, "+FLAGS.SILENT (\\Deleted)")
            .await
            .map_err(|e| format!("UID STORE +\\Deleted: {e}"))?;
        use futures_util::StreamExt;
        while let Some(r) = st.next().await {
            if let Err(e) = r {
                tracing::warn!("STORE stream error: {e}");
            }
        }
    }

    expunge_uid_or_all(&mut session, &uid_str).await;

    let _ = session.logout().await;

    purge_envelope_local(db, envelope).await
}

/// Verify via UID SEARCH that the given UID no longer exists in the
/// currently-selected mailbox. Used as a fallback after a MOVE/COPY/STORE
/// failure to distinguish "UID is genuinely gone" from "real server
/// error". Returns `true` if the UID is confirmed absent (safe to
/// proceed with local cleanup), `false` on any other outcome — including
/// the SEARCH itself failing, which is the conservative default
/// (don't pretend a delete succeeded when the server's state is
/// unclear).
async fn confirm_uid_gone(
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    uid_str: &str,
) -> bool {
    match session.uid_search(format!("UID {uid_str}")).await {
        Ok(set) => set.is_empty(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                uid = %uid_str,
                "confirm_uid_gone: UID SEARCH failed; assuming UID still present"
            );
            false
        }
    }
}

/// Drop the local envelope row for a message whose server-side
/// counterpart is already gone (or where we just confirmed it gone via
/// `confirm_uid_gone`). Identical DB write to the success path of
/// move/delete — the only difference is that no IMAP write happened.
async fn purge_envelope_local(
    db: &DbHandle,
    envelope: &EnvelopeDetail,
) -> Result<(), String> {
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteEnvelopes {
            folder_id: envelope.folder_id,
            imap_uids: vec![envelope.imap_uid],
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete envelope: {e}"))?;
    Ok(())
}

/// Try UID EXPUNGE (UIDPLUS, RFC 4315); if the server rejects it, fall back
/// to plain EXPUNGE. Both return untagged response streams that must be
/// drained. Errors are logged and swallowed — if EXPUNGE fails outright,
/// the message is left with `\Deleted` on the server and will vanish at
/// the next expunge the server performs on its own.
async fn expunge_uid_or_all(
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    uid_set: &str,
) {
    use futures_util::StreamExt;

    // Borrow-checker trick: resolve the UID EXPUNGE attempt to a flat flag
    // so the session borrow it implies is fully released before we attempt
    // the plain EXPUNGE fallback in a second, independent statement.
    let uid_ok = {
        match session.uid_expunge(uid_set).await {
            Ok(stream) => {
                tokio::pin!(stream);
                while let Some(r) = stream.next().await {
                    if let Err(e) = r {
                        tracing::warn!("UID EXPUNGE stream error: {e}");
                    }
                }
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "UID EXPUNGE failed, trying plain EXPUNGE");
                false
            }
        }
    };

    if !uid_ok {
        match session.expunge().await {
            Ok(stream) => {
                tokio::pin!(stream);
                while let Some(r) = stream.next().await {
                    if let Err(e) = r {
                        tracing::warn!("EXPUNGE stream error: {e}");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "EXPUNGE failed"),
        }
    }
}

fn keyring_password(account_id: &str) -> Result<String, String> {
    let entry_name = format!("imap::{account_id}");
    keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get: {e}"))
}

/// UID MOVE versuchen, bei Bedarf auf UID COPY + \Deleted + EXPUNGE
/// zurückfallen. Der einzige beobachtbare Effekt nach Erfolg ist:
/// die Mail liegt jetzt im `dst_folder`. Fehler kommen als String mit
/// dem Original-Server-Text, sodass die Caller sie pattern-matchen
/// können (siehe `looks_like_namespace_error`).
async fn try_move_or_copy(
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    uid_str: &str,
    dst_folder: &str,
) -> Result<(), String> {
    use futures_util::StreamExt;

    // 1) UID MOVE — schneller, atomarer, RFC 6851. Manche Server
    // beherrschen ihn nicht, deshalb der Fallback weiter unten.
    match session.uid_mv(uid_str, dst_folder).await {
        Ok(()) => {
            tracing::info!(uid = %uid_str, dst = %dst_folder, "UID MOVE ok");
            return Ok(());
        }
        Err(e) => {
            // Wenn das ein namespace-Problem ist, wollen wir sofort
            // den vollständigen Fehler nach oben durchreichen — der
            // Caller probiert dann mit `INBOX.`-Prefix erneut. Bei
            // anderen Fehlern (z.B. "unknown command UID MOVE") fallen
            // wir auf COPY zurück.
            let s = e.to_string();
            if looks_like_namespace_error(&s) {
                return Err(format!("UID MOVE {dst_folder}: {e}"));
            }
            tracing::warn!(error = %e, "UID MOVE failed, falling back to COPY+\\Deleted+EXPUNGE");
        }
    };

    // 2) Fallback: COPY + STORE \Deleted + EXPUNGE
    session
        .uid_copy(uid_str, dst_folder)
        .await
        .map_err(|e| format!("UID COPY {dst_folder}: {e}"))?;

    {
        let mut st = session
            .uid_store(uid_str, "+FLAGS.SILENT (\\Deleted)")
            .await
            .map_err(|e| format!("UID STORE +\\Deleted: {e}"))?;
        while let Some(r) = st.next().await {
            if let Err(e) = r {
                tracing::warn!("STORE stream error: {e}");
            }
        }
    }

    expunge_uid_or_all(session, uid_str).await;
    Ok(())
}

/// Erkennt das spezifische "Mailbox sollte mit INBOX. prefixed sein"-
/// Pattern, das namespace-prefixed IMAP-Server (Zoho.eu, manche
/// Dovecot-Setups) liefern wenn unprefixed Folder-Namen für
/// schreibende Operationen genutzt werden. SELECT funktioniert auf
/// solchen Servern oft auch ohne Prefix, COPY/MOVE dagegen nicht —
/// daher der spezielle Retry-Pfad in `move_message`.
///
/// Substring-Match auf "prefixed with: INBOX" ist robust genug für
/// Versions-Variationen der Server-Meldung.
fn looks_like_namespace_error(err: &str) -> bool {
    err.contains("prefixed with: INBOX")
        || err.contains("nonexistent namespace")
            && err.contains("INBOX")
}

/// Den korrigierten Folder-Namen in die `accounts`-Tabelle zurückschreiben,
/// damit die nächste Move/Delete/Spam-Operation auf demselben Konto
/// gleich den richtigen Namen nimmt. Wir vergleichen den fehlerhaften
/// `original` mit den Account-Folder-Feldern und matchen auf Gleichheit
/// — nur das eine Feld wird angefasst.
async fn persist_folder_name_correction(
    db: &DbHandle,
    account: &AccountSummary,
    original: &str,
    corrected: &str,
) -> Result<(), String> {
    use crate::domain::account::{Account, ImapEndpoint, SmtpEndpoint};
    use crate::domain::auth::AuthCredential;

    // Welches Account-Folder-Feld matcht den fehlerhaften Namen?
    // Wir schreiben einen Klon mit dem korrigierten Wert in genau dem
    // einen Feld zurück; alle anderen Felder bleiben wie sie sind.
    let mut account_for_update = Account {
        id: account.id,
        display_name: account.display_name.clone(),
        address: account.address.clone(),
        from_name: account.from_name.clone(),
        color: account.color.clone(),
        signature: account.signature.clone(),
        signature_html: account.signature_html.clone(),
        imap: ImapEndpoint {
            host: account.imap_host.clone(),
            port: account.imap_port,
            tls: account.imap_tls,
        },
        smtp: SmtpEndpoint {
            host: account.smtp_host.clone(),
            port: account.smtp_port,
            tls: account.smtp_tls,
        },
        credential: AuthCredential::Password {
            keyring_entry: format!("imap::{}", account.id.0),
        },
        archive_folder: account.archive_folder.clone(),
        sent_folder: account.sent_folder.clone(),
        drafts_folder: account.drafts_folder.clone(),
        trash_folder: account.trash_folder.clone(),
        spam_folder: account.spam_folder.clone(),
        archive_on_reply: account.archive_on_reply,
        prefetch_days: account.prefetch_days,
        sync_mode: account.sync_mode,
        server_stores_sent: account.server_stores_sent,
    };
    let mut updated_field: Option<&'static str> = None;
    if account.trash_folder == original {
        account_for_update.trash_folder = corrected.to_string();
        updated_field = Some("trash_folder");
    } else if account.spam_folder == original {
        account_for_update.spam_folder = corrected.to_string();
        updated_field = Some("spam_folder");
    } else if account.archive_folder == original {
        account_for_update.archive_folder = corrected.to_string();
        updated_field = Some("archive_folder");
    } else if account.sent_folder == original {
        account_for_update.sent_folder = corrected.to_string();
        updated_field = Some("sent_folder");
    } else if account.drafts_folder == original {
        account_for_update.drafts_folder = corrected.to_string();
        updated_field = Some("drafts_folder");
    }
    if updated_field.is_none() {
        // Move-Target war ein Custom-Folder (User hat in MoveToDialog
        // einen freihändig getippten Namen gegeben) — kein Account-Feld
        // zu ändern, der User trägt den Namen beim nächsten Mal halt
        // selbst korrigiert ein.
        tracing::info!(
            original = %original,
            corrected = %corrected,
            "namespace-Korrektur: Folder ist kein Account-Special, kein Persist nötig"
        );
        return Ok(());
    }

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpdateAccount {
            account: account_for_update,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("UpdateAccount: {e}"))?;
    tracing::info!(
        field = %updated_field.unwrap_or("?"),
        original = %original,
        corrected = %corrected,
        "namespace-Korrektur persistiert"
    );
    Ok(())
}
