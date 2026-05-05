// Tauri command adapters for the Phase 0 calendar feature: parse a
// `text/calendar` attachment of an open message and prepare an RFC 5546 REPLY
// the user can send back to the organizer.
//
// Phase 0 is intentionally storage-less. We do not persist events, do not run
// migrations, do not project anything to the IMAP folder. The reply ICS is
// dropped into a temp file purely so the existing Compose attachment pipeline
// (which expects a path on disk) can pick it up.

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::application::attachments;
use crate::application::ics;
use crate::domain::calendar::{InvitationResponse, ParsedIcsEvent};
use crate::domain::message::MessageId;
use crate::state::AppState;

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
