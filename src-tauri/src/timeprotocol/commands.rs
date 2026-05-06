// Tauri command adapters for the Calendar bounded context.
//
// Phase 0 commands (`ics_parse_attachment`, `ics_build_invitation_reply`)
// remain storage-less: parse a `text/calendar` attachment of an open
// message and prepare an RFC 5546 REPLY the user can send back to the
// organizer. The reply ICS lands in a temp file purely so the existing
// Compose attachment pipeline (which expects a path on disk) can pick it
// up; the SMTP path then recognises it via the iMIP detection in
// `application::smtp` and emits a multipart/alternative message.
//
// Phase 1+ commands (`cal_*`) operate on the local commitment store
// introduced in `super::store`. They never touch Mail-domain data
// directly — the inbound read of an ICS attachment is the only Mail-layer
// access in this whole module.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use super::domain::{
    Commitment, CommitmentDraft, CommitmentStatus, InvitationResponse, ParsedIcsEvent,
};
use super::{ics, store};
use crate::application::attachments;
use crate::domain::message::MessageId;
use crate::infrastructure::db::WriteCmd;
use crate::state::{AppState, CalendarConfig};

// ─── Phase 2.1: persisted calendar IMAP-sync config ──────────────────────

const CALENDAR_CONFIG_FILE: &str = "calendar_config.json";

fn calendar_config_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join(CALENDAR_CONFIG_FILE))
}

/// Load the persisted calendar config from disk. Called from the Tauri
/// `setup` hook in `main.rs`. Missing or unreadable file → defaults
/// (sync disabled, Phase-1 fallback). Same shape as
/// `commands::workflows::load_persisted` and `commands::pi::load_persisted`.
pub fn load_persisted(app: &AppHandle) -> Option<CalendarConfig> {
    let path = calendar_config_path(app)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<CalendarConfig>(&bytes).ok()
}

fn save_persisted(app: &AppHandle, cfg: &CalendarConfig) -> Result<(), String> {
    let path = calendar_config_path(app)
        .ok_or_else(|| "app_data_dir nicht verfügbar".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write: {e}"))
}

/// Read-side: the in-memory CalendarConfig. Frontend Settings panel uses
/// this to populate the calendar tab.
#[tauri::command]
pub async fn cal_get_config(app: AppHandle) -> CalendarConfig {
    let state = app.state::<AppState>();
    let guard = state.calendar_config.lock().unwrap();
    guard.clone()
}

/// Write-side: replace the in-memory config and persist to disk. After
/// the swap, reconcile the IDLE actor — start/stop/restart it to match
/// the new config (account or folder may have changed). Persist failures
/// are logged but not surfaced — the in-memory write succeeded.
#[tauri::command]
pub async fn cal_set_config(
    app: AppHandle,
    config: CalendarConfig,
) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        let mut guard = state.calendar_config.lock().unwrap();
        *guard = config.clone();
    }
    if let Err(e) = save_persisted(&app, &config) {
        tracing::warn!(error = %e, "persisting calendar_config failed");
    }
    // Apply the config change to the running IDLE actor (if any). Done
    // unconditionally because any field change (enabled, account_id,
    // folder, idle_enabled) is a reason to restart the actor. The
    // reconcile call is idempotent.
    crate::application::calendar_actor::reconcile(&app).await;
    Ok(())
}

/// Spawn a fire-and-forget background sync if the user has opted in to
/// sync-on-mutation. Called by every successful local CRUD command so
/// the user doesn't have to click the "Sync"-button after every edit.
fn maybe_spawn_mutation_sync(app: &AppHandle, reason: &'static str) {
    let state = app.state::<AppState>();
    let should = {
        let cfg = state.calendar_config.lock().unwrap();
        cfg.enabled && cfg.sync_on_mutation && cfg.account_id.is_some()
    };
    if should {
        super::sync::spawn_background_sync(app, reason);
    }
}

/// One-shot IMAP sync per ADR-0011: ensure-folder, read remote, resolve
/// LWW per UID, diff against local, publish/import as needed, then
/// optionally compact superseded messages into `<folder>/Archive`.
/// Wraps `super::sync::run_with_lock` so concurrent triggers (manual
/// button, periodic timer, IDLE actor, sync-on-mutation) cannot race.
#[tauri::command]
pub async fn cal_sync_imap(app: AppHandle) -> Result<super::sync::SyncReport, String> {
    super::sync::run_with_lock(&app).await
}

/// Parse a single `text/calendar` (or `application/ics`) attachment and
/// return the first VEVENT in a UI-friendly shape. Returns `Ok(None)` when
/// the part exists and is well-formed iCalendar but contains no events.
#[tauri::command]
pub async fn ics_parse_attachment(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
) -> Result<Option<ParsedIcsEvent>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Option<ParsedIcsEvent>, String> {
            let (bytes, _filename, _mime) = attachments::bytes(&db, &message_id, part_idx)?;
            ics::parse(&bytes)
        }
    })
    .await
    .map_err(|e| format!("ics parse task panicked: {e}"))?
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationReplyDraft {
    /// PARTSTAT the user picked. Frontend uses this to format the subject
    /// prefix in its own locale ("Accepted: …" / "Zugesagt: …" / …).
    pub response: InvitationResponse,
    /// Echoed verbatim — frontend renders the subject and the body from it.
    pub event_summary: Option<String>,
    pub event_dtstart: Option<String>,
    /// Where the reply should go. Mailto address from the original ORGANIZER.
    pub recipient_email: String,
    pub recipient_display_name: Option<String>,
    /// On-disk path of the freshly-written REPLY ICS. Drop this into the
    /// outgoing ComposeDraft.attachments and the SMTP path attaches it as
    /// `text/calendar; method=REPLY`.
    pub attachment_path: String,
    pub attachment_filename: String,
    pub attachment_size_bytes: u32,
}

/// Build a REPLY ICS for the parsed VEVENT in the given attachment, write it
/// to a stable per-message temp file, and return the metadata the frontend
/// needs to seed a Compose draft.
#[tauri::command]
pub async fn ics_build_invitation_reply(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
    response: InvitationResponse,
    attendee_email: String,
    attendee_name: Option<String>,
) -> Result<InvitationReplyDraft, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let message_id_str = message_id.0.to_string();

    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<InvitationReplyDraft, String> {
            let (bytes, _filename, _mime) = attachments::bytes(&db, &message_id, part_idx)?;
            let parsed = ics::parse(&bytes)?
                .ok_or_else(|| "ICS attachment contains no event".to_string())?;
            let organizer = parsed
                .organizer
                .as_ref()
                .ok_or_else(|| "invitation has no ORGANIZER — cannot reply".to_string())?
                .clone();

            let reply_text = ics::build_reply(
                &parsed,
                response,
                &attendee_email,
                attendee_name.as_deref(),
            );
            let reply_bytes = reply_text.into_bytes();
            let size = reply_bytes.len() as u32;

            // Stable temp location, scoped per message + attachment + chosen
            // response. Re-clicking the same button overwrites idempotently;
            // switching from Accepted → Declined writes a sibling file rather
            // than mutating the previous one in case Compose is still open.
            let dir = std::env::temp_dir()
                .join("crystalmail")
                .join("ics-reply")
                .join(&message_id_str);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("create reply temp dir: {e}"))?;
            let filename = format!(
                "reply-{}-{}.ics",
                part_idx,
                response_filename_suffix(response)
            );
            let path = dir.join(&filename);
            std::fs::write(&path, &reply_bytes)
                .map_err(|e| format!("write reply ics: {e}"))?;

            Ok(InvitationReplyDraft {
                response,
                event_summary: parsed.summary,
                event_dtstart: parsed.dtstart,
                recipient_email: organizer.email,
                recipient_display_name: organizer.display_name,
                attachment_path: path.to_string_lossy().into_owned(),
                attachment_filename: filename,
                attachment_size_bytes: size,
            })
        }
    })
    .await
    .map_err(|e| format!("ics reply task panicked: {e}"))?
}

fn response_filename_suffix(r: InvitationResponse) -> &'static str {
    match r {
        InvitationResponse::Accepted => "accepted",
        InvitationResponse::Tentative => "tentative",
        InvitationResponse::Declined => "declined",
    }
}

// ─── Phase 1: local commitment store ──────────────────────────────────────

/// List commitments overlapping the half-open `[from, to)` interval. Both
/// bounds are RFC 3339 strings with offset (matching what's stored).
#[tauri::command]
pub async fn cal_list_in_range(
    app: AppHandle,
    from: String,
    to: String,
) -> Result<Vec<Commitment>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Vec<Commitment>, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            store::list_in_range(&conn, &from, &to).map_err(|e| e.to_string())
        }
    })
    .await
    .map_err(|e| format!("cal_list task panicked: {e}"))?
}

/// Fetch a single commitment with its attendees attached.
#[tauri::command]
pub async fn cal_get(
    app: AppHandle,
    id: String,
) -> Result<Option<Commitment>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Option<Commitment>, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            store::get_with_attendees(&conn, &id).map_err(|e| e.to_string())
        }
    })
    .await
    .map_err(|e| format!("cal_get task panicked: {e}"))?
}

/// Create a brand-new commitment from a frontend draft form. Generates a
/// fresh UUID for both the local id and the RFC 5545 UID.
#[tauri::command]
pub async fn cal_create(
    app: AppHandle,
    draft: CommitmentDraft,
) -> Result<Commitment, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let mut commitment = ics::manual_commitment(
        draft.summary,
        draft.description,
        draft.location,
        draft.start_at,
        draft.end_at,
        draft.organizer,
        draft.attendees,
    );
    if let Some(tzid) = draft.original_tzid {
        commitment.original_tzid = Some(tzid);
    }

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpsertCommitment {
            commitment: commitment.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db upsert: {e}"))?;
    maybe_spawn_mutation_sync(&app, "cal_create");
    Ok(commitment)
}

/// Update an existing commitment. UID stays fixed (sharing a re-export
/// with foreign calendars must remain stable); everything else is taken
/// from the draft. Bumps `sequence` so peers that have an older copy
/// recognise this is a newer revision.
#[tauri::command]
pub async fn cal_update(
    app: AppHandle,
    id: String,
    draft: CommitmentDraft,
) -> Result<Commitment, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Pull the existing row so we can keep id/uid/sequence/source.
    let existing = {
        let db_for_read = db.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let conn = db_for_read.reads.get().map_err(|e| e.to_string())?;
            store::get_with_attendees(&conn, &id).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("cal_update read task panicked: {e}"))??
        .ok_or("commitment not found")?
    };

    let updated = Commitment {
        id: existing.id.clone(),
        uid: existing.uid.clone(),
        sequence: existing.sequence + 1,
        summary: draft.summary,
        description: draft.description,
        location: draft.location,
        start_at: draft.start_at,
        end_at: draft.end_at,
        original_tzid: draft.original_tzid.or(existing.original_tzid),
        organizer: draft.organizer.or(existing.organizer),
        attendees: draft.attendees,
        source: existing.source,
        // Editing a previously cancelled event implicitly un-cancels it
        // (the user is bringing it back). For all other transitions the
        // existing status carries over.
        status: if existing.status == CommitmentStatus::Cancelled {
            CommitmentStatus::Confirmed
        } else {
            existing.status
        },
        // Carry over — only sync sets last_published_sequence; user-driven
        // edits leave it alone so the diff sees `sequence > last_published`
        // and publishes the update on the next sync.
        last_published_sequence: existing.last_published_sequence,
        source_message_id: existing.source_message_id,
        created_at: existing.created_at,
        updated_at: chrono::Utc::now(),
    };

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpsertCommitment {
            commitment: updated.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update: {e}"))?;
    maybe_spawn_mutation_sync(&app, "cal_update");
    Ok(updated)
}

/// Cancel a commitment per ADR-0011's Variante B: this is a normal
/// mutation that bumps SEQUENCE by 1 and sets STATUS:CANCELLED. The
/// row stays in the table so that Phase 2's IMAP-publish path can emit
/// the cancellation envelope into the shared folder (the timeBank side
/// then sees the tombstone and stops considering the slot allocated).
/// Phase 1's UI filters CANCELLED rows out of the list view.
///
/// Frontend keeps the historic name `cal_delete` so the existing button
/// label maps cleanly; semantically it's now a soft cancel. Hard delete
/// (purge) is a future operation, not in Phase 1 scope.
#[tauri::command]
pub async fn cal_delete(app: AppHandle, id: String) -> Result<Commitment, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let existing = {
        let db_for_read = db.clone();
        let id_for_read = id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let conn = db_for_read.reads.get().map_err(|e| e.to_string())?;
            store::get_with_attendees(&conn, &id_for_read).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("cal_delete read task panicked: {e}"))??
        .ok_or("commitment not found")?
    };

    // Already cancelled → idempotent return without bumping the counter
    // again. Saves Phase 2 from emitting redundant cancellation envelopes
    // when a UI accidentally fires twice.
    if existing.status == CommitmentStatus::Cancelled {
        return Ok(existing);
    }

    let cancelled = Commitment {
        id: existing.id.clone(),
        uid: existing.uid.clone(),
        sequence: existing.sequence + 1,
        summary: existing.summary,
        description: existing.description,
        location: existing.location,
        start_at: existing.start_at,
        end_at: existing.end_at,
        original_tzid: existing.original_tzid,
        organizer: existing.organizer,
        attendees: existing.attendees,
        source: existing.source,
        status: CommitmentStatus::Cancelled,
        last_published_sequence: existing.last_published_sequence,
        source_message_id: existing.source_message_id,
        created_at: existing.created_at,
        updated_at: chrono::Utc::now(),
    };

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpsertCommitment {
            commitment: cancelled.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db cancel: {e}"))?;
    maybe_spawn_mutation_sync(&app, "cal_delete");
    Ok(cancelled)
}

/// Import a `text/calendar` attachment from a stored mail into the local
/// commitment store. When `my_email` matches an attendee in the ICS and
/// `my_partstat` is set, that PARTSTAT is stamped on the local row — used
/// by the auto-save-on-Annehmen flow so the saved event reflects what we
/// just told the organizer.
#[tauri::command]
pub async fn cal_import_ics_attachment(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
    my_email: Option<String>,
    my_partstat: Option<InvitationResponse>,
) -> Result<Commitment, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let message_id_str = message_id.0.to_string();
    let commitment = tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Commitment, String> {
            let (bytes, _filename, _mime) = attachments::bytes(&db, &message_id, part_idx)?;
            let parsed = ics::parse(&bytes)?
                .ok_or_else(|| "ICS attachment contains no event".to_string())?;
            ics::ics_to_commitment(
                &parsed,
                Some(message_id_str),
                my_email.as_deref(),
                my_partstat,
            )
        }
    })
    .await
    .map_err(|e| format!("cal_import task panicked: {e}"))??;

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpsertCommitment {
            commitment: commitment.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db upsert: {e}"))?;
    maybe_spawn_mutation_sync(&app, "cal_import_ics_attachment");
    Ok(commitment)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedIcs {
    pub content: String,
    pub filename: String,
    /// Set when the caller passed a `destination` and the file write
    /// succeeded — convenient for the UI to show "saved to <path>".
    pub written_to: Option<String>,
}

/// Render a stored commitment to a standalone ICS blob (METHOD:REQUEST).
/// When `destination` is set, also writes the blob to that path so the
/// frontend can drive a save-as dialog without needing an extra fs plugin.
/// Filename is derived from the summary, with a fallback to the
/// commitment id.
#[tauri::command]
pub async fn cal_export_to_ics(
    app: AppHandle,
    id: String,
    destination: Option<String>,
) -> Result<ExportedIcs, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<ExportedIcs, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            let c = store::get_with_attendees(&conn, &id)
                .map_err(|e| e.to_string())?
                .ok_or("commitment not found")?;
            let content = ics::build_ics_for_commitment(&c);
            let filename = ics_filename_from_summary(c.summary.as_deref(), &c.id);
            let written_to = match destination {
                Some(path) => {
                    let p = std::path::PathBuf::from(&path);
                    if let Some(parent) = p.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| format!("create parent dir: {e}"))?;
                        }
                    }
                    std::fs::write(&p, content.as_bytes())
                        .map_err(|e| format!("write ics: {e}"))?;
                    Some(p.to_string_lossy().into_owned())
                }
                None => None,
            };
            Ok(ExportedIcs {
                content,
                filename,
                written_to,
            })
        }
    })
    .await
    .map_err(|e| format!("cal_export task panicked: {e}"))?
}

fn ics_filename_from_summary(summary: Option<&str>, id: &str) -> String {
    let base = summary.map(slugify).filter(|s| !s.is_empty());
    match base {
        Some(slug) => format!("{slug}.ics"),
        None => format!("event-{}.ics", &id[..id.len().min(8)]),
    }
}

/// Tiny ASCII slugifier — enough to make a filename. Drops anything outside
/// `[A-Za-z0-9_-]`, collapses runs of whitespace into `-`, lower-cases.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if ch.is_whitespace() || ch == '-' {
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
    }
    out.trim_matches('-').to_string()
}
