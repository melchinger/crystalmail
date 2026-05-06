// Mail-layer boundary API for the Calendar IMAP carriage profile (ADR-0011).
//
// This module is the **only** allowed Mail-layer entry point for the
// `timeprotocol` module's Phase-2 IMAP write/read path. Everything the
// calendar logic needs to do against the IMAP store goes through this
// thin facade — keeping the architectural boundary documented in
// `timeprotocol/mod.rs` intact even as Phase 2 lights up the sync paths.
//
// Style note: each operation opens its own short-lived IMAP session
// (login → op → logout), mirroring `application::smtp::append_to_folder`
// and `infrastructure::imap_client::create_mailbox`. A session-bag
// refactor can come later if v1's connection-per-call cost becomes a
// concrete pain — for typical manual-sync calendars (≤10 events to
// publish per click) it is well below provider throttle limits.

use futures_util::StreamExt;
use serde::Serialize;

use crate::domain::account::AccountId;
use crate::infrastructure::db::DbHandle;
use crate::infrastructure::imap_client;
use crate::infrastructure::queries;

const KEYRING_SERVICE: &str = "crystalmail";

type ImapSession = async_imap::Session<
    tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
>;

/// One stored message returned by `list_messages`. Carries the IMAP UID
/// for the LWW-tiebreak per ADR-0011 §5 ("…the higher IMAP message UID
/// as a final tiebreaker"), the raw RFC822 bytes for downstream parsing,
/// and the X-Cal-Format-Version header value if present so the caller
/// can reject unknown profile versions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarMessage {
    pub imap_uid: u32,
    pub rfc822: Vec<u8>,
    /// The `X-Cal-Format-Version` header value, lowercased. `None` when
    /// the header was absent (treat as baseline v1 per ADR-0011 §7).
    pub format_version: Option<String>,
}

/// Ensure the calendar folder exists. Idempotent: a CREATE that fails
/// because the mailbox already exists is not an error. We don't attempt
/// to detect which kind of failure happened by parsing server text —
/// instead we follow CREATE with a SELECT, which succeeds if and only
/// if the folder is now usable. The SELECT result is discarded; callers
/// don't keep the session.
pub async fn ensure_folder(
    db: &DbHandle,
    account_id: &AccountId,
    folder: &str,
) -> Result<(), String> {
    let (account, password) = load_account(db, account_id)?;
    let mut session = open_session(&account, &password).await?;

    // CREATE may fail for reasons other than "already exists" (perm
    // denied, illegal name, etc.). We ignore all of them and let the
    // SELECT below produce the authoritative success/failure signal.
    let _ = session.create(folder).await;

    let select_result = session
        .select(folder)
        .await
        .map_err(|e| format!("SELECT {folder}: {e}"));

    let _ = session.logout().await;
    select_result.map(|_| ())
}

/// Append one RFC822 message to the calendar folder. Used by the
/// publish path. The message is marked `\Seen` because the calendar
/// folder is implementation-internal storage — unread counts on it
/// would surface as ghost notifications in the user's mail client.
pub async fn append_message(
    db: &DbHandle,
    account_id: &AccountId,
    folder: &str,
    rfc822: &[u8],
) -> Result<(), String> {
    let (account, password) = load_account(db, account_id)?;
    let mut session = open_session(&account, &password).await?;

    let result = session
        .append(folder, Some("(\\Seen)"), None, rfc822)
        .await
        .map_err(|e| format!("APPEND {folder}: {e}"));

    let _ = session.logout().await;
    result
}

/// COPY the given IMAP UIDs from `src_folder` to `dest_folder`. Used
/// by the compaction pass — followed by a `delete_messages` to remove
/// them from the source. We don't use IMAP MOVE (RFC 6851) because it
/// is an extension; COPY+DELETE is a portable equivalent that works on
/// every IMAP server.
pub async fn copy_messages(
    db: &DbHandle,
    account_id: &AccountId,
    src_folder: &str,
    dest_folder: &str,
    imap_uids: &[u32],
) -> Result<(), String> {
    if imap_uids.is_empty() {
        return Ok(());
    }
    let (account, password) = load_account(db, account_id)?;
    let mut session = open_session(&account, &password).await?;

    let result = (async {
        session
            .select(src_folder)
            .await
            .map_err(|e| format!("SELECT {src_folder}: {e}"))?;
        let uid_set = imap_uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        session
            .uid_copy(&uid_set, dest_folder)
            .await
            .map_err(|e| format!("UID COPY {src_folder} → {dest_folder}: {e}"))
    })
    .await;

    let _ = session.logout().await;
    result
}

/// Mark the given IMAP UIDs as `\Deleted` and EXPUNGE them. Used by the
/// compaction pass after a successful COPY to the archive folder.
pub async fn delete_messages(
    db: &DbHandle,
    account_id: &AccountId,
    folder: &str,
    imap_uids: &[u32],
) -> Result<(), String> {
    if imap_uids.is_empty() {
        return Ok(());
    }
    let (account, password) = load_account(db, account_id)?;
    let mut session = open_session(&account, &password).await?;

    // Two-phase: STORE \Deleted then EXPUNGE. The streams returned by
    // both commands hold a mutable borrow on `session` until fully
    // drained, so each runs in its own scope before the next.
    let store_result = (async {
        session
            .select(folder)
            .await
            .map_err(|e| format!("SELECT {folder}: {e}"))?;
        let uid_set = imap_uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let stream = session
            .uid_store(&uid_set, "+FLAGS.SILENT (\\Deleted)")
            .await
            .map_err(|e| format!("UID STORE \\Deleted ({folder}): {e}"))?;
        tokio::pin!(stream);
        while stream.next().await.is_some() {}
        Ok::<_, String>(())
    })
    .await;
    if let Err(e) = store_result {
        let _ = session.logout().await;
        return Err(e);
    }

    let expunge_result = (async {
        let stream = session
            .expunge()
            .await
            .map_err(|e| format!("EXPUNGE ({folder}): {e}"))?;
        tokio::pin!(stream);
        while stream.next().await.is_some() {}
        Ok::<_, String>(())
    })
    .await;

    let _ = session.logout().await;
    expunge_result
}

/// Fetch every message in the calendar folder. For Phase 2 v1 we don't
/// optimize for incremental sync — full scan, in-memory LWW resolution
/// in the caller. Acceptable for typical calendars (<1000 active
/// messages); compaction (ADR-0011 §6) is the long-term answer when
/// folders grow large.
pub async fn list_messages(
    db: &DbHandle,
    account_id: &AccountId,
    folder: &str,
) -> Result<Vec<CalendarMessage>, String> {
    let (account, password) = load_account(db, account_id)?;
    let mut session = open_session(&account, &password).await?;

    let result = list_messages_in_session(&mut session, folder).await;

    let _ = session.logout().await;
    result
}

async fn list_messages_in_session(
    session: &mut ImapSession,
    folder: &str,
) -> Result<Vec<CalendarMessage>, String> {
    session
        .select(folder)
        .await
        .map_err(|e| format!("SELECT {folder}: {e}"))?;

    let uids: Vec<u32> = {
        let set = session
            .uid_search("ALL")
            .await
            .map_err(|e| format!("UID SEARCH ({folder}): {e}"))?;
        let mut v: Vec<u32> = set.into_iter().collect();
        v.sort_unstable();
        v
    };
    if uids.is_empty() {
        return Ok(Vec::new());
    }

    let uid_set = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");

    // BODY.PEEK[] gives us the full RFC822 without setting \Seen — we
    // already mark messages \Seen at append time, but PEEK keeps reads
    // strictly side-effect-free regardless.
    let mut stream = session
        .uid_fetch(&uid_set, "(UID BODY.PEEK[])")
        .await
        .map_err(|e| format!("UID FETCH ({folder}): {e}"))?;

    let mut out: Vec<CalendarMessage> = Vec::new();
    while let Some(result) = stream.next().await {
        let fetch = match result {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("calendar list FETCH parse error: {e}");
                continue;
            }
        };
        let imap_uid = match fetch.uid {
            Some(u) => u,
            None => continue,
        };
        let body = match fetch.body() {
            Some(b) => b.to_vec(),
            None => continue,
        };
        let format_version = parse_format_version_header(&body);
        out.push(CalendarMessage {
            imap_uid,
            rfc822: body,
            format_version,
        });
    }

    Ok(out)
}

/// Lift the `X-Cal-Format-Version` header value out of the raw RFC822
/// header block. We don't pull in the full mail-parser here because the
/// header is single-token and we only need the value for one decision
/// (treat-as-baseline-v1 vs reject-as-unknown-version per ADR-0011 §7).
fn parse_format_version_header(rfc822: &[u8]) -> Option<String> {
    let needle = b"x-cal-format-version:";
    // Find the header block (everything before the first CRLFCRLF or
    // LFLF). Doing a bounded scan keeps malformed mails from being
    // parsed end-to-end.
    let header_end = rfc822
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| rfc822.windows(2).position(|w| w == b"\n\n"))
        .unwrap_or(rfc822.len());
    let header_block = &rfc822[..header_end];
    let lower: Vec<u8> = header_block.iter().map(|b| b.to_ascii_lowercase()).collect();
    let pos = lower.windows(needle.len()).position(|w| w == needle)?;
    let line_end = lower[pos..]
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|p| pos + p)
        .or_else(|| lower[pos..].iter().position(|&b| b == b'\n').map(|p| pos + p))
        .unwrap_or(lower.len());
    let value_start = pos + needle.len();
    let value_bytes = &lower[value_start..line_end];
    let value: String = std::str::from_utf8(value_bytes).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn load_account(
    db: &DbHandle,
    account_id: &AccountId,
) -> Result<(queries::AccountSummary, String), String> {
    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?
    };
    let entry_name = format!("imap::{}", account.id.0);
    let password = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get_password: {e}"))?;
    Ok((account, password))
}

async fn open_session(
    account: &queries::AccountSummary,
    password: &str,
) -> Result<ImapSession, String> {
    let client = imap_client::connect_tls(&account.imap_host, account.imap_port)
        .await
        .map_err(|e| format!("IMAP connect {}: {e}", account.imap_host))?;
    let session = client
        .login(&account.address, password)
        .await
        .map_err(|(e, _)| format!("IMAP LOGIN: {e}"))?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_format_version_header_when_present() {
        let raw = b"Subject: Test\r\n\
                   X-Cal-Format-Version: 1\r\n\
                   Content-Type: text/calendar\r\n\
                   \r\n\
                   BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n";
        assert_eq!(parse_format_version_header(raw).as_deref(), Some("1"));
    }

    #[test]
    fn parses_format_version_header_case_insensitive() {
        let raw = b"Subject: Test\r\n\
                   x-cal-FORMAT-version:   2\r\n\
                   \r\n";
        assert_eq!(parse_format_version_header(raw).as_deref(), Some("2"));
    }

    #[test]
    fn returns_none_when_header_absent() {
        let raw = b"Subject: Test\r\nFrom: a@b\r\n\r\nbody\r\n";
        assert!(parse_format_version_header(raw).is_none());
    }

    #[test]
    fn returns_none_for_empty_value() {
        let raw = b"X-Cal-Format-Version:   \r\n\r\n";
        assert!(parse_format_version_header(raw).is_none());
    }

    #[test]
    fn does_not_match_in_body_only_in_header_block() {
        // Header doesn't contain the field; body does. Must NOT match.
        let raw = b"Subject: Test\r\n\
                   \r\n\
                   The string X-Cal-Format-Version: 99 appears in body only.\r\n";
        assert!(parse_format_version_header(raw).is_none());
    }
}
