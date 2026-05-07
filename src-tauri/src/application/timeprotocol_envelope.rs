// Mail-layer boundary for the Phase-3 timeProtocol-envelope wire format.
//
// This module is the **fourth** allowed Mail-layer entry point for the
// `timeprotocol` module — alongside `attachments::bytes` (Phase 0/1
// invitation read), the ComposeDraft attachment surface (Phase 0
// outbound iMIP), and `calendar_imap` (Phase 2 IMAP-folder sync). Like
// those siblings, it stays narrow: read the bytes of an
// `application/time-protocol+json` attachment, hand back a parsed
// `Envelope`. The state engine + persistence layer live in
// `timeprotocol::negotiation_engine` and `negotiation_store`.

use crate::application::attachments;
use crate::domain::message::MessageId;
use crate::infrastructure::db::DbHandle;
use crate::timeprotocol::domain::Envelope;

/// Recognized as a timeProtocol envelope when the MIME type matches
/// `application/time-protocol+json` (case-insensitive, parameters
/// stripped). Caller usually filters attachments before invoking us.
pub fn is_envelope_mime(mime: &str) -> bool {
    let trimmed = mime
        .split(';')
        .next()
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    trimmed == "application/time-protocol+json"
}

/// Read the bytes of a single attachment by `(message_id, part_idx)`
/// and parse them as a timeProtocol envelope. Returns the parsed
/// `Envelope` or an error string suitable for surfacing to the user.
///
/// Validates only the *shape* (seven required fields per MVP profile
/// §3); semantic checks against the negotiation state happen later in
/// the engine.
pub fn parse_envelope_from_attachment(
    db: &DbHandle,
    message_id: &MessageId,
    part_idx: u32,
) -> Result<Envelope, String> {
    let (bytes, _filename, _mime) = attachments::bytes(db, message_id, part_idx)?;
    parse_envelope_bytes(&bytes)
}

/// Pure parse helper — useful for tests and for the inbound IMAP-sync
/// path (when timeBank starts pushing envelopes to the Calendar folder
/// directly, we'll want to parse without going through `attachments`).
pub fn parse_envelope_bytes(bytes: &[u8]) -> Result<Envelope, String> {
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|e| format!("envelope JSON parse: {e}"))?;
    validate_required_fields(&envelope)?;
    Ok(envelope)
}

/// MVP profile §3 envelope rules: the seven fields must all be present
/// and non-empty (`payload` may be `null` for some actions but the
/// field itself must exist; serde guarantees that already).
fn validate_required_fields(envelope: &Envelope) -> Result<(), String> {
    if envelope.message_id.trim().is_empty() {
        return Err("envelope.message_id is empty".into());
    }
    if envelope.from.trim().is_empty() {
        return Err("envelope.from is empty".into());
    }
    if envelope.to.trim().is_empty() {
        return Err("envelope.to is empty".into());
    }
    if envelope.negotiation_id.trim().is_empty() {
        return Err("envelope.negotiation_id is empty".into());
    }
    if envelope.timestamp.trim().is_empty() {
        return Err("envelope.timestamp is empty".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_envelope_mime() {
        assert!(is_envelope_mime("application/time-protocol+json"));
        assert!(is_envelope_mime("APPLICATION/TIME-PROTOCOL+JSON"));
        assert!(is_envelope_mime("application/time-protocol+json; charset=utf-8"));
        assert!(!is_envelope_mime("application/json"));
        assert!(!is_envelope_mime("text/calendar"));
    }

    #[test]
    fn parses_well_formed_envelope() {
        let raw = br#"{
            "messageId": "alice@example.com:msg-001",
            "from": "alice@example.com",
            "to": "bob@example.com",
            "negotiationId": "alice@example.com:neg-42",
            "action": "request",
            "timestamp": "2026-04-23T08:00:00+02:00",
            "payload": {
                "duration": "PT45M",
                "summary": "Project sync"
            }
        }"#;
        let env = parse_envelope_bytes(raw).expect("parse ok");
        assert_eq!(env.message_id, "alice@example.com:msg-001");
        assert_eq!(env.from, "alice@example.com");
        assert_eq!(env.negotiation_id, "alice@example.com:neg-42");
    }

    #[test]
    fn rejects_envelope_with_missing_field() {
        let raw = br#"{
            "messageId": "",
            "from": "alice@example.com",
            "to": "bob@example.com",
            "negotiationId": "alice@example.com:neg-42",
            "action": "request",
            "timestamp": "2026-04-23T08:00:00+02:00",
            "payload": {}
        }"#;
        assert!(parse_envelope_bytes(raw).is_err());
    }

    #[test]
    fn rejects_unknown_action() {
        let raw = br#"{
            "messageId": "x:1",
            "from": "a@b",
            "to": "c@d",
            "negotiationId": "x:n",
            "action": "delete_universe",
            "timestamp": "2026-04-23T08:00:00+02:00",
            "payload": {}
        }"#;
        assert!(parse_envelope_bytes(raw).is_err());
    }
}
