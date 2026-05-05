// Calendar invitation handling — Phase 0.
//
// Scope: parse a single VEVENT out of an `text/calendar` attachment so the
// Reader can show "<who> lädt zu <was> am <wann> ein" and let the user answer
// with an RFC 5546 REPLY. We deliberately do not store events anywhere; this
// module only models the on-the-wire shape needed for display + reply.
//
// Out of scope for Phase 0: recurrence (RRULE/EXDATE), VTIMEZONE blocks,
// attachments inside the VEVENT, multi-VEVENT calendars, anything outside
// METHOD:REQUEST. Those land in Phase 1+ when we get a local store and an
// actual calendar view.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IcsParticipant {
    pub email: String,
    pub display_name: Option<String>,
}

/// Single parsed VEVENT, sufficient to render an invitation banner and to
/// produce a REPLY for the same UID/SEQUENCE pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedIcsEvent {
    /// VCALENDAR-level METHOD. We treat anything other than `REQUEST` as
    /// "informational only" in the UI — no Annehmen/Ablehnen buttons.
    pub method: Option<String>,
    /// Stable event identifier. Matches between REQUEST and REPLY.
    pub uid: String,
    /// Monotonic revision of the event from the organizer side.
    pub sequence: u32,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    /// Raw RFC 5545 timestamp string (with TZID prefix or trailing Z), kept
    /// verbatim so a REPLY round-trips byte-identical timing back to the
    /// organizer. Display formatting happens in the frontend with the user's
    /// local timezone applied at render time.
    pub dtstart: Option<String>,
    pub dtend: Option<String>,
    pub organizer: Option<IcsParticipant>,
    pub attendees: Vec<IcsParticipant>,
    /// True when the event has at least one attendee. Drives the visibility
    /// of the response buttons — a calendar broadcast (no attendees) cannot
    /// meaningfully be replied to.
    pub is_invitation: bool,
}

/// Outgoing reply intent. Maps 1:1 to RFC 5545 PARTSTAT values.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvitationResponse {
    Accepted,
    Tentative,
    Declined,
}

impl InvitationResponse {
    /// PARTSTAT property value as it must appear in the REPLY ICS.
    pub fn partstat(self) -> &'static str {
        match self {
            InvitationResponse::Accepted => "ACCEPTED",
            InvitationResponse::Tentative => "TENTATIVE",
            InvitationResponse::Declined => "DECLINED",
        }
    }
}
