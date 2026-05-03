// Background body prefetch. Philosophy: keep the Inbox-lean workflow
// snappy without dragging down bandwidth for rarely-touched accounts.
//
// Per-account `prefetch_days` selects the time window; the worker
// collects every uncached envelope within that window (Spam/Trash
// excluded — those folders are intentionally avoided in a clean-inbox
// flow), opens exactly ONE IMAP session, and fans out UID FETCH
// BODY.PEEK[] calls per folder. Stored bodies go through the standard
// `WriteCmd::StoreBody` path so the FTS index stays up to date.
//
// Concurrency: `AppState::prefetch_running` de-dupes on account id. A
// trigger for an already-running account is a no-op. If the foreground
// user clicks a mail during prefetch, `body::fetch_and_store` opens its
// own session — no queuing, no priority inversion.

use std::collections::HashMap;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use futures_util::StreamExt;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use crate::domain::account::AccountId;
use crate::infrastructure::db::WriteCmd;
use crate::infrastructure::imap_client;
use crate::infrastructure::queries::{self, PrefetchCandidate};
use crate::state::AppState;

const KEYRING_SERVICE: &str = "crystalmail";

/// Skip bodies larger than this. The raw blob would otherwise blow up the
/// SQLite file without benefit — huge attachments are usually one-shot
/// downloads (invoices, videos) the user opens manually at most once.
const MAX_BODY_BYTES: i64 = 2 * 1024 * 1024; // 2 MB

/// Hard cap on the number of bodies we'll fetch in a single run. A fresh
/// account on first launch could have thousands of uncached envelopes
/// within a 30-day sync window — we don't want to saturate the IMAP
/// connection for minutes. Subsequent runs (after sync) pick up what's
/// left.
const MAX_CANDIDATES_PER_RUN: usize = 200;

/// Fire-and-forget entry point. Spawns the prefetch task on the Tauri
/// async runtime so callers (setup hook, sync command) don't block.
pub fn spawn(app: AppHandle, account_id: AccountId) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run(app, account_id).await {
            tracing::warn!(account = %account_id.0, "prefetch run failed: {e}");
        }
    });
}

pub async fn run(app: AppHandle, account_id: AccountId) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Concurrency gate: bail if another worker owns this account.
    {
        let mut running = state
            .prefetch_running
            .lock()
            .map_err(|_| "prefetch_running poisoned")?;
        if !running.insert(account_id) {
            tracing::debug!(account = %account_id.0, "prefetch already running — skipping");
            return Ok(());
        }
    }
    // Guard ensures the flag is cleared on any exit path (including ?).
    let _guard = RunningGuard {
        app: app.clone(),
        account_id,
    };

    // Load account + check opt-out.
    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, &account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?
    };
    if account.prefetch_days <= 0 {
        tracing::debug!(account = %account_id.0, "prefetch disabled for account");
        return Ok(());
    }

    // Gather candidates inside the time window.
    let since = Utc::now() - Duration::days(account.prefetch_days);
    let mut candidates = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_prefetch_candidates(&conn, &account_id, &since, MAX_BODY_BYTES)
            .map_err(|e| e.to_string())?
    };
    if candidates.is_empty() {
        tracing::debug!(account = %account_id.0, "nothing to prefetch");
        return Ok(());
    }
    candidates.truncate(MAX_CANDIDATES_PER_RUN);
    tracing::info!(
        account = %account_id.0,
        count = candidates.len(),
        days = account.prefetch_days,
        "prefetch: {} candidates",
        candidates.len()
    );

    // Group by folder so each SELECT is followed by N UID FETCHes — one
    // IMAP session total instead of N.
    let mut by_folder: HashMap<String, Vec<PrefetchCandidate>> = HashMap::new();
    for c in candidates {
        by_folder.entry(c.folder_name.clone()).or_default().push(c);
    }

    // Credentials.
    let entry_name = format!("imap::{}", account.id.0);
    let password = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get_password: {e}"))?;

    let client = imap_client::connect_tls(&account.imap_host, account.imap_port).await?;
    let mut session = client
        .login(&account.address, &password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;

    let mut stored_total: usize = 0;
    let mut failed_total: usize = 0;

    'folders: for (folder_name, entries) in by_folder {
        if let Err(e) = session.select(&folder_name).await {
            tracing::warn!(folder = %folder_name, error = %e, "prefetch SELECT failed, skipping folder");
            continue;
        }

        // Build a quick UID → MessageId lookup for stream demux.
        let mut by_uid: HashMap<u32, PrefetchCandidate> =
            entries.into_iter().map(|c| (c.imap_uid, c)).collect();

        // Chunk into reasonable batches — a server should happily accept
        // thousands of UIDs in a set, but smaller batches are friendlier
        // for memory/stream behavior and let us yield between them.
        let uids: Vec<u32> = by_uid.keys().copied().collect();
        for chunk in uids.chunks(25) {
            let uid_set = chunk
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let fetch_stream = match session.uid_fetch(&uid_set, "BODY.PEEK[]").await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(folder = %folder_name, error = %e, "UID FETCH failed");
                    failed_total += chunk.len();
                    continue;
                }
            };
            tokio::pin!(fetch_stream);
            while let Some(result) = fetch_stream.next().await {
                let fetch = match result {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("prefetch fetch stream: {e}");
                        continue;
                    }
                };
                let uid = match fetch.uid {
                    Some(u) => u,
                    None => continue,
                };
                let body_bytes = match fetch.body() {
                    Some(b) => b.to_vec(),
                    None => continue,
                };
                let Some(candidate) = by_uid.remove(&uid) else {
                    continue;
                };

                let parsed = parse_body(&body_bytes);
                // Authoritative attachment indicator from the decoded
                // MIME tree — overwrites the sync-time heuristic on
                // store. Inline parts (cid: images) don't count.
                let metas = crate::application::attachments::parse_metas(&body_bytes);
                let has_attachments = metas.iter().any(|a| !a.is_inline);

                let (tx, rx) = oneshot::channel();
                let send_result = db
                    .writer
                    .send(WriteCmd::StoreBody {
                        message_id: candidate.message_id,
                        raw_rfc822: body_bytes,
                        plain_text: parsed.plain,
                        html_text: parsed.html,
                        has_attachments,
                        ack: tx,
                    })
                    .await;
                if send_result.is_err() {
                    tracing::warn!("prefetch: writer channel closed");
                    break 'folders;
                }
                match rx.await {
                    Ok(Ok(())) => {
                        stored_total += 1;
                        // Body committed ⇒ run the workflow-rule
                        // matcher. Spawned as its own task so a slow
                        // rule can't stall the rest of the prefetch
                        // loop, and matcher errors can't abort the
                        // fetch session either.
                        tauri::async_runtime::spawn(
                            super::workflow_rules::evaluate_and_trigger(
                                app.clone(),
                                db.clone(),
                                candidate.message_id,
                            ),
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("prefetch store body error: {e}");
                        failed_total += 1;
                    }
                    Err(_) => {
                        tracing::warn!("prefetch writer dropped ack");
                        failed_total += 1;
                    }
                }
            }

            // Tiny yield so we don't hold the session hot in a tight loop.
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }
    }

    let _ = session.logout().await;

    tracing::info!(
        account = %account_id.0,
        stored = stored_total,
        failed = failed_total,
        "prefetch: run complete"
    );
    Ok(())
}

struct RunningGuard {
    app: AppHandle,
    account_id: AccountId,
}
impl Drop for RunningGuard {
    fn drop(&mut self) {
        // Bind the State<AppState> guard to a named var — `state::<T>()`
        // returns a temporary that would otherwise be dropped before the
        // lock survives through the `if let`. Same lifetime trick we use
        // for `pi_config` in the startup hook.
        let state = self.app.state::<AppState>();
        let lock_result = state.prefetch_running.lock();
        if let Ok(mut running) = lock_result {
            running.remove(&self.account_id);
        }
    }
}

struct ParsedParts {
    plain: Option<String>,
    html: Option<String>,
}

fn parse_body(raw: &[u8]) -> ParsedParts {
    let Some(msg) = mail_parser::MessageParser::default().parse(raw) else {
        return ParsedParts {
            plain: None,
            html: None,
        };
    };
    ParsedParts {
        plain: msg.body_text(0).map(|c| c.to_string()),
        html: msg.body_html(0).map(|c| c.to_string()),
    }
}

/// Convenience: kick off prefetch for every known account. Called once at
/// startup from the Tauri setup hook, so the user's freshly-launched app
/// races to warm its cache while they're still reading the sidebar.
pub async fn spawn_for_all_accounts(app: AppHandle) {
    let state = app.state::<AppState>();
    let Some(db) = state.db.get() else { return };
    let Ok(conn) = db.reads.get() else { return };
    let Ok(accounts) = queries::list_accounts(&conn) else {
        return;
    };
    drop(conn);
    for a in accounts {
        spawn(app.clone(), a.id);
    }
}
