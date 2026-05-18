// INBOX sync — the "is it real" proof of the IMAP pipeline.
//
// Scope: pull the last 30 days of envelopes for one account's INBOX into the
// local store. No body download, no threading, no IDLE — just proof that
// IMAP → domain → writer → SQLite works end to end.

use std::collections::HashSet;
use std::time::{Duration as StdDuration, Instant};

use chrono::{Duration, TimeZone, Utc};
use futures_util::StreamExt;
use mail_parser::MimeHeaders;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::domain::account::AccountId;
use crate::domain::folder::FolderId;
use crate::domain::message::{Address, Envelope, Flags, MessageId};
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::imap_client;
use crate::infrastructure::queries;

/// Live snapshot of sync progress, emitted as the `sync-progress`
/// Tauri event. Frontend turns this into a hover-tooltip + status-bar
/// line. Numbers are per-folder within a single account; `done=true`
/// signals the whole sync_account call is finished.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub account_id: String,
    pub account_name: String,
    pub folder: String,
    pub fetched: u32,
    pub total: u32,
    pub done: bool,
    /// Count of *newly inserted* INBOX rows during this sync operation
    /// (not re-syncs of UIDs we already had). Drives the "new mail"
    /// chime in the frontend, which only rings on the final
    /// `done=true` event when this is > 0. Always 0 on intermediate
    /// ticks; the cumulative number is set once at the end.
    pub new_in_inbox: u32,
}

/// Throttle: only emit every N processed envelopes + always at start/end
/// of a folder. 25 gives ~4 events per 100 mails — enough for the tooltip
/// to tick visibly, few enough to not drown the event bus.
const EMIT_EVERY: u32 = 25;

/// How long to wait for the next FETCH response before giving up on
/// the current folder. Chosen conservatively: a healthy IMAP session
/// delivers envelopes in tens of milliseconds; 60 s worth of silence
/// means the server stopped talking or imap-proto is stuck on an
/// unparseable envelope.
const FETCH_STEP_TIMEOUT: StdDuration = StdDuration::from_secs(60);

/// Upper bound on how long we wait for the writer actor to ack a
/// single envelope upsert. If prefetch is mid-flight with a 2 MB body
/// + FTS reindex, the queue can back up briefly — 30 s is generous
/// without being "hang forever".
const WRITER_ACK_TIMEOUT: StdDuration = StdDuration::from_secs(30);

const KEYRING_SERVICE: &str = "crystalmail";
const SYNC_WINDOW_DAYS: i64 = 30;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub folder: String,
    pub fetched: u32,
    pub stored: u32,
    pub duration_ms: u128,
}

pub async fn sync_inbox(
    app: &AppHandle,
    db: &DbHandle,
    account_id: AccountId,
    skip_folders: &[String],
) -> Result<SyncReport, String> {
    let started = Instant::now();
    tracing::info!(
        account = %account_id.0,
        skip = ?skip_folders,
        "sync_account: start"
    );

    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, &account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?
    };
    tracing::info!(address = %account.address, host = %account.imap_host, "sync_account: account loaded");

    let entry_name = format!("imap::{}", account.id.0);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?;
    let password = entry
        .get_password()
        .map_err(|e| format!("keyring get: {e} (entry={entry_name})"))?;

    // Canonical folder set per account. Any folder the server doesn't expose
    // (e.g. a Gmail account where the user points Archive at `[Gmail]/All Mail`
    // but has no `Drafts` folder) is skipped with a warning — not a fatal error.
    let mut targets: Vec<String> = vec![
        "INBOX".to_string(),
        account.archive_folder.clone(),
        account.sent_folder.clone(),
        account.drafts_folder.clone(),
        account.trash_folder.clone(),
        account.spam_folder.clone(),
    ];
    targets.dedup();

    let client = imap_client::connect_tls(&account.imap_host, account.imap_port).await?;
    let mut session = client
        .login(&account.address, &password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;
    tracing::info!("sync_account: login ok");

    // Discovery-Pass: Alle Ordner, die der Server über LIST ausspuckt,
    // einmal in die lokale `folders`-Tabelle schreiben. Der eigentliche
    // Envelope-Fetch bleibt in dieser Phase weiterhin auf die Specials
    // beschränkt — Non-Specials werden lazy beim Öffnen synchronisiert
    // (Phase 2). Neu entdeckte Ordner kriegen sync_enabled=1 (Default),
    // bekannte bleiben unverändert.
    if let Err(e) = discover_and_register_folders(&mut session, db, &account.id).await {
        tracing::warn!(error = %e, "folder discovery failed — continuing with configured specials only");
    }

    // Respect the per-folder sync-opt-out. A user who disabled their
    // Archiv in settings shouldn't have it eager-synced every 5 minutes.
    let disabled: HashSet<String> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_account_folders(&conn, &account.id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|f| !f.sync_enabled)
            .map(|f| f.name)
            .collect()
    };

    let mut total_fetched: u32 = 0;
    let mut total_stored: u32 = 0;
    // Cumulative count of brand-new INBOX rows across every folder
    // touched by this sync. Drives the chime via the final done event.
    let mut total_new_in_inbox: u32 = 0;
    let mut synced_labels: Vec<String> = Vec::with_capacity(targets.len());

    for folder_name in &targets {
        if disabled.contains(folder_name) {
            tracing::info!(folder = %folder_name, "skipping — sync disabled for this folder");
            continue;
        }
        // Caller asked us to leave this folder alone — typically because
        // it was already synced as the user-requested priority folder
        // before this call kicked in for "the rest".
        if skip_folders.iter().any(|s| s == folder_name) {
            tracing::debug!(folder = %folder_name, "skipping — already synced as priority");
            continue;
        }
        match sync_one_folder(
            app,
            db,
            &mut session,
            &account.id,
            &account.display_name,
            folder_name,
        )
        .await
        {
            Ok(outcome) => {
                total_fetched += outcome.fetched;
                total_stored += outcome.stored;
                total_new_in_inbox += outcome.new_in_inbox;
                synced_labels.push(folder_name.clone());
            }
            Err(e) => {
                // Missing mailbox or permissions — log and carry on. INBOX
                // failures are still reported as sync failure since nothing
                // useful got synced; everything else is best-effort.
                if folder_name.eq_ignore_ascii_case("INBOX") {
                    let _ = session.logout().await;
                    return Err(format!("INBOX sync failed: {e}"));
                }
                tracing::warn!(folder = %folder_name, error = %e, "optional folder sync failed");
            }
        }
    }

    let _ = session.logout().await;
    let total_ms = started.elapsed().as_millis();
    tracing::info!(
        folders = ?synced_labels,
        total_fetched,
        total_stored,
        total_new_in_inbox,
        total_ms,
        "sync_account: done"
    );

    // Final "done" event — frontend clears the tooltip / status line
    // and rings the new-mail chime if `new_in_inbox > 0`. Note that
    // `fetched` includes re-syncs of already-known UIDs (the SINCE-30d
    // window picks them up every run), so we *can't* use it as the
    // chime trigger — `new_in_inbox` counts only fresh INSERTs.
    let _ = app.emit(
        "sync-progress",
        SyncProgress {
            account_id: account.id.0.to_string(),
            account_name: account.display_name.clone(),
            folder: String::new(),
            fetched: total_fetched,
            total: total_fetched,
            done: true,
            new_in_inbox: total_new_in_inbox,
        },
    );

    // Rule-Sweep nach erfolgreichem Sync. Detached, weil der Sweep pro
    // Mail einen eigenen IMAP-Roundtrip aufmacht (über
    // message_ops::archive/delete/move_to oder eine Workflow-Apply) und
    // wir den Sync-Caller nicht damit warten lassen wollen. Fehler
    // werden vom Sweep selbst geloggt — keine Bubble-up nötig, das hier
    // ist Hintergrund-Hygiene.
    let db_for_sweep = db.clone();
    let app_for_sweep = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::application::rule_scheduler::sweep_once(&app_for_sweep, &db_for_sweep).await;
    });

    Ok(SyncReport {
        folder: synced_labels.join(", "),
        fetched: total_fetched,
        stored: total_stored,
        duration_ms: total_ms,
    })
}

/// Pull at most `limit` of the newest envelopes in a folder. This is
/// the lazy-on-open entry point: when the user clicks into a
/// non-special folder in the sidebar, the frontend calls this so the
/// list populates fast (~50 entries is enough to fill the viewport).
///
/// TTL: if the folder was synced less than `LAZY_TTL` ago, we skip the
/// IMAP round-trip entirely and return a zero-counts report. This
/// keeps rapid folder cycling from hammering the server. Explicit
/// user-driven re-syncs go through the main sync button (which doesn't
/// route here), so the TTL never gets in the way of "I want fresh data".
///
/// Respects the per-folder `sync_enabled` flag from Phase 1: a
/// disabled folder returns early with empty counts.
pub async fn sync_folder_recent(
    app: &AppHandle,
    db: &DbHandle,
    folder_id: FolderId,
    limit: u32,
) -> Result<SyncReport, String> {
    /// How fresh a lazy sync must be to short-circuit a repeat
    /// on-open call. Matches the "feels instant" heuristic we agreed
    /// on — long enough to skip double-clicks and back-and-forth,
    /// short enough that the user gets up-to-date state within the
    /// same work session.
    const LAZY_TTL_SECS: i64 = 300;

    let started = Instant::now();
    let meta = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_folder_meta(&conn, &folder_id)
            .map_err(|e| e.to_string())?
            .ok_or("folder not found")?
    };
    if !meta.sync_enabled {
        tracing::info!(folder = %meta.name, "sync_folder_recent: skipping — sync disabled");
        return Ok(SyncReport {
            folder: meta.name,
            fetched: 0,
            stored: 0,
            duration_ms: started.elapsed().as_millis(),
        });
    }
    if let Some(ts) = meta.last_sync_ts {
        let age = (Utc::now() - ts).num_seconds();
        if age < LAZY_TTL_SECS {
            tracing::debug!(
                folder = %meta.name,
                age_s = age,
                "sync_folder_recent: within TTL, skipping"
            );
            return Ok(SyncReport {
                folder: meta.name,
                fetched: 0,
                stored: 0,
                duration_ms: started.elapsed().as_millis(),
            });
        }
    }

    let (mut session, account_name) =
        open_session_for_account(db, &meta.account_id).await?;

    let result = fetch_recent_in_open_session(
        app,
        db,
        &mut session,
        &meta.account_id,
        &account_name,
        folder_id,
        &meta.name,
        limit,
    )
    .await;

    let _ = session.logout().await;

    let outcome = result?;
    Ok(SyncReport {
        folder: meta.name,
        fetched: outcome.fetched,
        stored: outcome.stored,
        duration_ms: started.elapsed().as_millis(),
    })
}

/// Pull at most `limit` envelopes older than whatever is currently
/// cached locally for the folder. This is the scroll-to-bottom pager:
/// the frontend just says "more please", and the backend resolves the
/// pivot UID from the DB. No TTL — this is always an explicit "give
/// me more" from the user.
///
/// Returns zero counts if the folder has no cached envelopes yet
/// (caller should drive `sync_folder_recent` first) or if the oldest
/// cached UID is already 1 (nothing older possible on the server).
pub async fn sync_folder_older(
    app: &AppHandle,
    db: &DbHandle,
    folder_id: FolderId,
    limit: u32,
) -> Result<SyncReport, String> {
    let started = Instant::now();
    let (meta, before_uid) = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let meta = queries::get_folder_meta(&conn, &folder_id)
            .map_err(|e| e.to_string())?
            .ok_or("folder not found")?;
        let pivot = queries::oldest_cached_uid(&conn, &folder_id)
            .map_err(|e| e.to_string())?;
        (meta, pivot)
    };
    if !meta.sync_enabled {
        return Ok(SyncReport {
            folder: meta.name,
            fetched: 0,
            stored: 0,
            duration_ms: started.elapsed().as_millis(),
        });
    }
    let Some(before_uid) = before_uid else {
        // Empty cache — there is nothing "older than" a non-existent
        // pivot. Caller should have run sync_folder_recent first.
        return Ok(SyncReport {
            folder: meta.name,
            fetched: 0,
            stored: 0,
            duration_ms: started.elapsed().as_millis(),
        });
    };
    if before_uid <= 1 {
        // There is no UID smaller than 1 on the server — short-circuit
        // so the frontend can stop paging.
        return Ok(SyncReport {
            folder: meta.name,
            fetched: 0,
            stored: 0,
            duration_ms: started.elapsed().as_millis(),
        });
    }

    let (mut session, account_name) =
        open_session_for_account(db, &meta.account_id).await?;

    let result = fetch_older_in_open_session(
        app,
        db,
        &mut session,
        &meta.account_id,
        &account_name,
        folder_id,
        &meta.name,
        before_uid,
        limit,
    )
    .await;

    let _ = session.logout().await;

    let outcome = result?;
    Ok(SyncReport {
        folder: meta.name,
        fetched: outcome.fetched,
        stored: outcome.stored,
        duration_ms: started.elapsed().as_millis(),
    })
}

/// Canonical-view paging: pull older envelopes for every account that
/// participates in the given unified bucket (e.g. `archive`). Wraps
/// `sync_folder_older` once per account, accumulating fetched/stored
/// counts. Used by the unified-archive scroll-to-bottom path — without
/// this, scrolling past the bottom of the unified Archive view would
/// only ever grow the local DB for a single account.
///
/// `account_filter`: when `Some`, only that account is touched (matches
/// the sidebar account-filter scope). Errors from individual accounts
/// are logged but don't abort the whole call — a flaky one shouldn't
/// keep the others from delivering.
pub async fn sync_unified_folder_older(
    app: &AppHandle,
    db: &DbHandle,
    folder_key: &str,
    account_filter: Option<AccountId>,
    limit: u32,
) -> Result<SyncReport, String> {
    let started = Instant::now();
    let targets: Vec<(AccountId, FolderId)> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::folder_ids_for_canonical(&conn, folder_key, account_filter.as_ref())
            .map_err(|e| e.to_string())?
    };

    let mut total_fetched: u32 = 0;
    let mut total_stored: u32 = 0;
    for (_acc, fid) in targets {
        // Per-account `sync_folder_older` opens its own session. We
        // accept the per-account login overhead here — bundling into
        // one parallel walker isn't worth the extra plumbing for a
        // user gesture that runs at most a few times per scroll.
        match sync_folder_older(app, db, fid, limit).await {
            Ok(r) => {
                total_fetched = total_fetched.saturating_add(r.fetched);
                total_stored = total_stored.saturating_add(r.stored);
            }
            Err(e) => {
                tracing::warn!(
                    folder_key,
                    error = %e,
                    "sync_unified_folder_older: per-account fetch failed"
                );
            }
        }
    }

    Ok(SyncReport {
        folder: folder_key.to_string(),
        fetched: total_fetched,
        stored: total_stored,
        duration_ms: started.elapsed().as_millis(),
    })
}

/// Shared boilerplate: resolve account creds, connect TLS, login.
/// Returns the session and the account's display name so callers
/// can emit progress events with a human label. All three lazy/single
/// entry points do the exact same thing, so factor it out.
///
/// Visibility: `pub(crate)` damit der per-Konto-Actor (siehe
/// `application::actor`) seine eigene IDLE-Session damit hochziehen kann.
pub(crate) async fn open_session_for_account(
    db: &DbHandle,
    account_id: &AccountId,
) -> Result<
    (
        async_imap::Session<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
        String,
    ),
    String,
> {
    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?
    };
    let entry_name = format!("imap::{}", account.id.0);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?;
    let password = entry
        .get_password()
        .map_err(|e| format!("keyring get: {e} (entry={entry_name})"))?;
    let client = imap_client::connect_tls(&account.imap_host, account.imap_port).await?;
    let session = client
        .login(&account.address, &password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;
    Ok((session, account.display_name))
}

/// `sync_folder_recent`'s inner half: once the session is open, select
/// the folder, pull all UIDs, take the top `limit`, and drive the fetch.
/// Returns `(fetched, stored)`.
///
/// Visibility: `pub(crate)` damit der IDLE-Actor (siehe `application::actor`)
/// nach einem Server-Push direkt auf seiner langlebigen Session den
/// Refresh fahren kann, ohne eine zweite Login-Roundtrip zu zahlen.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_recent_in_open_session(
    app: &AppHandle,
    db: &DbHandle,
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    account_id: &AccountId,
    account_name: &str,
    folder_id: FolderId,
    folder_name: &str,
    limit: u32,
) -> Result<FolderFetchOutcome, String> {
    let mailbox = session
        .select(folder_name)
        .await
        .map_err(|e| format!("SELECT {folder_name}: {e}"))?;
    if let Err(e) = handle_uidvalidity_change(
        db,
        folder_id,
        folder_name,
        mailbox.uid_validity,
    )
    .await
    {
        tracing::warn!(folder = %folder_name, error = %e, "UIDVALIDITY purge failed; continuing");
    }
    update_folder_uid_state(db, folder_id, &mailbox).await;

    // `UID SEARCH ALL` pulls every selectable UID. The lists get large
    // on busy folders but it's a cheap IMAP command — the server keeps
    // UIDs in an index, and even tens of thousands of u32s cost well
    // under a MB on the wire. We then sort client-side and take the
    // top N. Alternative would be `UID SEARCH UID <uid_next-limit>:*`
    // but that overshoots when the server has gaps from expunged
    // mails, leaving us with <limit envelopes.
    let uids: HashSet<u32> = session
        .uid_search("ALL")
        .await
        .map_err(|e| format!("UID SEARCH ({folder_name}): {e}"))?;

    // Reconcile against the full server set BEFORE picking the tail —
    // the IDLE/recent path runs after every server push, so this is
    // the place that catches Sieve/spam moves the moment they happen.
    if let Err(e) = reconcile_folder_uids(db, folder_id, folder_name, &uids).await {
        tracing::warn!(folder = %folder_name, error = %e, "reconcile failed; continuing");
    }

    let mut sorted: Vec<u32> = uids.into_iter().collect();
    sorted.sort_unstable();
    let take = limit as usize;
    let tail = if sorted.len() > take {
        sorted.split_off(sorted.len() - take)
    } else {
        std::mem::take(&mut sorted)
    };
    // tail: ascending, highest `limit` UIDs
    drive_uid_fetch(
        app,
        db,
        session,
        account_id,
        account_name,
        folder_id,
        folder_name,
        &tail,
    )
    .await
}

/// `sync_folder_older`'s inner half: select + UID-search, filter out
/// anything `>= before_uid`, take the top `limit`, drive fetch.
#[allow(clippy::too_many_arguments)]
async fn fetch_older_in_open_session(
    app: &AppHandle,
    db: &DbHandle,
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    account_id: &AccountId,
    account_name: &str,
    folder_id: FolderId,
    folder_name: &str,
    before_uid: u32,
    limit: u32,
) -> Result<FolderFetchOutcome, String> {
    let mailbox = session
        .select(folder_name)
        .await
        .map_err(|e| format!("SELECT {folder_name}: {e}"))?;
    update_folder_uid_state(db, folder_id, &mailbox).await;

    // Ask the server directly for the UID range we care about. `1:N-1`
    // avoids transferring UIDs we'd discard client-side and is
    // trivially fast on any IMAP server.
    let criterion = format!("UID 1:{}", before_uid.saturating_sub(1));
    let uids: HashSet<u32> = session
        .uid_search(&criterion)
        .await
        .map_err(|e| format!("UID SEARCH ({folder_name}): {e}"))?;
    let mut sorted: Vec<u32> = uids.into_iter().collect();
    sorted.sort_unstable();
    let take = limit as usize;
    let tail = if sorted.len() > take {
        sorted.split_off(sorted.len() - take)
    } else {
        std::mem::take(&mut sorted)
    };
    drive_uid_fetch(
        app,
        db,
        session,
        account_id,
        account_name,
        folder_id,
        folder_name,
        &tail,
    )
    .await
}

/// Sync **one** folder, standalone. Opens its own IMAP session, SELECTs
/// the folder, pulls envelopes with the same 30-day window as the full
/// sync, and logs out. Used by the sync-button priority path: the
/// currently-visible folder gets synced first so the UI updates fast,
/// then the rest of the account runs in the background.
///
/// Returns a `SyncReport` shaped like the full sync's report — just
/// with a single folder name and its counts.
pub async fn sync_single_folder(
    app: &AppHandle,
    db: &DbHandle,
    account_id: AccountId,
    folder_name: &str,
) -> Result<SyncReport, String> {
    let started = Instant::now();
    tracing::info!(
        account = %account_id.0,
        folder = %folder_name,
        "sync_single_folder: start"
    );

    let account = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_account(&conn, &account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?
    };

    let entry_name = format!("imap::{}", account.id.0);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?;
    let password = entry
        .get_password()
        .map_err(|e| format!("keyring get: {e} (entry={entry_name})"))?;

    // Short-circuit: if the user explicitly disabled this folder in
    // settings, honour that even when it was handed in as priority.
    // Priority only reorders work — it shouldn't override opt-out.
    let disabled_here = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_account_folders(&conn, &account_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .any(|f| !f.sync_enabled && f.name == folder_name)
    };
    if disabled_here {
        tracing::info!(
            folder = %folder_name,
            "sync_single_folder: skipping — sync disabled for this folder"
        );
        return Ok(SyncReport {
            folder: folder_name.to_string(),
            fetched: 0,
            stored: 0,
            duration_ms: started.elapsed().as_millis(),
        });
    }

    let client = imap_client::connect_tls(&account.imap_host, account.imap_port).await?;
    let mut session = client
        .login(&account.address, &password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;

    let result = sync_one_folder(
        app,
        db,
        &mut session,
        &account.id,
        &account.display_name,
        folder_name,
    )
    .await;

    let _ = session.logout().await;

    let outcome = result?;
    let duration_ms = started.elapsed().as_millis();
    tracing::info!(
        folder = %folder_name,
        fetched = outcome.fetched,
        stored = outcome.stored,
        new_in_inbox = outcome.new_in_inbox,
        duration_ms,
        "sync_single_folder: done"
    );

    // Tell the frontend the priority folder is settled so it can flip
    // into "the rest is running in the background" state. The chime
    // count rides along — when the priority folder *is* INBOX, this is
    // the first done event the user gets, so it must carry the count.
    let _ = app.emit(
        "sync-progress",
        SyncProgress {
            account_id: account.id.0.to_string(),
            account_name: account.display_name.clone(),
            folder: folder_name.to_string(),
            fetched: outcome.fetched,
            total: outcome.fetched,
            done: true,
            new_in_inbox: outcome.new_in_inbox,
        },
    );

    Ok(SyncReport {
        folder: folder_name.to_string(),
        fetched: outcome.fetched,
        stored: outcome.stored,
        duration_ms,
    })
}

/// Sync a single IMAP folder using the 30-day SINCE window. Returns
/// the per-folder counts. Assumes the session is already logged in;
/// caller handles logout.
async fn sync_one_folder(
    app: &AppHandle,
    db: &DbHandle,
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    account_id: &AccountId,
    account_name: &str,
    folder_name: &str,
) -> Result<FolderFetchOutcome, String> {
    let mailbox = session
        .select(folder_name)
        .await
        .map_err(|e| format!("SELECT {folder_name}: {e}"))?;
    tracing::info!(
        folder = %folder_name,
        exists = mailbox.exists,
        uid_validity = ?mailbox.uid_validity,
        uid_next = ?mailbox.uid_next,
        "sync_folder: SELECT ok"
    );

    let folder_id = ensure_folder(db, account_id, folder_name).await?;
    // Detect UIDVALIDITY change BEFORE persisting the new value — once
    // updated, we'd lose the prior reference and silently keep stale
    // rows around forever.
    if let Err(e) = handle_uidvalidity_change(
        db,
        folder_id,
        folder_name,
        mailbox.uid_validity,
    )
    .await
    {
        tracing::warn!(folder = %folder_name, error = %e, "UIDVALIDITY purge failed; continuing");
    }
    update_folder_uid_state(db, folder_id, &mailbox).await;

    // Reconcile against the FULL server-side UID set, not just the
    // SINCE-30d window. The window-search drives the fetch (we only
    // pay envelope bandwidth for recent mail), but pruning has to see
    // the complete picture — anything older than 30 days that we
    // already cached must stay, and only UIDs the server doesn't have
    // at all should be dropped. `UID SEARCH ALL` is one cheap RTT.
    let all_uids: HashSet<u32> = session
        .uid_search("ALL")
        .await
        .map_err(|e| format!("UID SEARCH ALL ({folder_name}): {e}"))?;
    if let Err(e) =
        reconcile_folder_uids(db, folder_id, folder_name, &all_uids).await
    {
        tracing::warn!(folder = %folder_name, error = %e, "reconcile failed; continuing");
    }

    // Sent / Drafts / Archive often store older messages that still matter.
    // Use the same 30-day window as INBOX so the unified view is useful.
    let since_date = Utc::now() - Duration::days(SYNC_WINDOW_DAYS);
    let query = format!("SINCE {}", since_date.format("%d-%b-%Y"));
    let uids: HashSet<u32> = session
        .uid_search(&query)
        .await
        .map_err(|e| format!("UID SEARCH ({folder_name}): {e}"))?;

    let mut sorted: Vec<u32> = uids.into_iter().collect();
    sorted.sort_unstable();

    drive_uid_fetch(
        app,
        db,
        session,
        account_id,
        account_name,
        folder_id,
        folder_name,
        &sorted,
    )
    .await
}

/// Drive the actual FETCH stream for a given set of UIDs and upsert
/// everything we get back. Shared by the eager SINCE-window sync,
/// the lazy recent-N sync (folder open in the UI), and the lazy
/// older-than sync (scroll to bottom). All three paths share the
/// same spam-rule pass, envelope-parse step, and writer-ack logic.
///
/// `uids_sorted` must be ascending (the IMAP set-string we build from
/// it is passed verbatim to the server — order affects response
/// order, not correctness, but we emit progress as they come in).
#[allow(clippy::too_many_arguments)]
async fn drive_uid_fetch(
    app: &AppHandle,
    db: &DbHandle,
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    account_id: &AccountId,
    account_name: &str,
    folder_id: FolderId,
    folder_name: &str,
    uids_sorted: &[u32],
) -> Result<FolderFetchOutcome, String> {
    let emit_progress = |fetched: u32, total: u32| {
        let _ = app.emit(
            "sync-progress",
            SyncProgress {
                account_id: account_id.0.to_string(),
                account_name: account_name.to_string(),
                folder: folder_name.to_string(),
                fetched,
                total,
                done: false,
                // Intermediate ticks always carry 0 — only the final
                // done event broadcasts the cumulative new-mail count.
                new_in_inbox: 0,
            },
        );
    };
    // Only INBOX inserts count for the chime — Sent/Drafts/Archive
    // arrivals are typically the user's own outbound mail or system
    // moves, not "you got new mail". Comparing case-insensitively to
    // be tolerant of servers that quote it as `Inbox` (rare but seen).
    let count_for_chime = folder_name.eq_ignore_ascii_case("INBOX");
    // Pull the active spam rules *once* per sync run and pre-compile
    // their regex patterns in the same step. They're small rows and
    // pi-generated regexes don't change mid-sync, so the envelope loop
    // can run matchers purely in-memory without ever hitting SQLite or
    // `Regex::new` again. Saves on a 500-mail / 5-regex-rule sync alone
    // ~2.500 unnötige Regex-Compiles.
    let rules = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let raw = crate::infrastructure::queries::list_enabled_spam_rules(
            &conn,
            Some(account_id),
        )
        .map_err(|e| e.to_string())?;
        crate::application::spam_rules::compile_all(raw)
    };
    // Workflow-Rules-Fast-Path: alle aktiven Rules für diesen Account
    // einmal pro Sync-Run laden. Anwendbar nur auf INBOX-Eintreffer;
    // in anderen Ordnern (Sent, Drafts, Archive) macht das Match-and-
    // tag keinen Sinn — der User hat dort schon eine Aktion getroffen.
    //
    // Body-store-time matcher (workflow_rules::evaluate_and_trigger)
    // läuft als zweite Stufe für Predicates, die den Body brauchen
    // (HasAttachmentExtension) — der Sync-Pfad hier kümmert sich nur
    // um envelope-auflösbare Rules + Direkt-Aktionen / Tagging mit Delay.
    let workflow_rules_for_inbox = if folder_name.eq_ignore_ascii_case("INBOX") {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        crate::infrastructure::queries::list_enabled_workflow_rules_for_account(
            &conn,
            account_id,
        )
        .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    // Skip spam/trash folders for the match pass — their envelopes
    // shouldn't be flagged by rules designed for the inbox. Cheap
    // guard: resolve account's spam+trash folder names and compare.
    // We also capture the spam-folder name itself so the auto-move
    // path below has a destination to point at without re-querying.
    let (skip_rules, spam_folder_for_move): (bool, Option<String>) = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        match queries::get_account(&conn, account_id).ok().flatten() {
            Some(acc) => {
                let skip = folder_name == acc.spam_folder
                    || folder_name == acc.trash_folder;
                let dest = (!acc.spam_folder.trim().is_empty()
                    && folder_name != acc.spam_folder)
                    .then(|| acc.spam_folder.clone());
                (skip, dest)
            }
            None => (false, None),
        }
    };

    let mut fetched: u32 = 0;
    let mut stored: u32 = 0;
    let mut new_in_inbox: u32 = 0;
    let total = uids_sorted.len() as u32;

    // Kick off a "starting this folder" event even when there's nothing
    // to do — so the frontend can flip the tooltip label between folders.
    emit_progress(0, total);

    if !uids_sorted.is_empty() {
        let set = uids_sorted
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");

        // Wir ziehen bewusst BODY.PEEK[HEADER] statt ENVELOPE. Grund:
        // imap-proto 0.16.7 (via async-imap 0.10.4) hat einen nom-TakeWhile1-
        // Parser-Bug bei bestimmten ENVELOPE-Atomen — typisch bei
        // Spam-Subjects à la "***SPAM***..." mit RFC-2047-Encoding.
        // Einmal getroffen, vergiftet das async-imap's FramedRead-Buffer
        // (siehe imap_stream.rs:104: decode_needs=0, aber buffer wird nicht
        // zurückgesetzt) und der Stream liefert endlos dieselbe Fehler-
        // Meldung → Endlosschleife im Sync. BODY.PEEK[HEADER] liefert die
        // rohen Header-Bytes, die wir dann dem sehr toleranten mail-parser
        // übergeben. Kostet ~1-2 KB mehr pro Mail, dafür kein vergiftbarer
        // Stream und zugleich fällt die manuelle RFC-2047-/IMAP-Quoted-
        // String-Entschlüsselung in map_fetch weg.
        let mut stream = session
            .uid_fetch(
                &set,
                "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])",
            )
            .await
            .map_err(|e| format!("UID FETCH ({folder_name}): {e}"))?;

        loop {
            // Bounded wait for each next envelope. If the server
            // stops responding mid-stream — happens on some servers
            // when the Papierkorb has a lot of stale UIDs, or when
            // imap-proto can't parse a borderline-invalid envelope —
            // we'd otherwise hang the whole sync forever. Break
            // gracefully with what we have.
            let next = match tokio::time::timeout(
                FETCH_STEP_TIMEOUT,
                stream.next(),
            )
            .await
            {
                Ok(Some(r)) => r,
                Ok(None) => break, // stream drained normally
                Err(_) => {
                    tracing::warn!(
                        folder = %folder_name,
                        after = fetched,
                        total,
                        "fetch stream silent for {}s — aborting this folder",
                        FETCH_STEP_TIMEOUT.as_secs()
                    );
                    break;
                }
            };
            let fetch = match next {
                Ok(f) => f,
                Err(e) => {
                    // async-imap 0.10.4 does NOT reset its parse buffer on
                    // imap-proto parse errors (see imap_stream.rs:104). Once
                    // a FETCH response fails to parse, every subsequent
                    // poll_next hits the exact same bytes and yields the
                    // same error — a tight infinite error loop that the
                    // 60s silence-timeout above can't catch. Observed with
                    // envelopes whose Subject starts with "***SPAM***".
                    // Breaking here keeps everything already fetched and
                    // moves on to the next folder.
                    tracing::warn!(
                        folder = %folder_name,
                        error = %e,
                        after = fetched,
                        total,
                        "fetch stream parse error — aborting folder to avoid poisoned-stream loop"
                    );
                    break;
                }
            };
            fetched += 1;
            tracing::debug!(
                folder = %folder_name,
                uid = ?fetch.uid,
                at = fetched,
                of = total,
                "fetch: envelope received"
            );
            // Coarse progress throttle. Every 25 mails we send a
            // tick so the tooltip updates visibly without flooding
            // the event bus. Final count is emitted below once the
            // stream drains.
            if fetched % EMIT_EVERY == 0 {
                emit_progress(fetched, total);
            }

            let Some(mut envelope) = map_fetch(&fetch, *account_id, folder_id) else {
                continue;
            };

            // Apply active spam rules to the freshly-parsed envelope.
            // Match → flag $Junk locally, schedule a server-side move
            // to the account's spam folder. Per the design discussion:
            // if a rule fires, it fires; if too much gets moved, the
            // rule itself is bad — so we trust the user's rules
            // unconditionally rather than gate on a confidence score
            // or wait for a manual confirm. `hit_count` is bumped
            // synchronously enough to be useful for "is this rule
            // even firing"; the actual move runs as a spawned task so
            // a slow IMAP round-trip can't stall the fetch loop.
            //
            // Known trade-off (carried over from the flag-only era):
            // if the user toggled `junk` off on an already-flagged
            // envelope and a subsequent sync re-fetches it, this path
            // re-fires — and now also re-moves. That's the right
            // behaviour given the new "trust the rules" stance: the
            // user's escape hatch is to disable or refine the rule.
            let mut matched_rule = false;
            if !skip_rules && !rules.is_empty() && !envelope.flags.junk {
                if let Some(hit_id) = match_envelope(&envelope, &rules) {
                    envelope.flags.junk = true;
                    matched_rule = true;
                    let (tx, _rx) = oneshot::channel();
                    let _ = db
                        .writer
                        .send(WriteCmd::IncrementSpamRuleHits {
                            rule_id: hit_id,
                            delta: 1,
                            ack: tx,
                        })
                        .await;
                    // Intentionally not awaiting rx — the hit-count
                    // bump is purely statistical and its failure
                    // shouldn't stall sync.
                }
            }

            // Workflow-Rules-Match-at-Sync: erste passende Rule mit
            // envelope-auflösbaren Predicates liefert ein
            // `ScheduledActionTag`. Spam-markierte Mails überspringen
            // wir — Tagging im Spam-Ordner ergibt keinen Sinn, weil der
            // Sweeper Mails aus dem Inbox-Folder anpackt.
            //
            // Das Tag wird hier nur berechnet, nicht persistiert — die
            // DB-Row gibt's noch nicht. Tagging passiert in einer
            // zweiten Writer-Roundtrip nach dem erfolgreichen Upsert.
            let scheduled_tag = if !workflow_rules_for_inbox.is_empty() && !envelope.flags.junk {
                crate::application::rule_scheduler::match_at_sync_time(
                    &envelope,
                    folder_name,
                    &workflow_rules_for_inbox,
                )
            } else {
                None
            };

            // Capture the IMAP UID before `envelope` moves into the
            // WriteCmd — the auto-move spawn task uses it (paired with
            // folder_id) to look up the persisted message_id once the
            // writer commits.
            let envelope_uid = envelope.imap_uid;

            let (tx, rx) = oneshot::channel();
            if db
                .writer
                .send(WriteCmd::UpsertEnvelope {
                    envelope,
                    body_text: None,
                    ack: tx,
                })
                .await
                .is_err()
            {
                return Err("writer channel closed".into());
            }
            // Writer ack is normally near-instant, but can back up
            // when prefetch is writing a multi-MB body in parallel.
            // Bound the wait so we don't stall the FETCH stream.
            //
            // The bool the writer hands back tells us whether this was
            // a brand-new envelope or a re-sync of a UID we already
            // had — only the new ones count toward the new-mail chime.
            let upsert_ok = match tokio::time::timeout(WRITER_ACK_TIMEOUT, rx).await {
                Ok(Ok(Ok(was_new))) => {
                    stored += 1;
                    // Spam-Rule-Treffer zählen NICHT als "neue Mail" für die
                    // Chime-/Refresh-Pipeline: der nachgelagerte Auto-Move
                    // verschiebt sie sowieso aus der Inbox, also würden
                    // Audiosignal und Inbox-Refresh nur kurz für einen
                    // Eintrag triggern, der sofort wieder verschwindet —
                    // genau die "Spam-Ping"-UX, die der Filter verhindern
                    // soll. Mit dem !matched_rule-Gate bleibt newInInbox=0,
                    // wenn nur Filter-Treffer reinkamen → kein Ton, kein
                    // Refresh, kein Aufblitzen in der Liste.
                    if was_new && count_for_chime && !matched_rule {
                        new_in_inbox += 1;
                    }
                    true
                }
                Ok(Ok(Err(e))) => {
                    tracing::warn!(folder = %folder_name, error = %e, "upsert failed");
                    false
                }
                Ok(Err(_)) => {
                    tracing::warn!(folder = %folder_name, "writer dropped ack");
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        folder = %folder_name,
                        "writer ack timeout ({}s) — envelope not stored, continuing",
                        WRITER_ACK_TIMEOUT.as_secs()
                    );
                    false
                }
            };

            // Spam-rule auto-move: the row is now committed, so we can
            // safely spawn an async task that opens its own IMAP
            // session, sets $Junk on the server, and moves the mail
            // to the account's spam folder. We do this in a detached
            // task so the fetch loop isn't blocked by per-mail IMAP
            // round-trips — N matches in a sync still take O(1) wall
            // time from sync's perspective. Errors are logged but
            // intentionally not surfaced: the user's local view
            // already shows the SPAM badge thanks to junk=true; if
            // the IMAP move fails (network blip, server flake), the
            // next sync's match-pass will retry.
            if upsert_ok && matched_rule {
                if let Some(dest) = spam_folder_for_move.clone() {
                    let app2 = app.clone();
                    let db2 = db.clone();
                    let acct = *account_id;
                    let fid = folder_id;
                    tauri::async_runtime::spawn(async move {
                        auto_move_to_spam(&app2, &db2, &acct, &fid, envelope_uid, dest)
                            .await;
                    });
                }
            }

            // Scheduled-Action-Tag persistieren. Erst NACH erfolgreichem
            // Upsert — sonst läuft das UPDATE auf eine nicht-existente
            // Row und wäre ein No-op, ohne dass wir es merken. Wir
            // brauchen die persistierte message_id; weil `map_fetch`
            // bei Re-Sync einer bekannten UID nicht garantiert
            // dieselbe UUID liefert wie der erste Insert, lookup über
            // (folder_id, imap_uid) — genauso wie `auto_move_to_spam`.
            //
            // Bei Direkt-Action + delay_minutes=0 setzt das Tag `scheduled_at`
            // auf jetzt; der Sweeper, der gleich nach dem Sync läuft,
            // packt die Mail im selben Tick an. Das spart eine extra
            // Spawn-Branche im Sync-Loop und garantiert, dass das Audit-
            // Log einen sauberen Eintrag pro Action bekommt.
            if upsert_ok {
                if let Some(tag) = scheduled_tag {
                    let resolved_id = {
                        let conn = match db.reads.get() {
                            Ok(c) => Some(c),
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "scheduled tag: db read for id-lookup failed"
                                );
                                None
                            }
                        };
                        conn.and_then(|conn| {
                            conn.query_row(
                                "SELECT id FROM envelopes
                                  WHERE folder_id = ?1 AND imap_uid = ?2",
                                rusqlite::params![
                                    folder_id.0.to_string(),
                                    envelope_uid
                                ],
                                |r| r.get::<_, String>(0),
                            )
                            .ok()
                            .and_then(|s| Uuid::parse_str(&s).ok())
                            .map(MessageId)
                        })
                    };
                    if let Some(message_id) = resolved_id {
                        crate::application::rule_scheduler::tag_after_upsert(
                            db,
                            message_id,
                            tag,
                        )
                        .await;
                    }
                }
            }
        }
    }

    tracing::info!(
        folder = %folder_name,
        fetched,
        stored,
        new_in_inbox,
        "sync_folder: done"
    );
    // Final per-folder tick — makes sure the tooltip shows the exact
    // end number even when the last throttled tick missed the total.
    emit_progress(fetched, total);
    Ok(FolderFetchOutcome {
        fetched,
        stored,
        new_in_inbox,
    })
}

/// Per-folder result of a fetch loop. The new-mail count is only ever
/// non-zero when the folder was INBOX — see `count_for_chime` above.
pub(crate) struct FolderFetchOutcome {
    pub(crate) fetched: u32,
    pub(crate) stored: u32,
    pub(crate) new_in_inbox: u32,
}

/// Auto-move a freshly-matched spam envelope to the account's spam
/// folder. Spawned as a detached task from the sync loop — runs after
/// the writer has committed the row, so the (folder_id, imap_uid) →
/// message_id lookup is safe.
///
/// Reuses the same IMAP-side helpers user-driven actions take:
///   * `flags::apply` to set `$Junk` server-side (so other clients
///     see the spam classification too, and a re-sync won't undo it),
///   * `message_ops::move_to` to MOVE the UID into the spam folder
///     and drop the local row.
///
/// All failures are logged but intentionally swallowed: the local
/// `junk=true` flag is already in the DB, so the user sees the SPAM
/// badge regardless. If the network move fails, the next sync's
/// match-pass will retry — and if it keeps failing, the rule is firing
/// faster than IMAP can keep up, which is fine to fix manually.
async fn auto_move_to_spam(
    _app: &AppHandle,
    db: &crate::infrastructure::db::DbHandle,
    _account_id: &AccountId,
    folder_id: &FolderId,
    imap_uid: u32,
    dest_folder: String,
) {
    // Translate (folder_id, imap_uid) → message_id. The fresh UUID
    // generated in `map_fetch` may not be the persisted id when the
    // upsert took the conflict-update path (rule firing on a UID we
    // already had cached). One indexed point lookup, sub-millisecond.
    let message_id = {
        let conn = match db.reads.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "auto_move_to_spam: db read failed");
                return;
            }
        };
        let row: Result<String, _> = conn.query_row(
            "SELECT id FROM envelopes WHERE folder_id = ?1 AND imap_uid = ?2",
            rusqlite::params![folder_id.0.to_string(), imap_uid],
            |r| r.get(0),
        );
        match row {
            Ok(s) => match Uuid::parse_str(&s) {
                Ok(u) => crate::domain::message::MessageId(u),
                Err(e) => {
                    tracing::warn!(error = %e, "auto_move_to_spam: bad uuid");
                    return;
                }
            },
            Err(e) => {
                tracing::warn!(
                    folder_id = %folder_id.0,
                    imap_uid,
                    error = %e,
                    "auto_move_to_spam: envelope row not found"
                );
                return;
            }
        }
    };

    // Step 1 — set $Junk on IMAP. message_ops::move_to copies the row
    // to the destination, but the IMAP keyword has to be set on the
    // *source* side first so the copy carries it. Without this, a
    // future sync of the spam folder would see no $Junk and the spam
    // candidate badge would render in the wrong tone.
    if let Err(e) = crate::application::flags::apply(
        db,
        message_id,
        crate::domain::message::FlagChanges {
            junk: Some(true),
            ..Default::default()
        },
    )
    .await
    {
        tracing::warn!(
            message_id = %message_id.0,
            error = %e,
            "auto_move_to_spam: flag step failed (proceeding anyway)"
        );
        // Don't return — the move below is independently useful, and
        // re-running this whole task on next sync would re-flag.
    }

    // Step 2 — IMAP MOVE (or COPY+EXPUNGE fallback). Drops the local
    // source row on success, so the UI updates immediately.
    if let Err(e) = crate::application::message_ops::move_to(
        db,
        message_id,
        dest_folder.clone(),
    )
    .await
    {
        tracing::warn!(
            message_id = %message_id.0,
            dest = %dest_folder,
            error = %e,
            "auto_move_to_spam: move step failed"
        );
        return;
    }

    tracing::info!(
        message_id = %message_id.0,
        dest = %dest_folder,
        "auto_move_to_spam: ok"
    );
}

/// Walk all active rules and return the id of the first one that hits.
/// Only from/subject-based patterns are checked at sync time — body
/// and header rules require a body blob we don't have yet (bodies
/// arrive asynchronously via prefetch). Running those rules later is
/// possible but would double the complexity; v1 keeps sync flat.
///
/// Takes pre-compiled rules (`spam_rules::Compiled`) so the regex for
/// every `SubjectRegex` pattern is parsed once per sync, not per envelope.
fn match_envelope(
    envelope: &Envelope,
    rules: &[crate::application::spam_rules::Compiled],
) -> Option<crate::domain::spam_rule::SpamRuleId> {
    use crate::domain::spam_rule::SpamPatternType as P;
    let from_email = envelope
        .from
        .first()
        .map(|a| a.email.as_str())
        .unwrap_or("");
    let features = crate::application::spam_rules::MatchFeatures::from_parts(
        from_email,
        &envelope.subject,
        None, // body not loaded at sync time
        None, // headers not loaded at sync time
    );
    for compiled in rules {
        // Shortcut: body/header rules simply can't match here since
        // their features are `None`. Spares a pointless `matches` call.
        match compiled.rule.pattern_type {
            P::BodyContains | P::HeaderContains => continue,
            _ => {}
        }
        if crate::application::spam_rules::matches_compiled(&features, compiled) {
            return Some(compiled.rule.id);
        }
    }
    None
}

/// Persist the `UIDVALIDITY` / `UIDNEXT` the server just reported
/// alongside a fresh `last_sync_ts`. Both the SINCE-window sync and
/// the lazy-recent/older syncs call this right after `SELECT`, so
/// `folders.last_sync_ts` is the source of truth for TTL decisions.
/// Errors are logged but not propagated — a failed state update is
/// annoying (we might re-sync a bit too often), not catastrophic.
async fn update_folder_uid_state(
    db: &DbHandle,
    folder_id: FolderId,
    mailbox: &async_imap::types::Mailbox,
) {
    if let (Some(uv), Some(un)) = (mailbox.uid_validity, mailbox.uid_next) {
        let (tx, rx) = oneshot::channel();
        if db
            .writer
            .send(WriteCmd::UpdateFolderSyncState {
                folder_id,
                uid_validity: uv,
                uid_next: un,
                last_sync_ts: Utc::now(),
                ack: tx,
            })
            .await
            .is_err()
        {
            tracing::warn!("writer closed during folder sync-state update");
            return;
        }
        let _ = rx.await;
    }
}

/// Drop local envelopes for UIDs the server no longer has in this
/// folder. Closes the gap between "we cached this UID once" and
/// "the server moved/expunged it without us being there to see the
/// transition" — the classic Sieve/spam-filter case where a mail
/// lands in INBOX, we sync it, and then the server quietly relocates
/// it before we look again. Without this pass the row sticks around
/// in the local DB forever, so the inbox list keeps showing it and
/// the body fetch fails with `envelope not found`.
///
/// `server_uids` MUST be the complete current UID set for the folder
/// (i.e. result of `UID SEARCH ALL`). Passing a windowed set like
/// `UID SEARCH SINCE 30d` would falsely prune everything older than
/// the window.
async fn reconcile_folder_uids(
    db: &DbHandle,
    folder_id: FolderId,
    folder_name: &str,
    server_uids: &HashSet<u32>,
) -> Result<u32, String> {
    let local: Vec<u32> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_envelope_uids(&conn, &folder_id).map_err(|e| e.to_string())?
    };
    let stale: Vec<u32> = local
        .into_iter()
        .filter(|uid| !server_uids.contains(uid))
        .collect();

    if stale.is_empty() {
        return Ok(0);
    }

    let pruned = stale.len() as u32;
    tracing::info!(
        folder = %folder_name,
        pruned,
        "reconcile: pruning UIDs that vanished server-side"
    );

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteEnvelopes {
            folder_id,
            imap_uids: stale,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("delete_envelopes: {e}"))?;
    Ok(pruned)
}

/// If the server reports a different `UIDVALIDITY` than we have
/// cached, every UID we hold for this folder is meaningless — they
/// reference a previous incarnation of the mailbox. Per RFC 3501,
/// the only safe response is to drop everything and re-sync.
///
/// Returns `Ok(true)` when a purge happened (caller can choose to
/// log/announce), `Ok(false)` for "no change, carry on". Errors out
/// of caution if the writer is unhealthy; sync should retry next
/// tick rather than push fresh rows next to stale ones.
async fn handle_uidvalidity_change(
    db: &DbHandle,
    folder_id: FolderId,
    folder_name: &str,
    server_uid_validity: Option<u32>,
) -> Result<bool, String> {
    let server_uv = match server_uid_validity {
        Some(v) if v > 0 => v,
        _ => return Ok(false),
    };
    let prior = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_folder_uid_validity(&conn, &folder_id).map_err(|e| e.to_string())?
    };
    let prior = match prior {
        Some(v) => v,
        None => return Ok(false), // first SELECT — nothing to invalidate
    };
    if prior == server_uv {
        return Ok(false);
    }

    tracing::warn!(
        folder = %folder_name,
        prior_uid_validity = prior,
        new_uid_validity = server_uv,
        "UIDVALIDITY changed — purging cached envelopes for this folder"
    );

    // Drop every local UID — fastest path is enumerate + DeleteEnvelopes,
    // which also clears FTS rows. Keeping the folder row itself avoids
    // having to re-create it (and its sync_enabled flag) right away.
    let local: Vec<u32> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_envelope_uids(&conn, &folder_id).map_err(|e| e.to_string())?
    };
    if local.is_empty() {
        return Ok(true);
    }
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteEnvelopes {
            folder_id,
            imap_uids: local,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("delete_envelopes: {e}"))?;
    Ok(true)
}

async fn ensure_folder(
    db: &DbHandle,
    account_id: &AccountId,
    name: &str,
) -> Result<FolderId, String> {
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::EnsureFolder {
            account_id: *account_id,
            name: name.to_string(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("ensure_folder: {e}"))
}

/// Walk the server's full mailbox hierarchy once and register every
/// selectable folder in the local `folders` table. Follows the same
/// `LIST "" "*"` pattern as [`imap_client::discover_folders`] — the
/// wildcard matches across nesting depths (Dovecot's `INBOX.Foo.Bar`,
/// Gmail's `[Gmail]/Foo`, plain top-level, all included). `\NoSelect`
/// entries are skipped because they're pure namespace containers and
/// would blow up on `SELECT`.
///
/// New folders land with `sync_enabled = 1` (migration default). Already-
/// known folders keep their existing setting — this pass is strictly
/// additive, no UPDATE happens here.
async fn discover_and_register_folders(
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    db: &DbHandle,
    account_id: &AccountId,
) -> Result<(), String> {
    use async_imap::imap_proto::types::NameAttribute;

    let mut stream = session
        .list(Some(""), Some("*"))
        .await
        .map_err(|e| format!("LIST: {e}"))?;

    let mut names: Vec<String> = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(entry) => {
                let no_select = entry
                    .attributes()
                    .iter()
                    .any(|a| matches!(a, NameAttribute::NoSelect));
                if no_select {
                    continue;
                }
                names.push(entry.name().to_string());
            }
            Err(e) => {
                // Single malformed LIST line shouldn't kill discovery —
                // keep going and take what we got.
                tracing::warn!("LIST parse error: {e}");
            }
        }
    }
    drop(stream);

    tracing::info!(count = names.len(), "folder discovery: LIST done");

    for name in names {
        if let Err(e) = ensure_folder(db, account_id, &name).await {
            // One ensure_folder failing means the writer is unhappy;
            // log it but keep trying the rest. A stuck writer will
            // surface on the next envelope upsert anyway.
            tracing::warn!(folder = %name, error = %e, "folder discovery: ensure_folder failed");
        }
    }
    Ok(())
}

fn map_fetch(
    fetch: &async_imap::types::Fetch,
    account_id: AccountId,
    folder_id: FolderId,
) -> Option<Envelope> {
    let uid = fetch.uid?;
    // Rohe Header-Bytes aus BODY.PEEK[HEADER] — kein ENVELOPE-Parse,
    // also kein imap-proto-Poison-Risiko. mail-parser frisst alles,
    // auch die Spam-Exoten, an denen imap-proto stirbt.
    let header_bytes = fetch.header()?;
    let msg = mail_parser::MessageParser::default().parse(header_bytes)?;

    let subject = msg.subject().unwrap_or("").to_string();
    let message_id_header = msg
        .message_id()
        .map(|s| strip_angle_brackets(s.to_string()))
        .filter(|s| !s.is_empty());

    let from = msg.from().map(addresses_from).unwrap_or_default();
    let to = msg.to().map(addresses_from).unwrap_or_default();
    let cc = msg.cc().map(addresses_from).unwrap_or_default();

    // In-Reply-To + References in genau der Reihenfolge sammeln, wie der
    // JWZ-Threader sie erwartet (In-Reply-To hat Vorrang, dann die
    // Referenz-Kette).
    let mut references: Vec<String> = Vec::new();
    if let Some(list) = msg.in_reply_to().as_text_list() {
        for s in list {
            let id = strip_angle_brackets(s.to_string());
            if !id.is_empty() {
                references.push(id);
            }
        }
    }
    if let Some(list) = msg.references().as_text_list() {
        for s in list {
            let id = strip_angle_brackets(s.to_string());
            if !id.is_empty() && !references.contains(&id) {
                references.push(id);
            }
        }
    }

    let date = fetch
        .internal_date()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            msg.date()
                .and_then(|dt| Utc.timestamp_opt(dt.to_timestamp(), 0).single())
        })
        .unwrap_or_else(Utc::now);

    let flags = fetch_flags(fetch);
    let size_bytes = fetch.size.unwrap_or(0);

    let has_attachments = has_attachment_heuristic(&msg);

    Some(Envelope {
        id: MessageId(Uuid::new_v4()),
        account_id,
        folder_id,
        imap_uid: uid,
        message_id_header,
        from,
        to,
        cc,
        subject,
        date,
        flags,
        references,
        size_bytes,
        body_cached: false,
        has_attachments,
    })
}

/// Flach-gemachte Adressliste aus einem `mail_parser::Address`
/// (das ist *nicht* unser Domain-`Address`!). Groups werden
/// flachgeklopft — für den Unified-Inbox-View irrelevant, welche
/// Group-Labels eine Adresse hatte.
fn addresses_from(addr: &mail_parser::Address) -> Vec<Address> {
    addr.iter()
        .filter_map(|a| {
            let email = a.address().map(str::to_string).unwrap_or_default();
            if email.is_empty() {
                return None;
            }
            let name = a
                .name()
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            Some(Address { name, email })
        })
        .collect()
}

fn fetch_flags(fetch: &async_imap::types::Fetch) -> Flags {
    use async_imap::types::Flag;
    let mut f = Flags::default();
    for flag in fetch.flags() {
        match flag {
            Flag::Seen => f.seen = true,
            Flag::Answered => f.answered = true,
            Flag::Flagged => f.flagged = true,
            Flag::Draft => f.draft = true,
            Flag::Deleted => f.deleted = true,
            Flag::Custom(kw) => {
                // `$Forwarded` keyword: set by clients when the user has
                // forwarded the message. Match case-insensitively since the
                // exact casing varies per server.
                if kw.eq_ignore_ascii_case("$Forwarded") {
                    f.forwarded = true;
                } else if kw.eq_ignore_ascii_case("$Junk") {
                    // `$Junk` (RFC 5788): mail is user-/server-classified
                    // as spam. The filter-builder treats `$Junk` in any
                    // folder other than the configured spam folder as a
                    // correction signal.
                    f.junk = true;
                }
            }
            _ => {}
        }
    }
    f
}

fn strip_angle_brackets(s: String) -> String {
    let trimmed = s.trim();
    if trimmed.starts_with('<') && trimmed.ends_with('>') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Sync-time attachment heuristic from the top-level Content-Type
/// header alone: `multipart/mixed` is the standard MIME wrapper for
/// "main content + attachments" — we can't see the inner parts from
/// BODY.PEEK[HEADER], but flagging the obvious case is enough for the
/// inbox-list paperclip to land within milliseconds of sync. The
/// body-fetch path overwrites this with the authoritative answer
/// once the MIME tree is decoded (see `application::body::store`).
///
/// Public-via-mod so the tests further down can drive it with parsed
/// header bytes; not used outside this module.
fn has_attachment_heuristic(msg: &mail_parser::Message) -> bool {
    msg.content_type()
        .map(|ct| {
            ct.ctype().eq_ignore_ascii_case("multipart")
                && ct
                    .subtype()
                    .map(|s| s.eq_ignore_ascii_case("mixed"))
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

// ─── Tests ────────────────────────────────────────────────────────────────────
//
// `map_fetch` selbst verlangt einen `async_imap::types::Fetch`, den man
// nicht trivial in einem Unit-Test konstruiert. Wir testen daher die
// pure Helfer-Funktionen, die map_fetch komponiert: header-parsing-
// abhängige Logik (`has_attachment_heuristic`, `addresses_from`) und
// rein syntaktische Säuberung (`strip_angle_brackets`).
#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::MessageParser;

    fn parse(headers: &[u8]) -> mail_parser::Message<'_> {
        MessageParser::default()
            .parse(headers)
            .expect("test header should parse")
    }

    // ─── strip_angle_brackets ────────────────────────────────────────

    #[test]
    fn strip_angle_brackets_removes_paired_brackets() {
        assert_eq!(
            strip_angle_brackets("<msg-1@example.com>".into()),
            "msg-1@example.com"
        );
    }

    #[test]
    fn strip_angle_brackets_trims_whitespace_first() {
        // Server-output hat manchmal Whitespace außenrum (z.B. nach
        // unfaltbaren Headers). Trim muss vor dem Bracket-Check kommen.
        assert_eq!(
            strip_angle_brackets("  <id@x.tld>  ".into()),
            "id@x.tld"
        );
    }

    #[test]
    fn strip_angle_brackets_passes_unbracketed_through() {
        assert_eq!(
            strip_angle_brackets("naked-id@x.tld".into()),
            "naked-id@x.tld"
        );
    }

    #[test]
    fn strip_angle_brackets_leaves_only_one_bracket_untouched() {
        // Asymmetrisches `<...` ohne abschließendes `>` darf NICHT
        // angeschnitten werden — Server-Bug nicht doppeln.
        assert_eq!(strip_angle_brackets("<broken".into()), "<broken");
        assert_eq!(strip_angle_brackets("broken>".into()), "broken>");
    }

    #[test]
    fn strip_angle_brackets_handles_minimal_lengths() {
        // "<>" ist Länge 2, sollte zu "" werden, nicht panicken.
        assert_eq!(strip_angle_brackets("<>".into()), "");
        // Einzel-Char: passes through, kein crash auf bytes[..0].
        assert_eq!(strip_angle_brackets("<".into()), "<");
        assert_eq!(strip_angle_brackets("".into()), "");
    }

    // ─── addresses_from ──────────────────────────────────────────────

    #[test]
    fn addresses_from_pulls_email_and_name() {
        let msg = parse(b"From: Alice Example <alice@example.com>\r\n\r\n");
        let addrs = addresses_from(msg.from().expect("from header"));
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].email, "alice@example.com");
        assert_eq!(addrs[0].name.as_deref(), Some("Alice Example"));
    }

    #[test]
    fn addresses_from_drops_entries_without_email() {
        // mail_parser tolerates malformed entries — we filter them out
        // so empty-email rows never reach the envelopes table.
        let msg = parse(
            b"From: \"No Email\", real@x.tld\r\n\r\n",
        );
        let addrs = addresses_from(msg.from().expect("from header"));
        assert!(
            addrs.iter().all(|a| !a.email.is_empty()),
            "no empty-email entries should survive: {addrs:?}"
        );
    }

    #[test]
    fn addresses_from_handles_multiple_recipients() {
        let msg = parse(
            b"To: Alice <a@x.tld>, Bob <b@y.tld>, c@z.tld\r\n\r\n",
        );
        let addrs = addresses_from(msg.to().expect("to header"));
        assert_eq!(addrs.len(), 3);
        let emails: Vec<&str> = addrs.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(emails, vec!["a@x.tld", "b@y.tld", "c@z.tld"]);
    }

    #[test]
    fn addresses_from_drops_empty_name_to_none() {
        // "<a@x.tld>" hat kein Display-Name → name soll None sein,
        // nicht Some("").
        let msg = parse(b"From: <a@x.tld>\r\n\r\n");
        let addrs = addresses_from(msg.from().expect("from header"));
        assert_eq!(addrs.len(), 1);
        assert!(addrs[0].name.is_none(), "empty name must not be Some(\"\")");
    }

    // ─── has_attachment_heuristic ────────────────────────────────────

    #[test]
    fn heuristic_flags_multipart_mixed() {
        let msg = parse(
            b"Content-Type: multipart/mixed; boundary=\"x\"\r\n\r\n",
        );
        assert!(has_attachment_heuristic(&msg));
    }

    #[test]
    fn heuristic_does_not_flag_multipart_alternative() {
        // multipart/alternative ist nur Plain+HTML-Variante, KEIN
        // Anhang-Indikator.
        let msg = parse(
            b"Content-Type: multipart/alternative; boundary=\"x\"\r\n\r\n",
        );
        assert!(!has_attachment_heuristic(&msg));
    }

    #[test]
    fn heuristic_does_not_flag_plain_text() {
        let msg = parse(b"Content-Type: text/plain; charset=utf-8\r\n\r\n");
        assert!(!has_attachment_heuristic(&msg));
    }

    #[test]
    fn heuristic_does_not_flag_text_html() {
        let msg = parse(b"Content-Type: text/html; charset=utf-8\r\n\r\n");
        assert!(!has_attachment_heuristic(&msg));
    }

    #[test]
    fn heuristic_is_case_insensitive() {
        // Spec sagt MIME-Typen sind case-insensitive — wir müssen
        // `MULTIPART/Mixed` genauso erkennen wie `multipart/mixed`.
        let msg = parse(
            b"Content-Type: MULTIPART/Mixed; boundary=\"x\"\r\n\r\n",
        );
        assert!(has_attachment_heuristic(&msg));
    }

    #[test]
    fn heuristic_handles_missing_content_type() {
        // Mail-Header ohne Content-Type (sehr alte Server) → nicht
        // panicken, einfach "kein Anhang".
        let msg = parse(b"Subject: ohne content-type\r\n\r\n");
        assert!(!has_attachment_heuristic(&msg));
    }
}
