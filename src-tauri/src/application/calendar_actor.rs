// IMAP-IDLE calendar actor (Phase 2.5).
//
// Runs a long-lived IMAP session against the configured calendar folder
// and triggers a sync whenever the server pushes a notification. Per-app
// instance (one actor at most), spawned at boot when CalendarConfig is
// `enabled` and `idle_enabled`, restarted on config change via the
// command channel held in `AppState::calendar_actor_tx`.
//
// The actor lives in the Mail layer (next to `application::actor`, which
// runs IDLE on INBOX) because it owns an async-imap Session — the
// timeprotocol module's only Mail-layer-IO surface remains
// `application::calendar_imap`. The actor calls into
// `timeprotocol::sync::run_with_lock` to do the actual sync work; that
// crossing is the inverse of the boundary rule but acceptable because
// the actor is a Mail-layer adapter triggering a calendar-side action,
// not the other way around.

use std::time::Duration;

use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

use crate::domain::account::AccountId;
use crate::infrastructure::imap_client;
use crate::infrastructure::queries;
use crate::state::AppState;

/// IDLE-RFC's de-facto safe ceiling: most servers drop IDLE sessions
/// after 30 min, we refresh ~every 28 min to stay conservatively under.
const IDLE_REFRESH: Duration = Duration::from_secs(28 * 60);

const RECONNECT_INITIAL: Duration = Duration::from_secs(5);
const RECONNECT_MAX: Duration = Duration::from_secs(300);

const KEYRING_SERVICE: &str = "crystalmail";

#[derive(Debug)]
pub enum ActorCmd {
    /// Graceful shutdown — close the IDLE session and exit. Config-
    /// change handling (`cal_set_config`) sends Shutdown to the old
    /// actor and then spawns a fresh one with the new params; there is
    /// no in-process restart message because the actor's params are
    /// immutable after spawn.
    Shutdown,
}

type ImapSession = async_imap::Session<
    tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
>;

/// Spawn the calendar-IDLE actor as a long-lived tokio task. Returns the
/// channel sender so the caller can stash it in
/// `AppState::calendar_actor_tx` and send `Shutdown` later.
pub fn spawn(app: AppHandle, account_id: AccountId, folder: String) -> mpsc::Sender<ActorCmd> {
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(run(app, account_id, folder, rx));
    tx
}

/// Reconcile the IMAP-IDLE actor against the current `CalendarConfig`.
/// Idempotent: shuts down the previous actor (if any) and spawns a new
/// one when the config says we should be running. Called from the boot
/// hook (`main.rs` setup) and from `cal_set_config` after a config
/// change. Always shutdown+respawn rather than try to determine whether
/// the running actor's params still match — small overhead, no risk of
/// drift.
pub async fn reconcile(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cfg = {
        let guard = state.calendar_config.lock().unwrap();
        guard.clone()
    };
    let should_run = cfg.enabled && cfg.idle_enabled && cfg.account_id.is_some();

    let mut tx_guard = state.calendar_actor_tx.lock().await;
    if let Some(old_tx) = tx_guard.take() {
        // Best-effort shutdown: if the receiver already dropped (actor
        // exited via disconnect path), we just ignore the send error.
        let _ = old_tx.send(ActorCmd::Shutdown).await;
    }

    if should_run {
        let account_id = cfg.account_id.expect("checked above");
        let folder = cfg.folder_path.clone();
        let new_tx = spawn(app.clone(), account_id, folder);
        *tx_guard = Some(new_tx);
        tracing::info!("calendar actor: spawned");
    } else {
        tracing::info!("calendar actor: not running (sync disabled or idle disabled or no account)");
    }
}

async fn run(
    app: AppHandle,
    account_id: AccountId,
    folder: String,
    mut cmd_rx: mpsc::Receiver<ActorCmd>,
) {
    let mut backoff = RECONNECT_INITIAL;
    loop {
        match try_idle_session(&app, &account_id, &folder, &mut cmd_rx).await {
            ActorOutcome::Shutdown => {
                tracing::info!("calendar actor: shutdown");
                return;
            }
            ActorOutcome::Disconnected(reason) => {
                tracing::warn!(
                    reason = %reason,
                    backoff_secs = backoff.as_secs(),
                    "calendar actor: disconnected, reconnecting after backoff"
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(ActorCmd::Shutdown) | None => {
                                tracing::info!("calendar actor: shutdown during backoff");
                                return;
                            }
                        }
                    }
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
        }
    }
}

enum ActorOutcome {
    Shutdown,
    Disconnected(String),
}

async fn try_idle_session(
    app: &AppHandle,
    account_id: &AccountId,
    folder: &str,
    cmd_rx: &mut mpsc::Receiver<ActorCmd>,
) -> ActorOutcome {
    let state = app.state::<AppState>();
    let db = match state.db.get() {
        Some(d) => d,
        None => return ActorOutcome::Disconnected("database not ready".into()),
    };

    // Resolve account + password.
    let (account, password) = {
        let conn = match db.reads.get() {
            Ok(c) => c,
            Err(e) => return ActorOutcome::Disconnected(format!("db pool: {e}")),
        };
        let account = match queries::get_account(&conn, account_id) {
            Ok(Some(a)) => a,
            Ok(None) => return ActorOutcome::Disconnected("account not found".into()),
            Err(e) => return ActorOutcome::Disconnected(format!("get_account: {e}")),
        };
        let entry_name = format!("imap::{}", account.id.0);
        let password = match keyring::Entry::new(KEYRING_SERVICE, &entry_name)
            .and_then(|e| e.get_password())
        {
            Ok(p) => p,
            Err(e) => {
                return ActorOutcome::Disconnected(format!("keyring: {e}"));
            }
        };
        (account, password)
    };

    let client = match imap_client::connect_tls(&account.imap_host, account.imap_port).await {
        Ok(c) => c,
        Err(e) => return ActorOutcome::Disconnected(format!("connect: {e}")),
    };
    let session = match client.login(&account.address, &password).await {
        Ok(s) => s,
        Err((e, _)) => return ActorOutcome::Disconnected(format!("login: {e}")),
    };

    // Trigger an initial sync as soon as we connect — the user may have
    // gotten remote changes while CrystalMail was offline. The lock-based
    // run_with_lock handles single-flighting against any concurrent
    // periodic / mutation trigger.
    let app_for_initial = app.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::timeprotocol::sync::run_with_lock(&app_for_initial).await {
            tracing::warn!(error = %e, "calendar actor: initial sync failed");
        }
    });

    let outcome = drive_idle_loop(app, session, folder, cmd_rx).await;
    outcome
}

async fn drive_idle_loop(
    app: &AppHandle,
    mut session: ImapSession,
    folder: &str,
    cmd_rx: &mut mpsc::Receiver<ActorCmd>,
) -> ActorOutcome {
    if let Err(e) = session.select(folder).await {
        let _ = session.logout().await;
        return ActorOutcome::Disconnected(format!("SELECT {folder}: {e}"));
    }

    loop {
        let mut handle = session.idle();
        if let Err(e) = handle.init().await {
            return ActorOutcome::Disconnected(format!("IDLE init: {e}"));
        }

        // Three wake-up sources: server push / IDLE-refresh-timeout
        // (`wait_fut` resolves), Cmd (Shutdown), or — in future versions
        // — a periodic poll tick. The borrow-of-handle for `wait_fut`
        // must end before we can `handle.done()` (consume). The inner
        // block enforces that lifetime cleanly. Same shape as the
        // existing `application::actor` IDLE loop.
        enum Wakeup {
            ServerOrTimeout(
                Result<
                    async_imap::extensions::idle::IdleResponse,
                    async_imap::error::Error,
                >,
            ),
            Cmd(Option<ActorCmd>),
        }

        let wakeup: Wakeup = {
            let (wait_fut, stop_source) = handle.wait_with_timeout(IDLE_REFRESH);
            tokio::pin!(wait_fut);
            let w = tokio::select! {
                res = &mut wait_fut => Wakeup::ServerOrTimeout(res),
                cmd = cmd_rx.recv() => Wakeup::Cmd(cmd),
            };
            let interrupted_via_cmd = matches!(w, Wakeup::Cmd(_));
            drop(stop_source);
            if interrupted_via_cmd {
                let _ = (&mut wait_fut).await;
            }
            w
        };

        session = match handle.done().await {
            Ok(s) => s,
            Err(e) => return ActorOutcome::Disconnected(format!("IDLE done: {e}")),
        };

        match wakeup {
            Wakeup::ServerOrTimeout(Ok(
                async_imap::extensions::idle::IdleResponse::Timeout,
            )) => {
                // 28-min refresh: server didn't push anything. Just
                // re-issue IDLE; no sync needed.
                continue;
            }
            Wakeup::ServerOrTimeout(Ok(_)) => {
                // Server pushed (NewData / something changed). Trigger
                // a single-flighted sync via run_with_lock.
                let app_clone = app.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::timeprotocol::sync::run_with_lock(&app_clone).await
                    {
                        tracing::warn!(
                            error = %e,
                            "calendar actor: sync after IDLE push failed"
                        );
                    }
                });
            }
            Wakeup::ServerOrTimeout(Err(e)) => {
                let _ = session.logout().await;
                return ActorOutcome::Disconnected(format!("IDLE wait: {e}"));
            }
            Wakeup::Cmd(Some(ActorCmd::Shutdown)) | Wakeup::Cmd(None) => {
                let _ = session.logout().await;
                return ActorOutcome::Shutdown;
            }
        }
    }
}
