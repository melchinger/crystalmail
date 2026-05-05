pub mod accounts;
pub mod backup;
pub mod calendar;
pub mod contacts;
pub mod draft_import;
pub mod folders;
pub mod mail;
pub mod taskbar;
pub mod pi;
pub mod pi_models;
pub mod spam;
pub mod workflows;

use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use crate::infrastructure::db::{DbError, WriteCmd};
use crate::state::AppState;

#[tauri::command]
pub fn ping() -> &'static str {
    "pong"
}

/// End-to-end liveness probe — read-side via `PRAGMA user_version`,
/// write-side via `WriteCmd::Ping` (a no-op the writer just acks).
/// Was useful during setup; today the UI uses it as a smoke test
/// when the app boots before the first real data lands.
///
/// History: an earlier version sent a deliberately-FK-violating
/// `UpsertFolder` with a nil `account_id` to "test" the writer.
/// The constraint correctly rejected it, but each call still spent
/// a write transaction and dropped a SQLite error into the trace
/// log — high-rate frontend polling during dev added avoidable noise.
/// `WriteCmd::Ping` is the right shape: prove the channel is alive
/// without touching the DB.
#[tauri::command]
pub async fn db_ping(app: AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let db = state
        .db
        .get()
        .ok_or_else(|| "database not yet initialized".to_string())?;

    // Read-side: schema version check confirms the read pool + DB are alive.
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|e| e.to_string())?;

    // Write-side: ping the writer actor without touching SQL.
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::Ping { ack: tx })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e: DbError| format!("writer ping failed: {e}"))?;

    Ok(format!("db ready — schema v{version}"))
}
