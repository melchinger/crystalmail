// Thin IMAP helpers. For MVP we support implicit TLS (port 993 typical).
// STARTTLS on 143 lands later — requires the wrap-after-STARTTLS dance.
//
// TLS is provided by tokio-rustls + webpki-roots so everything stays in the
// tokio-io trait universe that `async-imap` expects.

use std::sync::Arc;

use std::time::Instant;

use async_imap::imap_proto::types::NameAttribute;
use async_imap::{Client, Session};
use futures_util::StreamExt;
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use serde::Serialize;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

/// Convenience alias used by older signatures and reserved for any
/// future code that wants to talk about a session without spelling out
/// the full TLS-stream path. Currently no in-tree caller uses it; we
/// keep the alias so adding back a typed handle (e.g. for the IDLE
/// actor's session lifecycle) doesn't require touching every callsite
/// at once.
#[allow(dead_code)]
pub type ImapSession = Session<TlsStream<TcpStream>>;

fn tls_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

pub async fn connect_tls(host: &str, port: u16) -> Result<Client<TlsStream<TcpStream>>, String> {
    let tcp = TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("TCP connect {host}:{port}: {e}"))?;

    let connector = TlsConnector::from(tls_config());
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|e| format!("invalid TLS SNI host: {e}"))?;
    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("TLS handshake: {e}"))?;

    Ok(Client::new(tls_stream))
}

/// Full connect → login → SELECT INBOX → LOGOUT round trip. Returns `Ok(())`
/// if the credentials and server are usable. The inbox SELECT is included
/// because some servers accept LOGIN but reject subsequent commands for
/// misconfigured accounts.
pub async fn test_login(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> Result<(), String> {
    let client = connect_tls(host, port).await?;
    let mut session = client
        .login(user, password)
        .await
        .map_err(|(e, _client)| format!("LOGIN: {e}"))?;
    session
        .select("INBOX")
        .await
        .map_err(|e| format!("SELECT INBOX: {e}"))?;
    session
        .logout()
        .await
        .map_err(|e| format!("LOGOUT: {e}"))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerboseStep {
    pub elapsed_ms: u128,
    pub kind: String, // "info" | "ok" | "err"
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerboseReport {
    pub ok: bool,
    pub total_ms: u128,
    pub steps: Vec<VerboseStep>,
}

struct Logger {
    start: Instant,
    steps: Vec<VerboseStep>,
}

impl Logger {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            steps: Vec::new(),
        }
    }
    fn push(&mut self, kind: &str, message: impl Into<String>) {
        self.steps.push(VerboseStep {
            elapsed_ms: self.start.elapsed().as_millis(),
            kind: kind.into(),
            message: message.into(),
        });
    }
    fn finish(self, ok: bool) -> VerboseReport {
        let total_ms = self.start.elapsed().as_millis();
        VerboseReport {
            ok,
            total_ms,
            steps: self.steps,
        }
    }
}

/// Same workflow as `test_login`, but records every step with timing so the
/// UI can show exactly where a failure happened and how slow each stage is.
/// Always returns `Ok(VerboseReport)`; the report's `ok` field indicates
/// whether the login ultimately succeeded.
pub async fn test_login_verbose(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> VerboseReport {
    let mut log = Logger::new();

    log.push("info", format!("TCP-Verbindung zu {host}:{port}"));
    let tcp = match TcpStream::connect((host, port)).await {
        Ok(s) => {
            log.push("ok", "TCP verbunden");
            s
        }
        Err(e) => {
            log.push("err", format!("TCP connect fehlgeschlagen: {e}"));
            return log.finish(false);
        }
    };

    log.push("info", format!("TLS-Handshake (SNI={host})"));
    let connector = TlsConnector::from(tls_config());
    let server_name = match ServerName::try_from(host.to_owned()) {
        Ok(n) => n,
        Err(e) => {
            log.push("err", format!("Ungültiger SNI-Host: {e}"));
            return log.finish(false);
        }
    };
    let tls_stream = match connector.connect(server_name, tcp).await {
        Ok(s) => {
            log.push("ok", "TLS etabliert");
            s
        }
        Err(e) => {
            log.push("err", format!("TLS-Handshake fehlgeschlagen: {e}"));
            return log.finish(false);
        }
    };

    let client = Client::new(tls_stream);
    log.push("info", "IMAP-Greeting empfangen");

    log.push("info", format!("LOGIN als {user}"));
    let mut session = match client.login(user, password).await {
        Ok(s) => {
            log.push("ok", "Authentifizierung erfolgreich");
            s
        }
        Err((e, _client)) => {
            log.push("err", format!("LOGIN abgelehnt: {e}"));
            return log.finish(false);
        }
    };

    // CAPABILITY after LOGIN — shows post-auth capabilities (IDLE, MOVE, etc.)
    log.push("info", "CAPABILITY");
    match session.capabilities().await {
        Ok(caps) => {
            let joined = caps
                .iter()
                .map(|c| format!("{c:?}"))
                .collect::<Vec<_>>()
                .join(" ");
            log.push("ok", format!("Server unterstützt: {joined}"));
        }
        Err(e) => {
            log.push("err", format!("CAPABILITY fehlgeschlagen: {e}"));
        }
    };

    log.push("info", "SELECT INBOX");
    match session.select("INBOX").await {
        Ok(mb) => {
            let uid_next = mb.uid_next.map(|u| u.to_string()).unwrap_or_else(|| "?".into());
            let uid_validity = mb
                .uid_validity
                .map(|u| u.to_string())
                .unwrap_or_else(|| "?".into());
            log.push(
                "ok",
                format!(
                    "INBOX: {} Nachrichten, UIDNEXT={}, UIDVALIDITY={}",
                    mb.exists, uid_next, uid_validity
                ),
            );
        }
        Err(e) => {
            log.push("err", format!("SELECT INBOX fehlgeschlagen: {e}"));
            // Best-effort logout before returning.
            let _ = session.logout().await;
            return log.finish(false);
        }
    };

    log.push("info", "LOGOUT");
    match session.logout().await {
        Ok(_) => log.push("ok", "Verbindung sauber geschlossen"),
        Err(e) => log.push("err", format!("LOGOUT fehlgeschlagen: {e}")),
    };

    log.finish(true)
}

/// Result of an auto-discovery LIST scan. All four folder paths are
/// best-effort: server-advertised SPECIAL-USE flags first (RFC 6154), then
/// a heuristic fallback over common English/German/Gmail names. `None` on
/// a slot means "leave the user-configured value alone".
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredFolders {
    pub archive: Option<String>,
    pub sent: Option<String>,
    pub drafts: Option<String>,
    pub trash: Option<String>,
    pub spam: Option<String>,
    /// Every folder the server returned — exposed so the UI can show a
    /// picker in case the heuristic guesses wrong.
    pub all: Vec<String>,
}

/// Create a mailbox (IMAP CREATE). `name` is the raw IMAP path
/// exactly as the server expects it — `INBOX.Steuer`, `Projects/2024`,
/// etc. Fails with the server's own error when the name collides,
/// contains invalid chars, or the account lacks permissions.
pub async fn create_mailbox(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    name: &str,
) -> Result<(), String> {
    let client = connect_tls(host, port).await?;
    let mut session = client
        .login(user, password)
        .await
        .map_err(|(e, _c)| format!("LOGIN: {e}"))?;
    session
        .create(name)
        .await
        .map_err(|e| format!("CREATE: {e}"))?;
    let _ = session.logout().await;
    Ok(())
}

/// Delete a mailbox (IMAP DELETE). Servers usually refuse to delete
/// folders that still contain child folders — surface that error
/// verbatim so the UI can explain. Does NOT touch local DB state;
/// the caller handles that after a successful server-side delete.
pub async fn delete_mailbox(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    name: &str,
) -> Result<(), String> {
    let client = connect_tls(host, port).await?;
    let mut session = client
        .login(user, password)
        .await
        .map_err(|(e, _c)| format!("LOGIN: {e}"))?;
    session
        .delete(name)
        .await
        .map_err(|e| format!("DELETE: {e}"))?;
    let _ = session.logout().await;
    Ok(())
}

pub async fn discover_folders(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> Result<DiscoveredFolders, String> {
    let client = connect_tls(host, port).await?;
    let mut session = client
        .login(user, password)
        .await
        .map_err(|(e, _c)| format!("LOGIN: {e}"))?;

    // Scan the entire hierarchy — some providers nest mail folders under
    // `INBOX.` (Dovecot default) or `[Gmail]/` (Gmail IMAP), so a top-level
    // `%` wildcard would miss them.
    let names = {
        let mut stream = session
            .list(Some(""), Some("*"))
            .await
            .map_err(|e| format!("LIST: {e}"))?;
        let mut out: Vec<(String, Vec<NameAttribute<'static>>)> = Vec::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(name) => {
                    // Skip \NoSelect entries — they are containers, not
                    // selectable mailboxes, so SELECT would fail later.
                    let attrs_owned: Vec<NameAttribute<'static>> = name
                        .attributes()
                        .iter()
                        .map(owned_attr)
                        .collect();
                    if attrs_owned.iter().any(|a| matches!(a, NameAttribute::NoSelect)) {
                        continue;
                    }
                    out.push((name.name().to_string(), attrs_owned));
                }
                Err(e) => {
                    tracing::warn!("LIST parse error: {e}");
                }
            }
        }
        out
    };

    let _ = session.logout().await;

    let all_names: Vec<String> = names.iter().map(|(n, _)| n.clone()).collect();

    // Pass 1: server-advertised SPECIAL-USE. Most modern servers (Dovecot,
    // Cyrus, Gmail, Outlook.com) set these; when present they are
    // authoritative.
    let mut archive = pick_by_attr(&names, |a| matches!(a, NameAttribute::Archive));
    let mut sent = pick_by_attr(&names, |a| matches!(a, NameAttribute::Sent));
    let mut drafts = pick_by_attr(&names, |a| matches!(a, NameAttribute::Drafts));
    let mut trash = pick_by_attr(&names, |a| matches!(a, NameAttribute::Trash));
    let mut spam = pick_by_attr(&names, |a| matches!(a, NameAttribute::Junk));

    // Pass 2 (fallback): common name heuristics. Case-insensitive; first hit
    // wins so we prefer more specific paths (e.g. `[Gmail]/Gesendet` over a
    // generic `Sent` that the server might also expose).
    if archive.is_none() {
        archive = pick_by_name(
            &all_names,
            &[
                "Archive",
                "Archiv",
                "INBOX.Archive",
                "INBOX.Archiv",
                "[Gmail]/All Mail",
                "[Gmail]/Alle Nachrichten",
                "All Mail",
            ],
        );
    }
    if sent.is_none() {
        sent = pick_by_name(
            &all_names,
            &[
                "Sent",
                "Sent Items",
                "Sent Messages",
                "Gesendet",
                "Gesendete Objekte",
                "Gesendete Elemente",
                "INBOX.Sent",
                "INBOX.Gesendet",
                "[Gmail]/Sent Mail",
                "[Gmail]/Gesendet",
            ],
        );
    }
    if drafts.is_none() {
        drafts = pick_by_name(
            &all_names,
            &[
                "Drafts",
                "Draft",
                "Entwürfe",
                "Entwurf",
                "INBOX.Drafts",
                "INBOX.Entwürfe",
                "[Gmail]/Drafts",
                "[Gmail]/Entwürfe",
            ],
        );
    }
    if trash.is_none() {
        trash = pick_by_name(
            &all_names,
            &[
                "Trash",
                "Deleted Items",
                "Deleted Messages",
                "Papierkorb",
                "Gelöschte Objekte",
                "Gelöschte Elemente",
                "INBOX.Trash",
                "INBOX.Papierkorb",
                "[Gmail]/Trash",
                "[Gmail]/Papierkorb",
            ],
        );
    }
    if spam.is_none() {
        spam = pick_by_name(
            &all_names,
            &[
                "Spam",
                "Junk",
                "Junk E-mail",
                "Junk Email",
                "INBOX.Spam",
                "INBOX.Junk",
                "[Gmail]/Spam",
                "[Gmail]/Junk",
            ],
        );
    }

    Ok(DiscoveredFolders {
        archive,
        sent,
        drafts,
        trash,
        spam,
        all: all_names,
    })
}

fn pick_by_attr(
    names: &[(String, Vec<NameAttribute<'static>>)],
    predicate: impl Fn(&NameAttribute<'_>) -> bool,
) -> Option<String> {
    names
        .iter()
        .find(|(_, attrs)| attrs.iter().any(&predicate))
        .map(|(n, _)| n.clone())
}

fn pick_by_name(all_names: &[String], candidates: &[&str]) -> Option<String> {
    for cand in candidates {
        if let Some(n) = all_names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(cand))
        {
            return Some(n.clone());
        }
    }
    None
}

/// `NameAttribute<'a>` borrows from the LIST response. We want to hold onto
/// the tag set past the stream's lifetime, so we map `Custom(&str)` into
/// `Custom(Cow::Owned)` and trust the others are 'static.
fn owned_attr(a: &NameAttribute<'_>) -> NameAttribute<'static> {
    use std::borrow::Cow;
    match a {
        NameAttribute::NoInferiors => NameAttribute::NoInferiors,
        NameAttribute::NoSelect => NameAttribute::NoSelect,
        NameAttribute::Marked => NameAttribute::Marked,
        NameAttribute::Unmarked => NameAttribute::Unmarked,
        NameAttribute::All => NameAttribute::All,
        NameAttribute::Archive => NameAttribute::Archive,
        NameAttribute::Drafts => NameAttribute::Drafts,
        NameAttribute::Flagged => NameAttribute::Flagged,
        NameAttribute::Junk => NameAttribute::Junk,
        NameAttribute::Sent => NameAttribute::Sent,
        NameAttribute::Trash => NameAttribute::Trash,
        NameAttribute::Extension(s) => {
            NameAttribute::Extension(Cow::Owned(s.to_string()))
        }
        // Any variants imap-proto may add in future versions fall through
        // as an opaque Extension — we only care about the SPECIAL-USE
        // subset here, so losing the exact tag is fine.
        other => NameAttribute::Extension(Cow::Owned(format!("{other:?}"))),
    }
}
