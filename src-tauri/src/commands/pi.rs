// Tauri commands for the pi RPC subprocess. Mirrors the mila pattern:
//   * One persistent `pi --mode rpc` process per app run, respawned when
//     the user-visible PiConfig changes (detected via fingerprint).
//   * Streaming text deltas come back via the `chat-stream` Tauri event,
//     which the terminal panel subscribes to.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::llm::pi_rpc::{self, PiRpc};
use crate::state::{AppState, PiConfig};

const PI_CONFIG_FILE: &str = "pi_config.json";

/// Sentinel error string returned by every AI entry point (chat, spam
/// analysis, workflow training) when the user has flipped the master
/// AI kill-switch off. The frontend matches on this exact string —
/// don't translate or decorate it. Show your own friendly notice in
/// the UI based on the match.
pub const AI_DISABLED_ERR: &str = "ai_disabled";

/// Cheap helper: read the current enabled flag from state. Held for
/// the duration of a sync mutex lock; no async work allowed inside.
pub fn ai_enabled(state: &AppState) -> bool {
    let guard = state.pi_config.lock().unwrap();
    guard.enabled
}

fn config_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(PI_CONFIG_FILE))
}

/// Load the persisted pi config if present. Unreadable / corrupt files are
/// treated as "no config yet" — the caller falls back to defaults.
pub fn load_persisted(app: &AppHandle) -> Option<PiConfig> {
    let path = config_path(app)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<PiConfig>(&bytes).ok()
}

fn save_persisted(app: &AppHandle, cfg: &PiConfig) -> Result<(), String> {
    let path = config_path(app)
        .ok_or_else(|| "app_data_dir nicht verfügbar".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json =
        serde_json::to_vec_pretty(cfg).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write: {e}"))
}

#[tauri::command]
pub async fn get_pi_config(app: AppHandle) -> PiConfig {
    let state = app.state::<AppState>();
    let guard = state.pi_config.lock().unwrap();
    guard.clone()
}

/// Replace the pi configuration. Drops any existing subprocess so the next
/// `pi_ask` spawns a fresh one with the new settings — much simpler than
/// trying to reconfigure a running agent. Also persists the config to disk
/// so it survives an app restart.
#[tauri::command]
pub async fn set_pi_config(app: AppHandle, config: PiConfig) -> Result<(), String> {
    let state = app.state::<AppState>();
    {
        let mut guard = state.pi_config.lock().unwrap();
        *guard = config.clone();
    }
    // Best-effort persist — don't fail the call on a disk error, the
    // in-memory state is already up to date.
    if let Err(e) = save_persisted(&app, &config) {
        tracing::warn!(error = %e, "persisting pi_config failed");
    }
    let mut rpc_guard = state.pi_rpc.lock().await;
    *rpc_guard = None;
    Ok(())
}

/// Reset the pi subprocess (e.g. after a bin-path change). Any currently
/// running turn will finish; the next ask will respawn.
#[tauri::command]
pub async fn pi_reset(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut guard = state.pi_rpc.lock().await;
    *guard = None;
    Ok(())
}

/// Submit a prompt to pi. Ensures the subprocess is alive (spawning /
/// respawning as needed based on the config fingerprint) and waits for the
/// turn to complete. Streaming deltas arrive on the `chat-stream` event.
///
/// If the user has configured a `promptPrefix` in settings, it is prepended
/// once, separated by a blank line. The prefix is applied here (rather than
/// the frontend) so every entry point to pi automatically picks it up.
#[tauri::command]
pub async fn pi_ask(app: AppHandle, message: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    if !ai_enabled(state.inner()) {
        return Err(AI_DISABLED_ERR.to_string());
    }
    let prefix = {
        let cfg = state.pi_config.lock().unwrap();
        cfg.prompt_prefix.clone()
    };
    let full = if prefix.trim().is_empty() {
        message
    } else {
        format!("{}\n\n{message}", prefix.trim())
    };
    let rpc = ensure_rpc(&app, state.inner()).await?;
    rpc.prompt(full).await
}

/// Read the master AI kill-switch state. Frontend polls this on
/// startup + listens to `set_ai_enabled` to keep its indicator in
/// sync. Cheap — just a mutex read.
#[tauri::command]
pub async fn get_ai_enabled(app: AppHandle) -> bool {
    let state = app.state::<AppState>();
    ai_enabled(state.inner())
}

/// Flip the master AI kill-switch. Persists the whole `PiConfig` so
/// the change survives an app restart. Unlike `set_pi_config` we
/// deliberately don't respawn the pi subprocess: a flick of the
/// switch shouldn't kill an interactive chat, and turning AI off
/// while a pi process is alive just means the next call will refuse
/// — the existing process is harmless until then. When the user
/// flips back on, the existing process is reused (cheap), or a new
/// one spawns on next ask.
#[tauri::command]
pub async fn set_ai_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let state = app.state::<AppState>();
    let new_cfg = {
        let mut guard = state.pi_config.lock().unwrap();
        guard.enabled = enabled;
        guard.clone()
    };
    if let Err(e) = save_persisted(&app, &new_cfg) {
        tracing::warn!(error = %e, "persisting ai_enabled flip failed");
    }
    Ok(())
}

async fn ensure_rpc(
    app: &AppHandle,
    state: &AppState,
) -> Result<Arc<PiRpc>, String> {
    let cfg = {
        let guard = state.pi_config.lock().unwrap();
        guard.clone()
    };
    let fp = pi_rpc::fingerprint(&cfg);

    let mut guard = state.pi_rpc.lock().await;
    if let Some(rpc) = guard.as_ref() {
        if rpc.fingerprint() == fp {
            return Ok(rpc.clone());
        }
    }
    let fresh = PiRpc::spawn(app.clone(), &cfg).await?;
    *guard = Some(fresh.clone());
    Ok(fresh)
}
