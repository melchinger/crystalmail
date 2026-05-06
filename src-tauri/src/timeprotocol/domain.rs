// Calendar domain types — both the on-the-wire ICS shape used for
// invitation handling (Phase 0: ParsedIcsEvent + InvitationResponse) and
// the locally stored shape introduced in Phase 1 (Commitment).
//
// Out of scope across all phases-so-far: recurrence (RRULE/EXDATE),
// VTIMEZONE blocks, multi-VEVENT calendars. Recurrence can land later
// without breaking this model — Commitment is intentionally one slot.

use chrono::{DateTime, Utc};
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

// ─── Phase 1: locally stored commitment ──────────────────────────────────
//
// `Commitment` mirrors the timeProtocol v0.1-MVP-Profile §1.3 commitment
// object — the canonical outcome of a confirmed event, our source of truth
// in the local store. We store start/end as RFC 3339 strings (with explicit
// offset, matching the MVP profile's example) rather than chrono timestamps
// so we can round-trip the original wall-clock without lossy conversions
// for events created in foreign timezones. The frontend renders with the
// user's local TZ applied on the display side.

/// Lifecycle state of a commitment, mirroring RFC 5545's STATUS values
/// that ADR-0011 §3 references. `CONFIRMED` is the default; `CANCELLED`
/// is the tombstone state set by the cancel-flow (sequence-bumped
/// mutation, never hard-deleted in Phase 1+, so Phase 2 can still emit
/// the cancellation envelope into IMAP). `TENTATIVE` is reserved — not
/// produced by Phase 1 code paths but accepted as a valid stored value
/// to keep round-tripping foreign ICS imports simple.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum CommitmentStatus {
    Confirmed,
    Cancelled,
    Tentative,
}

impl CommitmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CommitmentStatus::Confirmed => "CONFIRMED",
            CommitmentStatus::Cancelled => "CANCELLED",
            CommitmentStatus::Tentative => "TENTATIVE",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "CONFIRMED" => Some(CommitmentStatus::Confirmed),
            "CANCELLED" => Some(CommitmentStatus::Cancelled),
            "TENTATIVE" => Some(CommitmentStatus::Tentative),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitmentSource {
    /// User typed it directly in the EventEditor.
    Manual,
    /// Imported from a `text/calendar` attachment in a mail (Phase 0/1
    /// "Annehmen"-flow). The originating message id is kept on the row
    /// so we can offer "view source mail" or re-import on user request.
    IcsImport,
    /// Materialised from a confirmed timeProtocol negotiation (Phase 3+).
    /// Not produced in Phase 1, but the enum slot stays so we don't
    /// migrate later.
    Negotiation,
}

impl CommitmentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CommitmentSource::Manual => "manual",
            CommitmentSource::IcsImport => "ics_import",
            CommitmentSource::Negotiation => "negotiation",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(CommitmentSource::Manual),
            "ics_import" => Some(CommitmentSource::IcsImport),
            "negotiation" => Some(CommitmentSource::Negotiation),
            _ => None,
        }
    }
}

/// Locally stored attendee row. Mirrors RFC 5545 ATTENDEE — including the
/// PARTSTAT we sent back if we replied to an invitation. The user's own
/// row is one of these (we identify it by email match against the user's
/// account address + aliases at render time, not by a flag).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitmentAttendee {
    pub email: String,
    pub display_name: Option<String>,
    /// RFC 5545 PARTSTAT. `None` for attendees we have no status for
    /// (e.g. participants in a manual event we created).
    pub partstat: Option<String>,
}

/// One stored commitment. Identifier discipline:
///   - `id` is the local UUID (our internal handle, stable for the row's
///     lifetime).
///   - `uid` is the RFC 5545 UID (stable across REPLY/UPDATE cycles, may
///     be shared with foreign calendars when imported via ICS).
/// Two distinct fields because we treat the row as the local truth even
/// if the upstream calendar's UID changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commitment {
    pub id: String,
    pub uid: String,
    pub sequence: u32,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    /// RFC 3339 with explicit offset, e.g. `2026-04-23T09:00:00+02:00`.
    pub start_at: String,
    pub end_at: String,
    /// Original RFC 5545 TZID string (e.g. `Europe/Berlin`) when the
    /// import had one. Kept for ICS re-export round-trip; rendering
    /// always uses the offset embedded in `start_at`/`end_at`.
    pub original_tzid: Option<String>,
    pub organizer: Option<IcsParticipant>,
    pub attendees: Vec<CommitmentAttendee>,
    pub source: CommitmentSource,
    /// RFC 5545 STATUS — CONFIRMED for active events, CANCELLED for
    /// tombstones, TENTATIVE for foreign-ICS imports that carried that
    /// status. The list/range queries filter CANCELLED out by default.
    #[serde(default = "default_status")]
    pub status: CommitmentStatus,
    /// Set when `source == IcsImport`: the message id this commitment was
    /// imported from. Not an FK in the db schema (the message may be
    /// deleted later, the commitment should survive).
    pub source_message_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_status() -> CommitmentStatus {
    CommitmentStatus::Confirmed
}

/// Form payload from the frontend for create/update. The id is server-
/// allocated on create (None) and required on update (Some). UID is
/// auto-generated on create as well; on update it's read-only (would
/// break ICS round-trip with the originator if it changed).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitmentDraft {
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start_at: String,
    pub end_at: String,
    pub original_tzid: Option<String>,
    pub organizer: Option<IcsParticipant>,
    #[serde(default)]
    pub attendees: Vec<CommitmentAttendee>,
}
