// Tauri command adapters for the mail feature. These stay thin on purpose:
// translate parameters, dispatch to `infrastructure`, map errors to strings
// the frontend can display.

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use serde::Serialize;

use crate::application::attachments::{self, AttachmentMeta};
use crate::application::body;
use crate::application::flags as flags_app;
use crate::application::mark_read;
use crate::application::message_ops;
use crate::application::prefetch;
use crate::application::smtp::{self, SendMailRequest};
use crate::application::sync::{self, SyncReport};
use crate::domain::account::AccountId;
use crate::domain::message::{FlagChanges, Flags, MessageId};
use crate::domain::folder::FolderId;
use crate::infrastructure::queries::{
    self, EnvelopeDetail, EnvelopeSummary, FolderSummary, SearchFilters,
    UnifiedUnreadCount,
};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDetail {
    pub envelope: EnvelopeDetail,
    pub plain_text: Option<String>,
    pub html_text: Option<String>,
    pub attachments: Vec<AttachmentMeta>,
}

/// Unified list across accounts for a specific canonical folder. Values of
/// `folder`: "inbox" | "archive" | "sent" | "drafts" | "trash".
/// Optional `account_id` narrows to a single account; omit for the full
/// unified view.
#[tauri::command]
pub async fn list_unified_folder(
    app: AppHandle,
    folder: String,
    account_id: Option<AccountId>,
    limit: u32,
    offset: u32,
) -> Result<Vec<EnvelopeSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_unified_folder(&conn, &folder, account_id.as_ref(), limit, offset)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_mail(
    app: AppHandle,
    query: String,
    limit: u32,
) -> Result<Vec<EnvelopeSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::search_envelopes(&conn, &query, limit).map_err(|e| e.to_string())
}

/// Folder-scoped full-text search. The raw user query is handed to FTS5,
/// so selector syntax like `subject:foo from_text:bar` + phrases + boolean
/// operators all work out of the box.
#[tauri::command]
pub async fn search_in_folder(
    app: AppHandle,
    folder: String,
    account_id: Option<AccountId>,
    query: String,
    limit: u32,
) -> Result<Vec<EnvelopeSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::search_in_folder(&conn, &folder, account_id.as_ref(), &query, limit)
        .map_err(|e| e.to_string())
}

/// Structured search across all four orthogonal axes (FTS, folder,
/// account, structured filters). Drives the DSL on the frontend —
/// `utils/searchDsl.ts` parses the user's raw query into this shape
/// and we serve it back as a single `EnvelopeSummary[]`.
///
/// `fts` empty ⇒ no FTS join, pure filter / folder lookup ordered by
/// date. `folder` None ⇒ across all folders (still excluding deleted).
/// `account_id` None ⇒ across all accounts. `filters` empty ⇒ no
/// structured constraints (unread/flagged/has-attachments/date).
#[tauri::command]
pub async fn search_advanced(
    app: AppHandle,
    fts: String,
    folder: Option<String>,
    folder_id: Option<FolderId>,
    account_id: Option<AccountId>,
    filters: SearchFilters,
    limit: u32,
) -> Result<Vec<EnvelopeSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::search_advanced(
        &conn,
        &fts,
        folder.as_deref(),
        folder_id.as_ref(),
        account_id.as_ref(),
        &filters,
        limit,
    )
    .map_err(|e| e.to_string())
}

/// IMAP folder inventory for one account. Drives the sidebar's per-account
/// expander so the user can jump into any mailbox — not just the canonical
/// Inbox/Archive/Sent/Drafts/Trash.
#[tauri::command]
pub async fn list_account_folders(
    app: AppHandle,
    account_id: AccountId,
) -> Result<Vec<FolderSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_account_folders(&conn, &account_id).map_err(|e| e.to_string())
}

/// Envelope list for a single folder, identified by its DB id (not the IMAP
/// path). Used by the sidebar sub-folder navigation. The corresponding
/// search command is `search_in_folder` with a canonical key — for now,
/// search on ad-hoc folders just uses the local per-folder listing without
/// FTS narrowing. (Easy enough to extend later.)
#[tauri::command]
pub async fn list_folder_envelopes(
    app: AppHandle,
    folder_id: FolderId,
    limit: u32,
    offset: u32,
) -> Result<Vec<EnvelopeSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_envelopes_in_folder(&conn, &folder_id, limit, offset)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sync_account(
    app: AppHandle,
    account_id: AccountId,
    priority_folder: Option<String>,
) -> Result<SyncReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Priority path: sync the folder the user is currently looking at
    // first, return fast so the UI can show fresh envelopes, and spawn
    // a background task that walks the remaining specials + prefetch.
    // Without priority we fall back to the historical flat sync —
    // used by timed background syncs and unified views where "what's
    // on screen" isn't one server folder.
    if let Some(name) = priority_folder {
        let priority_report =
            sync::sync_single_folder(&app, db, account_id, &name).await?;

        let app2 = app.clone();
        let skip = vec![name];
        tokio::spawn(async move {
            let state = app2.state::<AppState>();
            let Some(db) = state.db.get() else {
                tracing::warn!(
                    "background sync skipped — db gone before task started"
                );
                return;
            };
            if let Err(e) = sync::sync_inbox(&app2, db, account_id, &skip).await {
                tracing::warn!(
                    account = %account_id.0,
                    error = %e,
                    "background sync of remaining folders failed"
                );
            }
            prefetch::spawn(app2, account_id);
        });

        return Ok(priority_report);
    }

    let report = sync::sync_inbox(&app, db, account_id, &[]).await?;
    // Opportunistic body prefetch for whatever landed in the sync window.
    // Fire-and-forget — we've already reported the sync success.
    prefetch::spawn(app.clone(), account_id);
    Ok(report)
}

/// Lazy on-open sync: pull the N newest envelopes in a folder when
/// the user navigates to it. Default `limit=50` fills the visible
/// list without over-fetching. TTL-gated in the backend — repeat
/// calls within 5 minutes are no-ops.
#[tauri::command]
pub async fn sync_folder_recent(
    app: AppHandle,
    folder_id: FolderId,
    limit: Option<u32>,
) -> Result<SyncReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    sync::sync_folder_recent(&app, db, folder_id, limit.unwrap_or(50)).await
}

/// Per-folder sync opt-out. Flipping the flag takes effect on the
/// next sync pass — the in-flight one (if any) won't notice.
#[tauri::command]
pub async fn set_folder_sync_enabled(
    app: AppHandle,
    folder_id: FolderId,
    enabled: bool,
) -> Result<(), String> {
    use crate::infrastructure::db::WriteCmd;
    use tokio::sync::oneshot;

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::SetFolderSyncEnabled {
            folder_id,
            enabled,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update: {e}"))
}

/// Lazy pager: pull the next `limit` (default 10) envelopes older
/// than whatever is currently cached locally. Called when the user
/// scrolls past the bottom of the currently-loaded list. No TTL —
/// this is always an explicit "give me more" gesture from the user.
/// Pivot UID is resolved server-side from the DB so the frontend
/// never has to track it.
#[tauri::command]
pub async fn sync_folder_older(
    app: AppHandle,
    folder_id: FolderId,
    limit: Option<u32>,
) -> Result<SyncReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    sync::sync_folder_older(&app, db, folder_id, limit.unwrap_or(10)).await
}

/// Canonical-view pager: pull older envelopes for every account that
/// participates in the unified bucket (`folder` ∈
/// "inbox"|"archive"|"sent"|"drafts"|"trash"|"spam"). Drives the
/// scroll-to-bottom in the unified inbox / unified archive views,
/// where there's no single per-folder pivot to feed `sync_folder_older`.
/// `accountId` narrows to a single account (matches the sidebar filter).
#[tauri::command]
pub async fn sync_unified_folder_older(
    app: AppHandle,
    folder: String,
    account_id: Option<AccountId>,
    limit: Option<u32>,
) -> Result<SyncReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    sync::sync_unified_folder_older(&app, db, &folder, account_id, limit.unwrap_or(20)).await
}

/// Manual prefetch trigger. The frontend calls this on account list load
/// so cold-start app launches warm the cache without waiting for the next
/// sync. No-op when the account has `prefetch_days = 0`.
#[tauri::command]
pub async fn prefetch_account_bodies(
    app: AppHandle,
    account_id: AccountId,
) -> Result<(), String> {
    prefetch::spawn(app, account_id);
    Ok(())
}

/// Batch-mark all given messages as `\Seen`. Groups by
/// (account, folder) so one IMAP session handles each group; local DB
/// flags are updated via the writer actor. Returns a report so the
/// UI can show something like "117 von 120 Mails als gelesen markiert".
#[tauri::command]
pub async fn mark_messages_read(
    app: AppHandle,
    message_ids: Vec<MessageId>,
) -> Result<mark_read::MarkReadReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    mark_read::mark_messages_read(db, message_ids).await
}

/// Snapshot of unread-counts for all six canonical unified folders —
/// feeds the sidebar badges and the window-title count.
#[tauri::command]
pub async fn unified_unread_counts(
    app: AppHandle,
) -> Result<Vec<UnifiedUnreadCount>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_unified_unread_counts(&conn).map_err(|e| e.to_string())
}

/// Abort an in-flight `open_message` body fetch. Called by the frontend
/// right before archive/delete/move: no point spending IMAP bandwidth on
/// a body that's about to be moved or deleted, and dropping the session
/// frees the connection slot so the follow-up op reaches the server
/// without queuing on servers that serialize per-account sessions.
///
/// No-op if no fetch for that id is registered (already finished, never
/// started, or cancelled before).
#[tauri::command]
pub async fn cancel_pending_fetch(
    app: AppHandle,
    message_id: MessageId,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let lock_result = state.pending_fetch_cancels.lock();
    if let Ok(mut map) = lock_result {
        if let Some(tx) = map.remove(&message_id) {
            // Best-effort — receiver may have already dropped if the
            // fetch just finished, in which case send() returns Err.
            let _ = tx.send(());
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn set_message_flags(
    app: AppHandle,
    message_id: MessageId,
    changes: FlagChanges,
) -> Result<Flags, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    flags_app::apply(db, message_id, changes).await
}

#[tauri::command]
pub async fn archive_message(
    app: AppHandle,
    message_id: MessageId,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    message_ops::archive(db, message_id).await
}

#[tauri::command]
pub async fn delete_message(
    app: AppHandle,
    message_id: MessageId,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    message_ops::delete(db, message_id).await
}

/// Move a message to an arbitrary IMAP folder on the same account. Drives
/// the Move-to-Folder popup (hotkey `v`). The folder name must match an
/// entry from `list_account_folders` — validation happens server-side so
/// a stale UI can't silently create a new folder.
#[tauri::command]
pub async fn move_message_to(
    app: AppHandle,
    message_id: MessageId,
    folder: String,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    message_ops::move_to(db, message_id, folder).await
}

/// Mark a message as spam. Does two things atomically from the user's
/// point of view:
///   1. Sets the `$Junk` IMAP keyword (RFC 5788) so other clients and
///      server-side filters see the curation.
///   2. Moves the message into the account's configured spam folder.
///
/// Failure of either step is reported — the command doesn't try to
/// partially succeed. Called by the "!" hotkey and the Reader toolbar.
#[tauri::command]
pub async fn mark_as_spam(
    app: AppHandle,
    message_id: MessageId,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Step 1: $Junk flag. Go through the existing apply() path so the
    // local DB, the IMAP server, and the UI merger stay consistent.
    flags_app::apply(
        db,
        message_id,
        crate::domain::message::FlagChanges {
            junk: Some(true),
            ..Default::default()
        },
    )
    .await?;

    // Step 2: resolve account + spam folder, then move. Two early-returns
    // where the move step is not applicable:
    //   - account has no spam folder configured → flag-only behavior
    //   - message already lives in the spam folder → flag was the whole
    //     action, no move needed. This is the "!" pressed on a mail
    //     already in the Spam view case — the frontend short-circuits
    //     this too, but we guard here so the backend is consistent on
    //     its own.
    let (account, envelope_folder) = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let envelope = queries::get_envelope(&conn, &message_id)
            .map_err(|e| e.to_string())?
            .ok_or("envelope not found")?;
        let account = queries::get_account(&conn, &envelope.account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account for envelope no longer exists")?;
        (account, envelope.folder_name)
    };
    if account.spam_folder.trim().is_empty() {
        return Ok(());
    }
    if envelope_folder == account.spam_folder {
        return Ok(());
    }
    message_ops::move_to(db, message_id, account.spam_folder).await
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMailAndMarkRequest {
    #[serde(flatten)]
    pub send: SendMailRequest,
    /// When set, this message is marked `\Answered` after the SMTP send succeeds.
    #[serde(default)]
    pub mark_answered: Option<MessageId>,
    /// When set, this message is marked with the `$Forwarded` keyword.
    #[serde(default)]
    pub mark_forwarded: Option<MessageId>,
}

#[tauri::command]
pub async fn send_mail(
    app: AppHandle,
    request: SendMailAndMarkRequest,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // SMTP first — no point marking flags on a send that ultimately failed.
    smtp::send(db, request.send).await?;

    if let Some(parent) = request.mark_answered {
        if let Err(e) = flags_app::apply(
            db,
            parent,
            FlagChanges {
                answered: Some(true),
                ..Default::default()
            },
        )
        .await
        {
            tracing::warn!("mark answered failed (mail already sent): {e}");
        }

        // Per-account "archive-on-reply" workflow. Only applies to replies
        // (not forwards) — the user reasoning is "I answered, so it's done,
        // get it out of the inbox". Failures here are non-fatal: the mail
        // *was* sent and flagged, so we log and move on.
        let auto_archive = {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            queries::get_envelope(&conn, &parent)
                .map_err(|e| e.to_string())?
                .and_then(|env| {
                    queries::get_account(&conn, &env.account_id)
                        .ok()
                        .flatten()
                        .map(|a| a.archive_on_reply)
                })
                .unwrap_or(false)
        };
        if auto_archive {
            match message_ops::archive(db, parent).await {
                Ok(()) => tracing::info!("archive-on-reply: moved {parent:?} to archive"),
                Err(e) => tracing::warn!("archive-on-reply failed (reply already sent): {e}"),
            }
        }
    }
    if let Some(parent) = request.mark_forwarded {
        if let Err(e) = flags_app::apply(
            db,
            parent,
            FlagChanges {
                forwarded: Some(true),
                ..Default::default()
            },
        )
        .await
        {
            tracing::warn!("mark forwarded failed (mail already sent): {e}");
        }
    }

    Ok(())
}

/// Save the composed mail as a draft on the server (IMAP APPEND to the
/// account's Drafts folder with `\Draft \Seen` flags).
///
/// Anders als `send_mail` setzt das hier keinen Empfänger voraus — Drafts
/// dürfen unfertig sein. Wird sowohl vom expliziten "Als Entwurf
/// speichern"-Button als auch vom Failure-Path des optimistischen Send
/// genutzt: schlägt das tatsächliche SMTP-Senden fehl, landet die Mail
/// im Drafts-Ordner statt im Nichts.
#[tauri::command]
pub async fn save_draft(
    app: AppHandle,
    request: SendMailRequest,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    smtp::save_as_draft(db, request).await
}

#[tauri::command]
pub async fn open_message(
    app: AppHandle,
    message_id: MessageId,
) -> Result<MessageDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let envelope = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_envelope(&conn, &message_id)
            .map_err(|e| e.to_string())?
            .ok_or("message not found")?
    };

    // Fast path: body already cached → serve from DB. Attachments are
    // re-parsed from raw_rfc822 so the UI gets the full metadata without
    // storing a duplicate column set.
    if envelope.body_cached {
        if let Some(b) = body::cached(db, &message_id)? {
            let raw = {
                let conn = db.reads.get().map_err(|e| e.to_string())?;
                queries::get_body_raw(&conn, &message_id)
                    .map_err(|e| e.to_string())?
                    .unwrap_or_default()
            };
            let attachments = if raw.is_empty() {
                Vec::new()
            } else {
                attachments::parse_metas(&raw)
            };
            return Ok(MessageDetail {
                envelope,
                plain_text: b.plain_text,
                html_text: b.html_text,
                attachments,
            });
        }
        // Cache flag but no row — fall through to re-fetch.
    }

    let parsed = body::fetch_and_store(&app, db, message_id).await?;
    // Re-read envelope so body_cached flag reflects the write we just made.
    let envelope = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_envelope(&conn, &message_id)
            .map_err(|e| e.to_string())?
            .ok_or("message vanished during fetch")?
    };
    Ok(MessageDetail {
        envelope,
        plain_text: parsed.plain,
        html_text: parsed.html,
        attachments: parsed.attachments,
    })
}

/// Save a single attachment to a destination chosen by the frontend (via
/// `@tauri-apps/plugin-dialog`). Returns the final written path.
#[tauri::command]
pub async fn save_attachment(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
    destination: String,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let dest = PathBuf::from(destination);
    // File I/O on a blocking thread — SQLite read + fs::write shouldn't block the async runtime.
    let written = tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || attachments::save_to(&db, &message_id, part_idx, &dest)
    })
    .await
    .map_err(|e| format!("save task panicked: {e}"))??;
    Ok(written.to_string_lossy().to_string())
}

/// Decode an attachment, write it to a per-message temp directory, and hand
/// the path to the OS default application. Returns the temp path so the
/// frontend can show "Geöffnet aus …" feedback. The chip in the Reader fires
/// this on plain click; the separate save icon still goes through
/// `save_attachment`.
#[tauri::command]
pub async fn open_attachment(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let path = tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || attachments::open_with_default(&db, &message_id, part_idx)
    })
    .await
    .map_err(|e| format!("open task panicked: {e}"))??;
    Ok(path.to_string_lossy().to_string())
}

/// Return the raw bytes of an inline attachment as a data URL. The Reader uses
/// this to rewrite `cid:` image references inside the HTML sandbox so inline
/// images render without a remote request.
#[tauri::command]
pub async fn get_inline_attachment_data_url(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (data, _, mime) = tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || attachments::bytes(&db, &message_id, part_idx)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))??;
    Ok(format!("data:{};base64,{}", mime, base64_encode(&data)))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut iter = bytes.chunks_exact(3);
    for c in iter.by_ref() {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(n & 0x3F) as usize] as char);
    }
    let rem = iter.remainder();
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}
