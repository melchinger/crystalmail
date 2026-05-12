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
    /// VEVENT STATUS, normalized to uppercase. ADR-0011 §3 requires this
    /// field for the carriage profile (CONFIRMED for active commitments,
    /// CANCELLED for tombstone mutations). `None` when the source ICS
    /// did not carry STATUS (legacy invitations from non-profile senders).
    #[serde(default)]
    pub status: Option<String>,
    /// Raw RRULE property value when present (the part *after* `RRULE:`,
    /// e.g. `FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10`). Drives the on-import
    /// expansion of the series into individual occurrences. `None` for
    /// stand-alone events. RDATE/EXDATE/RECURRENCE-ID overrides are
    /// intentionally not surfaced — Phase 3+ scope is plain RRULE.
    #[serde(default)]
    pub rrule: Option<String>,
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
    /// Highest SEQUENCE ever observed in the IMAP folder for this UID.
    /// `None` means "never seen on IMAP" (Phase-1 row pre-Phase-2 sync,
    /// or a fresh local create not yet published). A `Some(n)` value
    /// lets the sync diff distinguish:
    ///   * "fresh local, needs initial publish" (None)
    ///   * "was published, then server-side hard-deleted" (Some(n) and
    ///      local.sequence == n) — accept as cancellation, don't republish
    ///   * "was published, user has edited locally since" (Some(n) and
    ///      local.sequence > n) — publish the update
    /// ADR-0011 §5 doesn't normatively cover the manual-mail-delete
    /// case; this field is CrystalMail's pragmatic recovery mechanism.
    /// See `external-contributions/2026-05-07-…` in the timeProtocol
    /// repo for the proposed ADR clarification.
    #[serde(default)]
    pub last_published_sequence: Option<u32>,
    /// Set when `source == IcsImport`: the message id this commitment was
    /// imported from. Not an FK in the db schema (the message may be
    /// deleted later, the commitment should survive).
    pub source_message_id: Option<String>,
    /// RRULE-expansion marker. When `Some(master_uid)`, this row is one
    /// individual occurrence of a recurring series — its own `uid` is a
    /// synthetic `${series_uid}@${dtstart_iso}` and a "cancel whole
    /// series" UI action cascade-deletes everything that shares this
    /// value. `None` means a stand-alone event (manual create or a
    /// singleton import).
    #[serde(default)]
    pub series_uid: Option<String>,
    /// Subscribed-calendar marker. Set on rows that come from a
    /// third-party iCal subscription overlay (read-only, never written
    /// to the `commitments` table — surfaces only through the in-memory
    /// merge in `cal_list_in_range`). The frontend keys read-only UI
    /// off this field. `None` for everything that lives in SQLite.
    #[serde(default)]
    pub subscription_id: Option<String>,
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

// ─── Phase 3: negotiation domain (timeProtocol v0.1 §5 + MVP profile) ────
//
// One thread = one `Negotiation` row + N `NegotiationSlot` children + N
// `NegotiationMessage` children. The state machine (`Requested → Proposed
// → Confirmed | Released | Expired`) lives on the negotiation; per-slot
// state (Active | Inactive | Confirmed | Released) lives on each slot
// because one thread can carry multiple parallel proposals per the MVP
// profile §4.2 ("A negotiation may have multiple active proposed slots;
// confirmed applies to exactly one slot_id; remaining proposals become
// inactive").

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRole {
    /// We sent the original `request` envelope; we're waiting for
    /// proposals from the counterparty.
    Initiator,
    /// We received a request; we owe the counterparty proposals.
    Responder,
}

impl ThreadRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadRole::Initiator => "initiator",
            ThreadRole::Responder => "responder",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "initiator" => Some(ThreadRole::Initiator),
            "responder" => Some(ThreadRole::Responder),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegotiationState {
    Requested,
    Proposed,
    Held,
    Confirmed,
    Released,
    Expired,
}

impl NegotiationState {
    pub fn as_str(self) -> &'static str {
        match self {
            NegotiationState::Requested => "requested",
            NegotiationState::Proposed => "proposed",
            NegotiationState::Held => "held",
            NegotiationState::Confirmed => "confirmed",
            NegotiationState::Released => "released",
            NegotiationState::Expired => "expired",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "requested" => Some(NegotiationState::Requested),
            "proposed" => Some(NegotiationState::Proposed),
            "held" => Some(NegotiationState::Held),
            "confirmed" => Some(NegotiationState::Confirmed),
            "released" => Some(NegotiationState::Released),
            "expired" => Some(NegotiationState::Expired),
            _ => None,
        }
    }
    /// MVP profile §4.2: confirmed/released/expired are terminal.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            NegotiationState::Confirmed
                | NegotiationState::Released
                | NegotiationState::Expired
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    /// Slot is on the table — counterparty (or us) can pick it.
    Active,
    /// Another slot in the same thread became `confirmed`; this one
    /// loses by virtue of the MVP-profile single-confirmation rule.
    Inactive,
    /// The winning slot for the thread.
    Confirmed,
    /// Proposer withdrew this specific slot via a `release_slot` message.
    Released,
}

impl SlotStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SlotStatus::Active => "active",
            SlotStatus::Inactive => "inactive",
            SlotStatus::Confirmed => "confirmed",
            SlotStatus::Released => "released",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(SlotStatus::Active),
            "inactive" => Some(SlotStatus::Inactive),
            "confirmed" => Some(SlotStatus::Confirmed),
            "released" => Some(SlotStatus::Released),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegotiationAction {
    Request,
    Propose,
    CounterPropose,
    Confirm,
    Release,
    /// Optional in MVP profile: declares the sender protects the slot
    /// locally. Recipient is *not* required to mirror — it's
    /// informational. CrystalMail accepts incoming `hold` and ignores
    /// it (no state change); we don't emit `hold` on the wire.
    Hold,
}

impl NegotiationAction {
    pub fn as_str(self) -> &'static str {
        match self {
            NegotiationAction::Request => "request",
            NegotiationAction::Propose => "propose",
            NegotiationAction::CounterPropose => "counter_propose",
            NegotiationAction::Confirm => "confirm",
            NegotiationAction::Release => "release",
            NegotiationAction::Hold => "hold",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "request" => Some(NegotiationAction::Request),
            "propose" => Some(NegotiationAction::Propose),
            "counter_propose" => Some(NegotiationAction::CounterPropose),
            "confirm" => Some(NegotiationAction::Confirm),
            "release" => Some(NegotiationAction::Release),
            "hold" => Some(NegotiationAction::Hold),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    Inbound,
    Outbound,
}

impl MessageDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageDirection::Inbound => "inbound",
            MessageDirection::Outbound => "outbound",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "inbound" => Some(MessageDirection::Inbound),
            "outbound" => Some(MessageDirection::Outbound),
            _ => None,
        }
    }
}

/// Constraints accompanying a `request`. All optional (MVP profile §4.1).
/// Carried verbatim from envelope to UI; we don't enforce them
/// server-side beyond surfacing them when a `propose` violates one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NegotiationConstraints {
    /// Slot must start before this time. RFC 3339 with offset.
    #[serde(default)]
    pub latest: Option<String>,
    /// Free-form hint from the requester ("morning", "after lunch"…).
    /// Not machine-enforced in MVP.
    #[serde(default)]
    pub preferred_time: Option<String>,
    /// ISO 8601 duration. Slot start must be at least this far in the
    /// future relative to the request's timestamp.
    #[serde(default)]
    pub minimum_notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NegotiationSlot {
    /// Proposer-owned compound identifier `<node_id>:<local_id>`.
    pub slot_id: String,
    pub proposer_node_id: String,
    pub start_at: String,
    pub end_at: String,
    pub status: SlotStatus,
    pub proposed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NegotiationMessage {
    pub message_id: String,
    pub direction: MessageDirection,
    pub action: NegotiationAction,
    /// Full envelope JSON for replay / debug. Surfaced to the frontend
    /// as `serde_json::Value` rather than a strict struct to leave
    /// room for forward-compatible payload extensions.
    pub envelope: serde_json::Value,
    pub source_message_id: Option<String>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Negotiation {
    pub id: String,
    pub negotiation_id: String,
    pub thread_role: ThreadRole,
    pub state: NegotiationState,
    pub duration_iso: Option<String>,
    pub constraints: Option<NegotiationConstraints>,
    pub counterparty_email: String,
    pub counterparty_name: Option<String>,
    pub confirmed_commitment_id: Option<String>,
    pub display_summary: Option<String>,
    pub slots: Vec<NegotiationSlot>,
    pub messages: Vec<NegotiationMessage>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Wire envelope per `docs/api-sketch-v0.1.md` "Cross-node negotiation
/// envelope". All seven fields required (MVP profile §3 envelope rules).
/// `payload` is loose-typed so each action can have its own shape;
/// concrete payload structs live alongside the action helpers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub negotiation_id: String,
    pub action: NegotiationAction,
    /// RFC 3339 with offset. Sender-creation time, not authoritative
    /// ordering — see api-sketch §"Envelope handling rules".
    pub timestamp: String,
    pub payload: serde_json::Value,
}

/// Payload of a `request` action. All fields optional except `duration`
/// (which the responder needs to know how big a slot to propose).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPayload {
    /// ISO 8601 duration. Required for v1.
    pub duration: String,
    #[serde(default)]
    pub constraints: Option<NegotiationConstraints>,
    /// Optional human-readable summary the requester wants the responder
    /// to see ("Project sync about Q3 plan"). Surfaces in the responder's
    /// UI so they know what they're agreeing to.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Payload of a `propose` or `counter_propose` action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposePayload {
    pub slot_id: String,
    pub start_at: String,
    pub end_at: String,
}

/// Payload of `confirm` / `release` / `hold` — they all reference one
/// slot_id. (`hold` is incoming-only in our implementation.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotRefPayload {
    pub slot_id: String,
}
