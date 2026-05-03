// Flag mutations — bridges user actions ("mark as read") to the IMAP server
// and the local store. Both sides must agree: server state is authoritative
// for future devices, local state drives the UI instantly.
//
// Flow for each call:
//   1. Compute the new full Flags set from current + caller's FlagChanges.
//   2. If the set actually changed, send IMAP STORE +/-FLAGS for the
//      affected flags and refresh via UID FETCH FLAGS.
//   3. Persist via `WriteCmd::UpdateFlags`.

use tokio::sync::oneshot;

use crate::domain::message::{FlagChanges, Flags, MessageId};
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::imap_client;
use crate::infrastructure::queries;

const KEYRING_SERVICE: &str = "crystalmail";

pub async fn apply(
    db: &DbHandle,
    message_id: MessageId,
    changes: FlagChanges,
) -> Result<Flags, String> {
    // Load everything we need to reach the server.
    let (envelope, account, folder_name) = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let envelope = queries::get_envelope(&conn, &message_id)
            .map_err(|e| e.to_string())?
            .ok_or("envelope not found")?;
        let account = queries::get_account(&conn, &envelope.account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account for envelope no longer exists")?;
        let folder_name: String = conn
            .query_row(
                "SELECT name FROM folders WHERE id = ?1",
                rusqlite::params![envelope.folder_id.0.to_string()],
                |row| row.get(0),
            )
            .map_err(|e| format!("folder lookup: {e}"))?;
        (envelope, account, folder_name)
    };

    let current = Flags {
        seen: envelope.seen,
        answered: envelope.answered,
        flagged: envelope.flagged,
        forwarded: envelope.forwarded,
        junk: envelope.junk,
        draft: false,   // not tracked in detail
        deleted: false, // excluded from the reader
    };

    let desired = Flags {
        seen: changes.seen.unwrap_or(current.seen),
        answered: changes.answered.unwrap_or(current.answered),
        flagged: changes.flagged.unwrap_or(current.flagged),
        forwarded: changes.forwarded.unwrap_or(current.forwarded),
        junk: changes.junk.unwrap_or(current.junk),
        draft: current.draft,
        deleted: current.deleted,
    };

    // Skip the IMAP round-trip when nothing materially changed.
    let same = desired.seen == current.seen
        && desired.answered == current.answered
        && desired.flagged == current.flagged
        && desired.forwarded == current.forwarded
        && desired.junk == current.junk;

    if !same {
        imap_store(
            &account.imap_host,
            account.imap_port,
            &account.address,
            &entry_password(&account.id.0.to_string())?,
            &folder_name,
            envelope.imap_uid,
            &current,
            &desired,
        )
        .await?;
    }

    // DB write even if IMAP was skipped, so UI reconciles instantly.
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpdateFlags {
            message_id,
            flags: desired.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update flags: {e}"))?;

    Ok(desired)
}

fn entry_password(account_uuid: &str) -> Result<String, String> {
    let entry_name = format!("imap::{}", account_uuid);
    keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get_password: {e}"))
}

async fn imap_store(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    folder: &str,
    uid: u32,
    current: &Flags,
    desired: &Flags,
) -> Result<(), String> {
    let mut additions: Vec<&str> = Vec::new();
    let mut removals: Vec<&str> = Vec::new();

    macro_rules! diff {
        ($field:ident, $flag:expr) => {
            if desired.$field != current.$field {
                if desired.$field {
                    additions.push($flag);
                } else {
                    removals.push($flag);
                }
            }
        };
    }

    diff!(seen, "\\Seen");
    diff!(answered, "\\Answered");
    diff!(flagged, "\\Flagged");
    diff!(forwarded, "$Forwarded");
    diff!(junk, "$Junk");

    let client = imap_client::connect_tls(host, port).await?;
    let mut session = client
        .login(user, password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;
    session
        .select(folder)
        .await
        .map_err(|e| format!("SELECT {folder}: {e}"))?;

    if !additions.is_empty() {
        drain(
            session
                .uid_store(&uid.to_string(), format!("+FLAGS ({})", additions.join(" ")))
                .await
                .map_err(|e| format!("UID STORE +FLAGS: {e}"))?,
        )
        .await;
    }
    if !removals.is_empty() {
        drain(
            session
                .uid_store(&uid.to_string(), format!("-FLAGS ({})", removals.join(" ")))
                .await
                .map_err(|e| format!("UID STORE -FLAGS: {e}"))?,
        )
        .await;
    }

    let _ = session.logout().await;
    Ok(())
}

async fn drain<S>(mut stream: S)
where
    S: futures_util::Stream + Unpin,
{
    use futures_util::StreamExt;
    while stream.next().await.is_some() {}
}
