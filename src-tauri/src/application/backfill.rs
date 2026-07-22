// Background envelope backfill. The eager sync only covers a 30-day
// window per folder; everything older used to require manual
// scroll-to-bottom paging, which meant the search index silently
// missed the "tens of thousands of archived mails" the landing page
// promises to search. This worker walks every sync-enabled folder in
// budgeted chunks until the oldest cached UID is 1 — i.e. the folder
// is fully mirrored header-wise. Envelopes are cheap (~1-2 KB each);
// bodies stay lazy via the prefetch window.
//
// Resumability: no extra state. Each chunk derives its pivot from the
// DB (`sync_folder_older` reads the oldest cached UID), so killing the
// app mid-run just means the next spawn continues where we stopped.
//
// Concurrency: `AppState::backfill_running` de-dupes on account id,
// mirroring the prefetch worker. One chunk at a time, sequential —
// each chunk opens its own short-lived IMAP session via
// `sync_folder_older`/`sync_folder_recent`, so user-driven syncs and
// IDLE actors never queue behind a long-held connection.
//
// Known limitation: if the server has a UID gap right below the pivot
// (expunged mail), a folder can sit at "incomplete" forever — the
// chunk returns zero progress and the run ends. Acceptable: the next
// spawn retries, and genuinely missing UIDs can't be fetched anyway.

use tauri::{AppHandle, Emitter, Manager};

use crate::domain::account::AccountId;
use crate::domain::folder::FolderId;
use crate::infrastructure::db::DbHandle;
use crate::infrastructure::queries;
use crate::state::AppState;

use super::sync;

/// Envelopes per folder per chunk. Small enough that a chunk is a
/// quick IMAP round-trip even on slow links, large enough that a
/// 50k-mail archive finishes within a session or two.
const CHUNK_SIZE: u32 = 500;

/// Tauri event emitted after every pass and at run end so an open
/// search UI can refresh its coverage indicator live.
const EVT_BACKFILL_CHANGED: &str = "backfill-changed";

/// Fire-and-forget entry point, mirroring `prefetch::spawn_for_all_accounts`.
/// Spawns one guarded worker per account and returns immediately.
pub async fn spawn_for_all_accounts(app: AppHandle) {
    let state = app.state::<AppState>();
    let Some(db) = state.db.get() else { return };
    let Ok(conn) = db.reads.get() else { return };
    let Ok(accounts) = queries::list_accounts(&conn) else {
        return;
    };
    drop(conn);
    for a in accounts {
        spawn_for_account(&app, a.id);
    }
}

/// Spawn the backfill worker for a single account. No-ops when the
/// worker is already running (guard inside `run_for_account`).
pub fn spawn_for_account(app: &AppHandle, account_id: AccountId) {
    let state = app.state::<AppState>();
    let Some(db) = state.db.get().cloned() else {
        return;
    };
    let app2 = app.clone();
    tokio::spawn(async move {
        if let Err(e) = run_for_account(app2, db, account_id).await {
            tracing::warn!(account = %account_id.0, error = %e, "backfill run failed");
        }
    });
}

async fn run_for_account(
    app: AppHandle,
    db: DbHandle,
    account_id: AccountId,
) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        let mut running = state
            .backfill_running
            .lock()
            .map_err(|_| "backfill_running poisoned")?;
        if !running.insert(account_id) {
            tracing::debug!(account = %account_id.0, "backfill already running — skipping");
            return Ok(());
        }
    }
    let result = run_passes(&app, &db, &account_id).await;

    let state = app.state::<AppState>();
    if let Ok(mut running) = state.backfill_running.lock() {
        running.remove(&account_id);
    }
    let _ = app.emit(EVT_BACKFILL_CHANGED, ());
    result
}

/// Loop passes over the account's folders until a full pass stores
/// nothing new — that's the "fully mirrored" fixpoint (or "nothing to
/// seed", for genuinely empty folders).
async fn run_passes(
    app: &AppHandle,
    db: &DbHandle,
    account_id: &AccountId,
) -> Result<(), String> {
    loop {
        let mut progress = false;

        // Seed pass: folders with zero cached envelopes get a top-N
        // recent pull to establish the pivot UID the older-than pager
        // needs. `sync_folder_recent` takes the newest of ALL UIDs, so
        // it works even when the 30-day window holds nothing.
        let empty = {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            queries::empty_sync_enabled_folders(&conn, account_id)
                .map_err(|e| e.to_string())?
        };
        for fid in empty {
            let report = sync::sync_folder_recent(app, db, fid, CHUNK_SIZE).await?;
            if report.stored > 0 {
                progress = true;
            }
        }

        // Chunk pass: one older-than chunk per candidate folder.
        let candidates: Vec<FolderId> = {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            queries::backfill_candidate_folders(&conn, account_id)
                .map_err(|e| e.to_string())?
        };
        for fid in &candidates {
            let report = sync::sync_folder_older(app, db, *fid, CHUNK_SIZE).await?;
            if report.stored > 0 {
                progress = true;
            }
        }

        let _ = app.emit(EVT_BACKFILL_CHANGED, ());
        if !progress {
            tracing::info!(account = %account_id.0, "backfill: fully mirrored, stopping");
            break;
        }
    }
    Ok(())
}
