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

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::domain::{
    Commitment, CommitmentAttendee, CommitmentDraft, CommitmentSource, CommitmentStatus,
    Envelope, IcsParticipant, InvitationResponse, MessageDirection, Negotiation,
    NegotiationAction, ParsedIcsEvent, SlotStatus, ThreadRole,
};
use super::{ics, negotiation_engine, negotiation_store, store};
use crate::application::{attachments, smtp, timeprotocol_envelope};
use crate::domain::account::AccountId;
use crate::domain::message::MessageId;
use crate::infrastructure::db::WriteCmd;
use crate::infrastructure::queries;
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
    let from_for_blocking = from.clone();
    let to_for_blocking = to.clone();
    let mut rows = tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Vec<Commitment>, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            store::list_in_range(&conn, &from_for_blocking, &to_for_blocking)
                .map_err(|e| e.to_string())
        }
    })
    .await
    .map_err(|e| format!("cal_list task panicked: {e}"))??;

    // Overlay third-party subscription events on top. These never live
    // in SQLite — `events_in_range` reads them from the in-memory cache
    // in `subscriptions.rs`. We append + re-sort by start_at so the UI
    // sees a single chronological list.
    if let Some(sub_store) = state.subscription_store.get() {
        let overlay = sub_store.events_in_range(&from, &to).await;
        if !overlay.is_empty() {
            rows.extend(overlay);
            rows.sort_by(|a, b| a.start_at.cmp(&b.start_at));
        }
    }

    Ok(rows)
}

/// Extract event details from a stored mail via pi. Does NOT persist —
/// returns a draft for the frontend to open in the EventEditor (the
/// user reviews and saves). `Empty` outcome means pi found no usable
/// event data; the UI surfaces that as a polite "nichts gefunden"
/// banner instead of an error.
#[tauri::command]
pub async fn cal_extract_from_message(
    app: AppHandle,
    message_id: MessageId,
) -> Result<crate::application::event_extract::EventExtractionResult, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    crate::application::event_extract::extract_event_for_message(
        app.clone(),
        db.clone(),
        message_id,
    )
    .await
}

/// List commitments touching a contact (matched by any of the contact's
/// email addresses against ORGANIZER or any ATTENDEE row). The contact's
/// emails are resolved on the backend so the frontend doesn't need to
/// pass them in — same shape as `commands::contacts::list_messages_for_contact`.
///
/// `from`/`to` bound the `start_at` of returned events (RFC 3339). The
/// caller typically passes `now - 30d` and a far-future upper bound,
/// then partitions the result into "Anstehend" vs "Letzte 30 Tage" in
/// the UI. CANCELLED rows are excluded. Subscription overlays are not
/// included — those events live outside SQLite and the contact-side
/// view focuses on commitments the user actually accepted/organized.
#[tauri::command]
pub async fn cal_list_for_contact(
    app: AppHandle,
    contact_id: String,
    from: String,
    to: String,
    limit: Option<i64>,
) -> Result<Vec<Commitment>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let contact_uuid =
        uuid::Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    let lim = limit.unwrap_or(200).clamp(1, 1000);

    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Vec<Commitment>, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            // Resolve the contact's email addresses. The contacts schema
            // owns this table; the calendar store reads it through plain
            // SQL rather than crossing module boundaries — Calendar's
            // Mail-layer-boundary discipline doesn't apply to its own
            // SQLite (the DB is shared infrastructure, not Mail-domain).
            let mut stmt = conn
                .prepare("SELECT email FROM contact_emails WHERE contact_id = ?1")
                .map_err(|e| format!("prepare contact_emails: {e}"))?;
            let emails: Vec<String> = stmt
                .query_map(
                    rusqlite::params![contact_uuid.to_string()],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|e| format!("query contact_emails: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("collect contact_emails: {e}"))?;
            if emails.is_empty() {
                return Ok(Vec::new());
            }
            store::list_for_emails(&conn, &emails, &from, &to, lim)
                .map_err(|e| e.to_string())
        }
    })
    .await
    .map_err(|e| format!("cal_list_for_contact panicked: {e}"))?
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
        // Series membership is a property of the row, not of the edit —
        // editing an occurrence keeps it tied to its series.
        series_uid: existing.series_uid,
        // Subscription rows are read-only and never reach `cal_update`
        // / `cal_delete` — the editor refuses these paths in the UI.
        // Keeping the field at None here matches that contract (and
        // would self-heal if a row ever slipped through).
        subscription_id: None,
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
        series_uid: existing.series_uid,
        // Subscription rows are read-only and never reach `cal_update`
        // / `cal_delete` — the editor refuses these paths in the UI.
        // Keeping the field at None here matches that contract (and
        // would self-heal if a row ever slipped through).
        subscription_id: None,
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
pub struct IcsImportReport {
    /// VEVENTs that were upserted into the local store.
    pub imported: usize,
    /// VEVENTs that the parser produced but we couldn't turn into a
    /// Commitment — usually missing DTSTART/DTEND. Soft-skipped, not an
    /// error: VTODO entries and stub events shouldn't block the import.
    pub skipped: usize,
    /// Per-event hard errors (writer rejected the upsert, malformed time
    /// zone, …). The import keeps going past them so a partial bulk import
    /// still lands what it can; the UI displays the count.
    pub errors: Vec<String>,
}

/// Import a user-supplied `.ics` file containing one or more VEVENTs. The
/// path comes from a Tauri save/open dialog — Tauri's fs-scope check has
/// already vetted it. We read once, parse all VEVENTs, and upsert by UID
/// so a re-import refreshes existing rows instead of duplicating them.
///
/// Size guard at 8 MiB keeps a stray pointer at a multi-GB file from
/// OOM'ing us; real calendars (even busy multi-year exports) sit well
/// under that.
#[tauri::command]
pub async fn cal_import_ics_file(
    app: AppHandle,
    path: String,
) -> Result<IcsImportReport, String> {
    const MAX_ICS_BYTES: usize = 8 * 1024 * 1024;

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Check size before reading so a typo-pointed-at-a-huge-file (or a
    // user dropping the wrong path) doesn't load gigabytes into RAM
    // before we reject it.
    let metadata = std::fs::metadata(&path).map_err(|e| format!("stat {path}: {e}"))?;
    if metadata.len() > MAX_ICS_BYTES as u64 {
        return Err(format!(
            "ICS file too large ({} bytes, limit {MAX_ICS_BYTES})",
            metadata.len()
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;

    let parsed = tauri::async_runtime::spawn_blocking(move || ics::parse_all(&bytes))
        .await
        .map_err(|e| format!("parse task panicked: {e}"))??;

    let mut report = IcsImportReport {
        imported: 0,
        skipped: 0,
        errors: Vec::new(),
    };

    for ev in parsed {
        // Expand recurring events into per-occurrence rows. Plain singletons
        // pass through as a Vec of length 1.
        let rows = match ics::ics_to_commitments(&ev, None, None, None) {
            Ok(rs) if rs.is_empty() => {
                report.skipped += 1;
                continue;
            }
            Ok(rs) => rs,
            Err(_) => {
                // VTODO, VFREEBUSY, VEVENT without DTSTART/DTEND, or a
                // malformed RRULE. The user gets a count of skips; the
                // specific reason isn't actionable.
                report.skipped += 1;
                continue;
            }
        };
        // Track whether this VEVENT's series was added at-least-once so
        // the cleanup-of-old-occurrences path (if we add one later) has
        // an anchor. Right now we just upsert per-occurrence-UID; stale
        // rows from a previous import outside the new window survive
        // until the user clears the series manually — accepted tradeoff
        // documented at the call site in CalendarView.
        for commitment in rows {
            let occ_uid = commitment.uid.clone();
            let (tx, rx) = oneshot::channel();
            db.writer
                .send(WriteCmd::UpsertCommitment {
                    commitment,
                    ack: tx,
                })
                .await
                .map_err(|_| "writer channel closed".to_string())?;
            match rx
                .await
                .map_err(|_| "writer dropped ack".to_string())?
            {
                Ok(()) => report.imported += 1,
                Err(e) => report.errors.push(format!("{occ_uid}: {e}")),
            }
        }
    }

    if report.imported > 0 {
        maybe_spawn_mutation_sync(&app, "cal_import_ics_file");
    }

    Ok(report)
}

/// Cascade-delete every occurrence row that shares a `series_uid`. The
/// UI hands us the master UID of an expanded RRULE series; we wipe the
/// whole series in one transaction. No IMAP cancellation is emitted —
/// series rows are excluded from the publish path (sync.rs filter), so
/// nothing went out to begin with.
#[tauri::command]
pub async fn cal_cancel_series(
    app: AppHandle,
    series_uid: String,
) -> Result<usize, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteSeries {
            series_uid,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    let removed = rx
        .await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete series: {e}"))?;
    Ok(removed)
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationRequestDraft {
    /// Persisted commitment after SEQUENCE bump + organizer stamp. Mirrors
    /// the contract of `cal_update` — frontend uses it to refresh its
    /// in-memory copy without a second round-trip.
    pub commitment: Commitment,
    /// Each attendee in a To: shape ready to drop into a ComposeDraft.
    /// `email` is the bare address; `display_name` is the optional CN.
    /// The frontend formats `"Name <email>, …"` itself so the user can
    /// edit before sending.
    pub recipients: Vec<IcsParticipant>,
    /// Echoed for the subject/body templating in the frontend.
    pub event_summary: Option<String>,
    /// On-disk path of the freshly-written REQUEST ICS. Drop into the
    /// outgoing ComposeDraft attachments — the SMTP path's iMIP detection
    /// (`method=REQUEST`) routes it as `multipart/alternative`.
    pub attachment_path: String,
    pub attachment_filename: String,
    pub attachment_size_bytes: u32,
}

/// Stamp the sending account as ORGANIZER, bump SEQUENCE by 1 (RFC 5546
/// §3.2.2.1: every REQUEST that is not the initial advertisement must
/// carry a higher SEQUENCE than the previous one — we treat every
/// explicit "send invitation" click as a re-advertisement so re-invites
/// after edits stay spec-conformant), persist the updated row, render a
/// METHOD:REQUEST ICS, drop it in a stable temp file, and hand the
/// frontend everything it needs to seed a Compose draft.
///
/// Refuses commitments without attendees (no one to invite), commitments
/// from third-party subscriptions (we don't own the canonical row), and
/// `CANCELLED` rows (cancellation re-publish goes through a separate
/// CANCEL flow — not in this PR).
#[tauri::command]
pub async fn cal_build_invitation_request(
    app: AppHandle,
    id: String,
    account_id: AccountId,
) -> Result<InvitationRequestDraft, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Resolve the sending account up front so we can fail fast before any
    // mutation. We need both address (mandatory ORGANIZER mailto) and
    // from_name (optional CN).
    let (organizer_email, organizer_name) = {
        let db_for_read = db.clone();
        let account_id = account_id.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(String, String), String> {
            let conn = db_for_read.reads.get().map_err(|e| e.to_string())?;
            let acc = queries::get_account(&conn, &account_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "account not found".to_string())?;
            Ok((acc.address, acc.from_name))
        })
        .await
        .map_err(|e| format!("account lookup panicked: {e}"))??
    };

    let existing = {
        let db_for_read = db.clone();
        let id_for_read = id.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<Commitment, String> {
            let conn = db_for_read.reads.get().map_err(|e| e.to_string())?;
            store::get_with_attendees(&conn, &id_for_read)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "commitment not found".to_string())
        })
        .await
        .map_err(|e| format!("cal_build_invitation read panicked: {e}"))??
    };

    if existing.subscription_id.is_some() {
        return Err("cannot invite from a subscribed (read-only) event".into());
    }
    if existing.status == CommitmentStatus::Cancelled {
        return Err("cannot send invitation for a cancelled event".into());
    }
    if existing.attendees.is_empty() {
        return Err("no attendees to invite".into());
    }

    // Strip the organizer from the attendee list — RFC 5546 §3.2.2.1
    // forbids the ORGANIZER also appearing as ATTENDEE. UI lets the user
    // accidentally type their own address, so we self-heal here.
    let attendees_for_send: Vec<CommitmentAttendee> = existing
        .attendees
        .iter()
        .filter(|a| !a.email.eq_ignore_ascii_case(&organizer_email))
        .cloned()
        .collect();
    if attendees_for_send.is_empty() {
        return Err("no attendees to invite (only the organizer is listed)".into());
    }

    let organizer = IcsParticipant {
        email: organizer_email.clone(),
        display_name: if organizer_name.trim().is_empty() {
            None
        } else {
            Some(organizer_name.clone())
        },
        partstat: None,
    };

    let now = chrono::Utc::now();
    let updated = Commitment {
        id: existing.id.clone(),
        uid: existing.uid.clone(),
        sequence: existing.sequence + 1,
        summary: existing.summary.clone(),
        description: existing.description.clone(),
        location: existing.location.clone(),
        start_at: existing.start_at.clone(),
        end_at: existing.end_at.clone(),
        original_tzid: existing.original_tzid.clone(),
        organizer: Some(organizer.clone()),
        // Persisted attendees keep their stored state (including the
        // organizer-if-self-added) — it's only the *outgoing* ICS that
        // drops the organizer. Stripping persisted state would discard
        // any PARTSTAT already recorded for a prior reply.
        attendees: existing.attendees.clone(),
        source: existing.source,
        status: existing.status,
        last_published_sequence: existing.last_published_sequence,
        source_message_id: existing.source_message_id.clone(),
        series_uid: existing.series_uid.clone(),
        subscription_id: None,
        created_at: existing.created_at,
        updated_at: now,
    };

    // Build the REQUEST ICS off a synthetic Commitment whose `attendees`
    // is the deduplicated send-list. This keeps the ICS attendee block
    // free of the organizer without diverging the stored row.
    let mut for_ics = updated.clone();
    for_ics.attendees = attendees_for_send.clone();
    let ics_text = ics::build_invitation_request(&for_ics);

    // Persist after building (so a builder panic doesn't leave a half-
    // bumped row behind). Writer is atomic.
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

    // Write the ICS to a stable temp location, scoped per commitment.
    // Re-clicking "Einladung senden" overwrites idempotently — only one
    // outstanding invite per event at a time.
    let dir = std::env::temp_dir()
        .join("crystalmail")
        .join("ics-request")
        .join(&updated.id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create request temp dir: {e}"))?;
    let filename = format!(
        "invite-{}.ics",
        ics_filename_from_summary(updated.summary.as_deref(), &updated.id)
            .trim_end_matches(".ics")
    );
    let path = dir.join(&filename);
    let bytes = ics_text.into_bytes();
    let size = bytes.len() as u32;
    std::fs::write(&path, &bytes).map_err(|e| format!("write request ics: {e}"))?;

    maybe_spawn_mutation_sync(&app, "cal_build_invitation_request");

    Ok(InvitationRequestDraft {
        commitment: updated.clone(),
        recipients: attendees_for_send
            .iter()
            .map(|a| IcsParticipant {
                email: a.email.clone(),
                display_name: a.display_name.clone(),
                partstat: a.partstat.clone(),
            })
            .collect(),
        event_summary: updated.summary,
        attachment_path: path.to_string_lossy().into_owned(),
        attachment_filename: filename,
        attachment_size_bytes: size,
    })
}

/// Outcome the frontend banner uses to decide between
/// "Bob hat zugesagt" / "Bob hat abgesagt" / "kein passender Termin".
/// Mirrors `ics::ReplyApplyOutcome` plus the extra context the UI needs
/// (responder identity + the row that was updated).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationReplyApplied {
    /// UID echoed from the REPLY — useful for the UI's "view this event"
    /// deep-link even when no local row exists yet.
    pub uid: String,
    /// `applied` / `noMatchingCommitment` / `noMatchingAttendee`.
    pub outcome: super::ics::ReplyApplyOutcome,
    /// PARTSTAT we actually wrote (`ACCEPTED`, `DECLINED`, `TENTATIVE`,
    /// …). `None` when nothing was applied. Single value — REPLYs
    /// typically carry one ATTENDEE; if multiple, we report the first
    /// matched one.
    pub responder_partstat: Option<String>,
    /// The responder's mailto address from the REPLY's ATTENDEE line —
    /// surfaced so the banner can say "Bob hat zugesagt" without re-
    /// reading the parsed event. `None` when the REPLY had no usable
    /// ATTENDEE entries.
    pub responder_email: Option<String>,
    pub responder_display_name: Option<String>,
    /// Updated row when `outcome = Applied`; `None` otherwise.
    pub commitment: Option<Commitment>,
}

/// Apply an inbound `text/calendar; method=REPLY` attachment to the local
/// commitment it references. Idempotent — re-applying the same REPLY
/// writes the same PARTSTAT a second time, no row churn beyond
/// `updated_at`. Silent on UID-not-found (we likely organized the invite
/// on a different device) and on attendee-not-found (delegation or typo).
///
/// Does NOT bump SEQUENCE: RFC 5546 §3.2.3 treats REPLYs as
/// attendee-scoped responses to a specific SEQUENCE the organizer
/// already advertised, not a new revision of the event.
#[tauri::command]
pub async fn cal_apply_invitation_reply(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
) -> Result<InvitationReplyApplied, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Parse the ICS off the writer thread (file I/O + parser).
    let parsed = {
        let db_for_parse = db.clone();
        let message_id = message_id.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<ParsedIcsEvent, String> {
            let (bytes, _filename, _mime) = attachments::bytes(
                &db_for_parse,
                &message_id,
                part_idx,
            )?;
            ics::parse(&bytes)?
                .ok_or_else(|| "ICS attachment contains no event".to_string())
        })
        .await
        .map_err(|e| format!("ics parse panicked: {e}"))??
    };

    // Refuse non-REPLY: the dedicated REQUEST/REPLY paths must stay
    // separate so we never mistake an Outlook calendar-publish for a
    // response.
    let is_reply = parsed
        .method
        .as_deref()
        .map(|m| m.eq_ignore_ascii_case("REPLY"))
        .unwrap_or(false);
    if !is_reply {
        return Err(format!(
            "expected METHOD:REPLY, got {:?}",
            parsed.method
        ));
    }

    // Find the responder we'll report on (first ATTENDEE with a
    // non-empty PARTSTAT). The apply loop below handles all of them,
    // but the UI banner only needs one summary line.
    let responder = parsed
        .attendees
        .iter()
        .find(|a| {
            a.partstat
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        })
        .cloned();

    // Look up the local commitment by UID.
    let existing = {
        let db_for_read = db.clone();
        let uid = parsed.uid.clone();
        tauri::async_runtime::spawn_blocking(
            move || -> Result<Option<Commitment>, String> {
                let conn = db_for_read.reads.get().map_err(|e| e.to_string())?;
                store::get_by_uid(&conn, &uid).map_err(|e| e.to_string())
            },
        )
        .await
        .map_err(|e| format!("cal_apply_reply read panicked: {e}"))??
    };

    let Some(existing) = existing else {
        return Ok(InvitationReplyApplied {
            uid: parsed.uid,
            outcome: super::ics::ReplyApplyOutcome::NoMatchingCommitment,
            responder_partstat: responder.as_ref().and_then(|r| r.partstat.clone()),
            responder_email: responder.as_ref().map(|r| r.email.clone()),
            responder_display_name: responder.as_ref().and_then(|r| r.display_name.clone()),
            commitment: None,
        });
    };

    // Subscriptions are read-only overlays — they never live in
    // SQLite, so `get_by_uid` shouldn't return one. Belt-and-braces.
    if existing.subscription_id.is_some() {
        return Ok(InvitationReplyApplied {
            uid: parsed.uid,
            outcome: super::ics::ReplyApplyOutcome::NoMatchingCommitment,
            responder_partstat: responder.as_ref().and_then(|r| r.partstat.clone()),
            responder_email: responder.as_ref().map(|r| r.email.clone()),
            responder_display_name: responder.as_ref().and_then(|r| r.display_name.clone()),
            commitment: None,
        });
    }

    let (updated, outcome) = ics::apply_reply_to_commitment(&existing, &parsed);

    if outcome != super::ics::ReplyApplyOutcome::Applied {
        return Ok(InvitationReplyApplied {
            uid: parsed.uid,
            outcome,
            responder_partstat: responder.as_ref().and_then(|r| r.partstat.clone()),
            responder_email: responder.as_ref().map(|r| r.email.clone()),
            responder_display_name: responder.as_ref().and_then(|r| r.display_name.clone()),
            commitment: None,
        });
    }

    // Persist. PARTSTAT updates are not a SEQUENCE-bump (see §3.2.3 of
    // RFC 5546) — `maybe_spawn_mutation_sync` still fires because the
    // attendee row mutated, and the IMAP-folder profile expects local
    // mutations to round-trip back as PUBLISH envelopes. But the
    // VEVENT SEQUENCE stays put.
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
        .map_err(|e| format!("db apply reply: {e}"))?;
    maybe_spawn_mutation_sync(&app, "cal_apply_invitation_reply");

    Ok(InvitationReplyApplied {
        uid: parsed.uid,
        outcome,
        responder_partstat: responder.as_ref().and_then(|r| r.partstat.clone()),
        responder_email: responder.as_ref().map(|r| r.email.clone()),
        responder_display_name: responder.as_ref().and_then(|r| r.display_name.clone()),
        commitment: Some(updated),
    })
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

// ─── Phase 3: negotiation commands ────────────────────────────────────────

/// Idempotent: parse the timeProtocol envelope from a `text/calendar`/-
/// adjacent attachment of an open mail, apply it to the negotiation
/// state machine, persist the result, and return the up-to-date
/// `Negotiation` for the frontend to render.
///
/// `own_email` lets the engine decide which side of the envelope is
/// "us" — typically the open mail's account address. The Reader has
/// it from `account.address` and threads it through here.
///
/// Re-calling this on a mail whose `message_id` has already been
/// processed is a no-op: we look up the existing negotiation and
/// return it unchanged. That makes the command safe to invoke on
/// every Reader render.
#[tauri::command]
pub async fn tp_apply_envelope_from_attachment(
    app: AppHandle,
    message_id: MessageId,
    part_idx: u32,
    own_email: String,
) -> Result<Negotiation, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let message_id_for_log = message_id.0.to_string();
    let envelope = {
        let db = db.clone();
        tauri::async_runtime::spawn_blocking(move || {
            timeprotocol_envelope::parse_envelope_from_attachment(&db, &message_id, part_idx)
        })
        .await
        .map_err(|e| format!("envelope parse task panicked: {e}"))??
    };

    // Idempotency check: have we already processed this message_id?
    {
        let db_for_lookup = db.clone();
        let mid = envelope.message_id.clone();
        let already = tauri::async_runtime::spawn_blocking(move || -> Result<bool, String> {
            let conn = db_for_lookup.reads.get().map_err(|e| e.to_string())?;
            negotiation_store::message_id_exists(&conn, &mid).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("idempotency lookup panicked: {e}"))??;
        if already {
            // Return the current state without re-applying. Look up by
            // negotiation_id (now in `envelope`).
            let neg_id = envelope.negotiation_id.clone();
            let db_for_lookup = db.clone();
            let neg = tauri::async_runtime::spawn_blocking(
                move || -> Result<Option<Negotiation>, String> {
                    let conn = db_for_lookup.reads.get().map_err(|e| e.to_string())?;
                    negotiation_store::get_by_negotiation_id(&conn, &neg_id)
                        .map_err(|e| e.to_string())
                },
            )
            .await
            .map_err(|e| format!("negotiation lookup panicked: {e}"))??;
            return neg.ok_or_else(|| {
                format!(
                    "message_id {} already processed but negotiation row missing",
                    envelope.message_id
                )
            });
        }
    }

    // Look up existing negotiation (None = fresh request).
    let existing = {
        let db_for_lookup = db.clone();
        let neg_id = envelope.negotiation_id.clone();
        tauri::async_runtime::spawn_blocking(
            move || -> Result<Option<Negotiation>, String> {
                let conn = db_for_lookup.reads.get().map_err(|e| e.to_string())?;
                negotiation_store::get_by_negotiation_id(&conn, &neg_id)
                    .map_err(|e| e.to_string())
            },
        )
        .await
        .map_err(|e| format!("negotiation lookup panicked: {e}"))??
    };

    // Apply.
    let (mut updated, message) = negotiation_engine::apply_envelope(
        existing.as_ref(),
        &envelope,
        MessageDirection::Inbound,
        &own_email,
        Some(message_id_for_log),
    )?;

    // Persist atomically through the writer actor.
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::ApplyNegotiationUpdate {
            negotiation: updated.clone(),
            new_message: Some(message.clone()),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db apply: {e}"))?;

    // Hydrate the messages list (the engine returned an empty one).
    let neg_id_for_hydrate = updated.negotiation_id.clone();
    let db_for_hydrate = db.clone();
    let hydrated = tauri::async_runtime::spawn_blocking(
        move || -> Result<Option<Negotiation>, String> {
            let conn = db_for_hydrate.reads.get().map_err(|e| e.to_string())?;
            negotiation_store::get_by_negotiation_id(&conn, &neg_id_for_hydrate)
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| format!("hydrate panicked: {e}"))??;
    if let Some(h) = hydrated {
        updated = h;
    }

    // If this envelope tipped the thread into Confirmed, materialise
    // a local Commitment so the user sees the meeting in their
    // calendar without a separate click.
    if matches!(updated.state, super::domain::NegotiationState::Confirmed) {
        if let Err(e) = materialize_commitment_if_confirmed(&app, &updated.negotiation_id).await {
            tracing::warn!(error = %e, "tp inbound confirm: materialise failed");
        } else {
            // Re-fetch one more time to surface the freshly-set
            // confirmed_commitment_id back to the frontend.
            let neg_id_final = updated.negotiation_id.clone();
            let db_final = db.clone();
            if let Ok(Some(refreshed)) = tauri::async_runtime::spawn_blocking(
                move || -> Result<Option<Negotiation>, String> {
                    let conn = db_final.reads.get().map_err(|e| e.to_string())?;
                    negotiation_store::get_by_negotiation_id(&conn, &neg_id_final)
                        .map_err(|e| e.to_string())
                },
            )
            .await
            .unwrap_or(Ok(None))
            {
                updated = refreshed;
            }
        }
    }
    Ok(updated)
}

/// Read-only fetch by negotiation_id. Used by the panel after a
/// response is sent to refresh the displayed state.
#[tauri::command]
pub async fn tp_get_negotiation(
    app: AppHandle,
    negotiation_id: String,
) -> Result<Option<Negotiation>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Option<Negotiation>, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            negotiation_store::get_by_negotiation_id(&conn, &negotiation_id)
                .map_err(|e| e.to_string())
        }
    })
    .await
    .map_err(|e| format!("tp_get_negotiation panicked: {e}"))?
}

/// One slot the responder is offering. Multi-slot per propose
/// envelope per the v0.1-MVP profile is supported by sending several
/// `propose` envelopes back-to-back; for v1 we keep the per-call
/// shape simple (caller picks one slot per click; multi-slot UI on
/// top is a future cleanup).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotInput {
    pub start_at: String,
    pub end_at: String,
}

/// Send a fresh `request` envelope to start a new negotiation. Mints
/// the `negotiation_id` (initiating-node ownership per spec
/// §"Identifier rules"), persists the new thread as a local
/// Initiator-side row, and dispatches the mail. The returned
/// `Negotiation` has state=Requested with no slots yet — the
/// counterparty's eventual `propose` envelope (or our future
/// counter-propose) will fill those in.
#[tauri::command]
pub async fn tp_send_initial_request(
    app: AppHandle,
    account_id: AccountId,
    to_email: String,
    duration: String,
    latest: Option<String>,
    preferred_time: Option<String>,
    minimum_notice: Option<String>,
    summary: Option<String>,
) -> Result<Negotiation, String> {
    if to_email.trim().is_empty() {
        return Err("recipient email required".into());
    }
    if duration.trim().is_empty() {
        return Err("duration required (e.g. PT45M)".into());
    }
    let our_email = own_email_for_account(&app, &account_id).await?;
    if our_email.eq_ignore_ascii_case(to_email.trim()) {
        return Err("cannot start a negotiation with yourself".into());
    }

    let constraints_json = serde_json::json!({
        "latest": latest,
        "preferredTime": preferred_time,
        "minimumNotice": minimum_notice,
    });
    // Build the envelope with a freshly-minted negotiation_id. The
    // initiating node owns this id for the entire thread; subsequent
    // envelopes from the counterparty will reference it back.
    let negotiation_id = format!("{our_email}:neg-{}", Uuid::new_v4());
    let envelope = Envelope {
        message_id: format!("{our_email}:msg-{}", Uuid::new_v4()),
        from: our_email.clone(),
        to: to_email.trim().to_string(),
        negotiation_id: negotiation_id.clone(),
        action: NegotiationAction::Request,
        timestamp: chrono::Utc::now().to_rfc3339(),
        payload: serde_json::json!({
            "duration": duration,
            "constraints": constraints_json,
            "summary": summary,
        }),
    };

    // Apply outbound to the engine — `existing=None` means the engine
    // bootstraps a fresh Initiator-side Negotiation row.
    let (updated, message) = negotiation_engine::apply_envelope(
        None,
        &envelope,
        MessageDirection::Outbound,
        &our_email,
        None,
    )?;

    // Persist before sending: same optimistic-local-commit pattern as
    // the response commands. SMTP failures still leave the local state
    // visible so the user can retry from the (now-existing) panel.
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::ApplyNegotiationUpdate {
            negotiation: updated.clone(),
            new_message: Some(message),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db apply: {e}"))?;

    // Build + send the mail.
    let envelope_json = serde_json::to_vec_pretty(&envelope)
        .map_err(|e| format!("envelope serialize: {e}"))?;
    let attachment_path = write_envelope_temp_file(&envelope.message_id, &envelope_json)?;
    let mail_subject = format!(
        "[TimeProtocol] request — {}",
        summary.as_deref().unwrap_or("Termin")
    );
    let mail_body = format!(
        "Diese Mail enthält eine timeProtocol-Negotiation-Anfrage (request).\n\
         Wenn dein Mail-Client das Format nicht erkennt, wird die Anfrage\n\
         als gewöhnliche Mail mit JSON-Anhang angezeigt — du kannst sie\n\
         einfach ignorieren.\n\n\
         Dauer: {duration}\n"
    );
    let req = smtp::SendMailRequest {
        account_id: account_id.clone(),
        from: None,
        to: vec![to_email.trim().to_string()],
        cc: vec![],
        bcc: vec![],
        subject: mail_subject,
        body: mail_body,
        body_html: None,
        in_reply_to: None,
        references: vec![],
        attachments: vec![smtp::AttachmentSpec {
            path: attachment_path.to_string_lossy().into_owned(),
            filename: Some("envelope-request.json".into()),
            mime_type: Some("application/time-protocol+json".into()),
            ..Default::default()
        }],
    };
    smtp::send(db, req).await.map_err(|e| format!("smtp: {e}"))?;

    // Return the hydrated negotiation row so the frontend can
    // immediately render an Initiator-side waiting view.
    fetch_negotiation(&app, &negotiation_id).await
}

/// Send a `propose` (or `counter_propose` if `is_counter`) for one or
/// more slots. Each slot ships as its own envelope/mail per the MVP
/// profile §4.1; one Tauri call sends all of them sequentially.
#[tauri::command]
pub async fn tp_send_propose_slots(
    app: AppHandle,
    negotiation_id: String,
    slots: Vec<SlotInput>,
    account_id: AccountId,
    is_counter: bool,
) -> Result<Negotiation, String> {
    if slots.is_empty() {
        return Err("at least one slot required".into());
    }
    let action = if is_counter {
        NegotiationAction::CounterPropose
    } else {
        NegotiationAction::Propose
    };
    for slot in &slots {
        let payload = serde_json::json!({
            "type": match action {
                NegotiationAction::CounterPropose => "counter_propose_slot",
                _ => "propose_slot",
            },
            "slotId": format!("{}:slot-{}", own_email_for_account(&app, &account_id).await?, Uuid::new_v4()),
            "startAt": slot.start_at,
            "endAt": slot.end_at,
        });
        send_outbound_envelope(&app, &account_id, &negotiation_id, action, payload).await?;
    }
    fetch_negotiation(&app, &negotiation_id).await
}

/// Confirm one slot. Spec §5: this is a binding declaration; on
/// receipt the counterparty independently materialises a local
/// `commitment` for the slot. We materialise on our side too (in
/// `super::sync::materialize_commitment_from_negotiation`, called
/// after persistence below).
#[tauri::command]
pub async fn tp_send_confirm_slot(
    app: AppHandle,
    negotiation_id: String,
    slot_id: String,
    account_id: AccountId,
) -> Result<Negotiation, String> {
    let payload = serde_json::json!({
        "type": "confirm_slot",
        "slotId": slot_id,
    });
    send_outbound_envelope(
        &app,
        &account_id,
        &negotiation_id,
        NegotiationAction::Confirm,
        payload,
    )
    .await?;
    // Outbound confirm side: we materialise the commitment on send,
    // mirroring the receiver who'll do the same on receipt.
    if let Err(e) = materialize_commitment_if_confirmed(&app, &negotiation_id).await {
        tracing::warn!(error = %e, "tp outbound confirm: materialise failed");
    }
    fetch_negotiation(&app, &negotiation_id).await
}

/// Release a slot (or the whole thread once no Active slots remain).
#[tauri::command]
pub async fn tp_send_release_slot(
    app: AppHandle,
    negotiation_id: String,
    slot_id: String,
    account_id: AccountId,
) -> Result<Negotiation, String> {
    let payload = serde_json::json!({
        "type": "release_slot",
        "slotId": slot_id,
    });
    send_outbound_envelope(
        &app,
        &account_id,
        &negotiation_id,
        NegotiationAction::Release,
        payload,
    )
    .await?;
    fetch_negotiation(&app, &negotiation_id).await
}

// ─── Outbound helpers ─────────────────────────────────────────────────────

async fn own_email_for_account(
    app: &AppHandle,
    account_id: &AccountId,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let account_id = account_id.clone();
    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<String, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            queries::get_account(&conn, &account_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "account not found".to_string())
                .map(|a| a.address)
        }
    })
    .await
    .map_err(|e| format!("own_email task panicked: {e}"))?
}

async fn fetch_negotiation(
    app: &AppHandle,
    negotiation_id: &str,
) -> Result<Negotiation, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let neg_id = negotiation_id.to_string();
    tauri::async_runtime::spawn_blocking({
        let db = db.clone();
        move || -> Result<Negotiation, String> {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            negotiation_store::get_by_negotiation_id(&conn, &neg_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "negotiation row vanished after write".to_string())
        }
    })
    .await
    .map_err(|e| format!("fetch_negotiation panicked: {e}"))?
}

/// The outbound envelope path: build envelope, persist outbound
/// message via the engine + writer actor, write JSON to a temp file,
/// dispatch SMTP. Fire-and-forget at the SMTP level — we await it so
/// the user sees an error rather than a silent drop. Network failure
/// here surfaces to the caller as `Err(_)`.
async fn send_outbound_envelope(
    app: &AppHandle,
    account_id: &AccountId,
    negotiation_id: &str,
    action: NegotiationAction,
    payload: serde_json::Value,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let our_email = own_email_for_account(app, account_id).await?;

    // Look up the existing negotiation to get the counterparty.
    let existing = {
        let db_for_lookup = db.clone();
        let neg_id = negotiation_id.to_string();
        tauri::async_runtime::spawn_blocking(
            move || -> Result<Option<Negotiation>, String> {
                let conn = db_for_lookup.reads.get().map_err(|e| e.to_string())?;
                negotiation_store::get_by_negotiation_id(&conn, &neg_id)
                    .map_err(|e| e.to_string())
            },
        )
        .await
        .map_err(|e| format!("negotiation lookup panicked: {e}"))??
    }
    .ok_or_else(|| {
        format!("no negotiation found for id {negotiation_id} — cannot respond")
    })?;

    let envelope = Envelope {
        message_id: format!("{our_email}:msg-{}", Uuid::new_v4()),
        from: our_email.clone(),
        to: existing.counterparty_email.clone(),
        negotiation_id: existing.negotiation_id.clone(),
        action,
        timestamp: chrono::Utc::now().to_rfc3339(),
        payload,
    };

    // Apply outbound envelope to engine to compute new state.
    let (updated, message) = negotiation_engine::apply_envelope(
        Some(&existing),
        &envelope,
        MessageDirection::Outbound,
        &our_email,
        None,
    )?;

    // Persist BEFORE sending the mail. If the SMTP send fails the
    // user sees the error but the local state already reflects the
    // intent — they can retry by reopening the panel and clicking
    // again, which sees the message_id (not yet seen by the
    // counterparty) and won't re-persist. The counterparty just
    // never sees a message that doesn't exist on their side.
    //
    // This is consciously "optimistic local commit" — it's the same
    // model as the existing iMIP REPLY flow (compose-then-send;
    // store the local commitment regardless of SMTP outcome).
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::ApplyNegotiationUpdate {
            negotiation: updated.clone(),
            new_message: Some(message),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db apply: {e}"))?;

    // Build the mail. Single attachment (the JSON envelope) + a
    // text/plain body that explains what arrived for human readers
    // (not all counterparties will be timeProtocol-aware in v0.1).
    let envelope_json = serde_json::to_vec_pretty(&envelope)
        .map_err(|e| format!("envelope serialize: {e}"))?;
    let attachment_path = write_envelope_temp_file(&envelope.message_id, &envelope_json)?;
    let subject = format!(
        "[TimeProtocol] {} — {}",
        action.as_str(),
        existing
            .display_summary
            .as_deref()
            .unwrap_or("Termin")
    );
    let body = format!(
        "Diese Mail enthält eine timeProtocol-Negotiation-Aktion ({}).\n\
         Wenn dein Mail-Client das Format nicht erkennt, wird die Anfrage\n\
         als gewöhnliche Mail mit JSON-Anhang angezeigt — du kannst sie\n\
         einfach ignorieren.\n",
        action.as_str()
    );
    let req = smtp::SendMailRequest {
        account_id: account_id.clone(),
        from: None,
        to: vec![existing.counterparty_email.clone()],
        cc: vec![],
        bcc: vec![],
        subject,
        body,
        body_html: None,
        in_reply_to: None,
        references: vec![],
        attachments: vec![smtp::AttachmentSpec {
            path: attachment_path.to_string_lossy().into_owned(),
            filename: Some(format!(
                "envelope-{}.json",
                action.as_str().replace('_', "-")
            )),
            mime_type: Some("application/time-protocol+json".into()),
            ..Default::default()
        }],
    };
    smtp::send(db, req).await.map_err(|e| format!("smtp: {e}"))?;
    Ok(())
}

fn write_envelope_temp_file(message_id: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("crystalmail").join("tp-envelope");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create temp dir: {e}"))?;
    let safe_id = message_id.replace([':', '/', '\\'], "_");
    let path = dir.join(format!("{safe_id}.json"));
    std::fs::write(&path, bytes).map_err(|e| format!("write envelope file: {e}"))?;
    Ok(path)
}

/// If the negotiation has reached `Confirmed` and we haven't yet
/// materialised a `Commitment` for it, create the commitment row and
/// link it back via `confirmed_commitment_id`. Per MVP profile §4.1:
/// "Upon receiving a valid confirm, both nodes independently create a
/// local commitment for the confirmed slot." This helper covers both
/// directions — inbound (we received their confirm) and outbound (we
/// sent the confirm).
///
/// Returns the new commitment's id when one was created, `None` when
/// no work was needed.
async fn materialize_commitment_if_confirmed(
    app: &AppHandle,
    negotiation_id: &str,
) -> Result<Option<String>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Re-fetch the negotiation post-update so we see the canonical
    // confirmed slot and any prior materialisation marker.
    let neg = {
        let db_for_lookup = db.clone();
        let neg_id = negotiation_id.to_string();
        tauri::async_runtime::spawn_blocking(
            move || -> Result<Option<Negotiation>, String> {
                let conn = db_for_lookup.reads.get().map_err(|e| e.to_string())?;
                negotiation_store::get_by_negotiation_id(&conn, &neg_id)
                    .map_err(|e| e.to_string())
            },
        )
        .await
        .map_err(|e| format!("materialize lookup panicked: {e}"))??
    };
    let neg = match neg {
        Some(n) => n,
        None => return Ok(None),
    };
    if neg.state != super::domain::NegotiationState::Confirmed {
        return Ok(None);
    }
    if neg.confirmed_commitment_id.is_some() {
        return Ok(None);
    }
    let confirmed_slot = match neg
        .slots
        .iter()
        .find(|s| matches!(s.status, SlotStatus::Confirmed))
    {
        Some(s) => s,
        None => {
            tracing::warn!(
                negotiation_id = %neg.negotiation_id,
                "state=Confirmed but no slot has SlotStatus::Confirmed; skipping materialise"
            );
            return Ok(None);
        }
    };

    // Build the commitment. The organiser semantics in spec/v0.1.md §3.1
    // pin to the initiator of the negotiation; the responder is just
    // an attendee. Both sides land as ATTENDEE rows with PARTSTAT
    // ACCEPTED, since by the time we hit Confirmed the slot is
    // mutually agreed.
    let our_email = own_email_for_thread(&neg);
    let now = chrono::Utc::now();
    let organizer_email = match neg.thread_role {
        ThreadRole::Initiator => our_email.clone(),
        ThreadRole::Responder => neg.counterparty_email.clone(),
    };
    let commitment_id = Uuid::new_v4().to_string();
    let commitment = Commitment {
        id: commitment_id.clone(),
        // UID derived from the negotiation_id so a future re-confirm
        // (rare; would only happen via a Phase 2.5+ recovery path)
        // upserts in place rather than producing a duplicate row.
        uid: format!("tp:{}", neg.negotiation_id),
        sequence: 0,
        summary: neg
            .display_summary
            .clone()
            .or_else(|| Some(format!("Termin mit {}", neg.counterparty_email))),
        description: Some(format!(
            "Aus Negotiation {} ({}) materialisiert.",
            neg.negotiation_id,
            neg.thread_role.as_str()
        )),
        location: None,
        start_at: confirmed_slot.start_at.clone(),
        end_at: confirmed_slot.end_at.clone(),
        original_tzid: None,
        organizer: Some(IcsParticipant {
            email: organizer_email.clone(),
            display_name: None,
            partstat: None,
        }),
        attendees: vec![
            CommitmentAttendee {
                email: our_email,
                display_name: None,
                partstat: Some("ACCEPTED".into()),
            },
            CommitmentAttendee {
                email: neg.counterparty_email.clone(),
                display_name: neg.counterparty_name.clone(),
                partstat: Some("ACCEPTED".into()),
            },
        ],
        source: CommitmentSource::Negotiation,
        status: CommitmentStatus::Confirmed,
        last_published_sequence: None,
        source_message_id: None,
        // Negotiated events are stand-alone — RRULE-series imports live
        // strictly in the file-import path.
        series_uid: None,
        subscription_id: None,
        created_at: now,
        updated_at: now,
    };

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpsertCommitment {
            commitment,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db upsert commitment: {e}"))?;

    // Link the commitment back to the negotiation row so the UI can
    // jump from one to the other.
    let mut linked = neg.clone();
    linked.confirmed_commitment_id = Some(commitment_id.clone());
    linked.updated_at = now;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::ApplyNegotiationUpdate {
            negotiation: linked,
            new_message: None,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db link commitment: {e}"))?;

    // Push to the IMAP calendar folder if Phase-2 sync is enabled —
    // same path as a manual edit through `cal_create`.
    super::sync::spawn_background_sync(app, "tp_confirm_materialize");
    Ok(Some(commitment_id))
}

/// We don't carry our own email on the negotiation row (it would be
/// redundant — the protocol identifies us as "the side that isn't the
/// counterparty"). The materialise helper needs it, though, to fill
/// the `ATTENDEE` row. Derived from the thread role + counterparty:
///
///   - Initiator: we are NOT the counterparty; check the latest
///     outbound message in the log and take its `from` field.
///   - Responder: same — earliest outbound message reflects our
///     identity at the time we replied.
///
/// Falling back to an empty string is acceptable as last resort; the
/// commitment row stays valid, the ATTENDEE entry just lacks our
/// address. UI shows the counterparty regardless.
fn own_email_for_thread(neg: &Negotiation) -> String {
    neg.messages
        .iter()
        .find(|m| matches!(m.direction, MessageDirection::Outbound))
        .and_then(|m| {
            m.envelope
                .get("from")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

// ─── Phase 3+: third-party iCal subscriptions ────────────────────────────

use super::subscriptions::{
    CalendarSubscription, RefreshReport, SubscriptionSource,
};

fn subscription_store(
    app: &AppHandle,
) -> Result<std::sync::Arc<super::subscriptions::SubscriptionStore>, String> {
    app.state::<AppState>()
        .subscription_store
        .get()
        .cloned()
        .ok_or_else(|| "subscription store not ready".to_string())
}

/// Snapshot of the user's subscriptions for the settings panel.
#[tauri::command]
pub async fn cal_subs_list(app: AppHandle) -> Result<Vec<CalendarSubscription>, String> {
    Ok(subscription_store(&app)?.list().await)
}

/// Add a new subscription. The first refresh runs in the background via
/// the periodic task; the caller gets the freshly-minted record back so
/// the UI can show it with `last_refreshed = null` until then. (Tighten
/// this later if the UX warrants kicking an immediate refresh.)
#[tauri::command]
pub async fn cal_subs_add(
    app: AppHandle,
    name: String,
    source: SubscriptionSource,
    refresh_interval_minutes: u32,
) -> Result<CalendarSubscription, String> {
    let store = subscription_store(&app)?;
    let sub = store.add(name, source, refresh_interval_minutes).await?;
    // Kick a refresh so the user doesn't sit staring at "no events yet"
    // for up to a minute. Result is ignored — the report goes into the
    // record's last_error / last_event_count for the UI to display.
    let id = sub.id.clone();
    tauri::async_runtime::spawn(async move {
        store.refresh(&id).await;
    });
    Ok(sub)
}

#[tauri::command]
pub async fn cal_subs_remove(app: AppHandle, id: String) -> Result<(), String> {
    subscription_store(&app)?.remove(&id).await
}

#[tauri::command]
pub async fn cal_subs_set_enabled(
    app: AppHandle,
    id: String,
    enabled: bool,
) -> Result<CalendarSubscription, String> {
    subscription_store(&app)?.set_enabled(&id, enabled).await
}

#[tauri::command]
pub async fn cal_subs_set_interval(
    app: AppHandle,
    id: String,
    minutes: u32,
) -> Result<CalendarSubscription, String> {
    subscription_store(&app)?.set_interval(&id, minutes).await
}

/// Manual-refresh button for a single subscription. Returns a status
/// report instead of `Err` for normal "fetch failed" cases so the UI
/// can show "✗ HTTP 503" without a modal.
#[tauri::command]
pub async fn cal_subs_refresh(app: AppHandle, id: String) -> Result<RefreshReport, String> {
    Ok(subscription_store(&app)?.refresh(&id).await)
}

/// Refresh every enabled subscription whose interval has elapsed. Used
/// by the "Sync all" toolbar button.
#[tauri::command]
pub async fn cal_subs_refresh_all(app: AppHandle) -> Result<Vec<RefreshReport>, String> {
    Ok(subscription_store(&app)?.refresh_all_due().await)
}

/// Set the per-calendar tint shown on event bars in the week/month
/// views. Expects a `#rrggbb` hex string; rejects anything else so the
/// UI's contrast assumptions hold.
#[tauri::command]
pub async fn cal_subs_set_color(
    app: AppHandle,
    id: String,
    color: String,
) -> Result<CalendarSubscription, String> {
    subscription_store(&app)?.set_color(&id, color).await
}
