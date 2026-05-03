// IMAP folder CRUD: create and delete on the server, then mirror
// into the local store so the sidebar + folder-sync settings
// reflect the new state without waiting for a full re-sync.
//
// Both commands are thin Tauri wrappers around an IMAP op + a DB
// write. We don't try to auto-pick a parent or sanitise names — the
// server validates (some reject `/` as separator, others require
// `INBOX.` prefix, Gmail uses `[Gmail]/…`), and guessing wrong
// would hide its error message from the user. Pass-through wins.

use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use crate::domain::account::AccountId;
use crate::domain::folder::FolderId;
use crate::infrastructure::db::WriteCmd;
use crate::infrastructure::{imap_client, queries};
use crate::state::AppState;

const KEYRING_SERVICE: &str = "crystalmail";

/// Create an IMAP mailbox on the given account and register it
/// locally via `EnsureFolder`. Returns the new folder id so the
/// caller can immediately SELECT it if they want.
#[tauri::command]
pub async fn create_folder(
    app: AppHandle,
    account_id: AccountId,
    name: String,
) -> Result<FolderId, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Ordnername darf nicht leer sein.".into());
    }
    // INBOX is universal — refusing re-creation here saves the user
    // the server's usually-cryptic "mailbox already exists" reply.
    if name.eq_ignore_ascii_case("INBOX") {
        return Err("Der INBOX-Ordner existiert immer schon.".into());
    }

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Load account + password from keyring.
    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, &account_id)
            .map_err(|e| e.to_string())?
            .ok_or("Konto nicht gefunden.")?
    };
    let entry_name = format!("imap::{}", account.id.0);
    let password = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get: {e}"))?;

    // Server-side CREATE. Errors bubble up verbatim so the UI can
    // quote the server's wording ("Mailbox already exists" etc).
    imap_client::create_mailbox(
        &account.imap_host,
        account.imap_port,
        &account.address,
        &password,
        &name,
    )
    .await?;

    // Register locally so the sidebar and folder-sync settings pick
    // it up right away. Sync is enabled by default (the migration
    // default for the column); the user can opt out afterwards.
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::EnsureFolder {
            account_id,
            name,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db ensure folder: {e}"))
}

/// Delete an IMAP mailbox. Refuses to touch the common specials by
/// name — dropping INBOX would be a support-ticket-magnet, and
/// ripping out Archive/Sent/Drafts/Trash/Spam without the account's
/// special-folder settings being rewritten first would orphan
/// reply-send and archive flows.
#[tauri::command]
pub async fn delete_folder(
    app: AppHandle,
    account_id: AccountId,
    name: String,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Kein Ordnername angegeben.".into());
    }

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let (account, folder) = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let account = queries::get_account(&conn, &account_id)
            .map_err(|e| e.to_string())?
            .ok_or("Konto nicht gefunden.")?;
        let folders =
            queries::list_account_folders(&conn, &account_id)
                .map_err(|e| e.to_string())?;
        let folder = folders
            .into_iter()
            .find(|f| f.name == name)
            .ok_or_else(|| {
                format!("Ordner \"{name}\" nicht in der lokalen Liste gefunden.")
            })?;
        (account, folder)
    };

    // Guardrails: block deleting INBOX and the configured special
    // folders. The user can change the account's special-folder
    // settings first if they really want to decommission one.
    if name.eq_ignore_ascii_case("INBOX") {
        return Err("INBOX kann nicht gelöscht werden.".into());
    }
    let specials = [
        ("Archiv", account.archive_folder.as_str()),
        ("Gesendet", account.sent_folder.as_str()),
        ("Entwürfe", account.drafts_folder.as_str()),
        ("Papierkorb", account.trash_folder.as_str()),
        ("Spam", account.spam_folder.as_str()),
    ];
    for (label, special) in specials {
        if !special.is_empty() && name == special {
            return Err(format!(
                "Ordner \"{name}\" ist als {label} konfiguriert. Zuerst in den Kontoeinstellungen umbiegen, dann löschen."
            ));
        }
    }

    let entry_name = format!("imap::{}", account.id.0);
    let password = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get: {e}"))?;

    imap_client::delete_mailbox(
        &account.imap_host,
        account.imap_port,
        &account.address,
        &password,
        &name,
    )
    .await?;

    // Local cleanup: drop the folder row (CASCADE takes care of
    // envelopes + bodies; the writer also nukes stale FTS rows).
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteFolderTree {
            folder_id: folder.id,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete folder: {e}"))
}
