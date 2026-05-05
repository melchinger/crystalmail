// iCalendar parsing + REPLY building for invitation-handling.
//
// Parser is the third-party `ical` crate. The reply builder is hand-rolled —
// the REPLY shape is small and well-defined, and writing it ourselves avoids
// pulling another writer dependency just to emit ~12 lines of CRLF text.

use std::io::Cursor;

use chrono::Utc;
use ical::parser::ical::component::IcalEvent;
use ical::IcalParser;

use crate::domain::calendar::{IcsParticipant, InvitationResponse, ParsedIcsEvent};

/// Parse the first VEVENT out of an iCalendar byte stream. Returns `Ok(None)`
/// when the input is well-formed iCalendar but contains no events (e.g. an
/// VFREEBUSY-only blob, or a CANCEL whose VEVENT was stripped). Returns `Err`
/// for genuinely broken input the parser refuses.
pub fn parse(raw: &[u8]) -> Result<Option<ParsedIcsEvent>, String> {
    let cursor = Cursor::new(raw);
    let mut parser = IcalParser::new(cursor);
    let Some(first) = parser.next() else {
        return Ok(None);
    };
    let cal = first.map_err(|e| format!("ical parse: {e}"))?;
    let method = first_property(&cal.properties, "METHOD");
    let Some(event) = cal.events.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(event_to_parsed(event, method)))
}

fn event_to_parsed(event: IcalEvent, method: Option<String>) -> ParsedIcsEvent {
    let uid = first_property(&event.properties, "UID").unwrap_or_default();
    let sequence = first_property(&event.properties, "SEQUENCE")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let summary = first_property(&event.properties, "SUMMARY");
    let description = first_property(&event.properties, "DESCRIPTION");
    let location = first_property(&event.properties, "LOCATION");
    let dtstart = first_property(&event.properties, "DTSTART");
    let dtend = first_property(&event.properties, "DTEND");
    let organizer = first_participant(&event.properties, "ORGANIZER");
    let attendees = all_participants(&event.properties, "ATTENDEE");
    let is_invitation = !attendees.is_empty();
    ParsedIcsEvent {
        method,
        uid,
        sequence,
        summary,
        description,
        location,
        dtstart,
        dtend,
        organizer,
        attendees,
        is_invitation,
    }
}

fn first_property(props: &[ical::property::Property], name: &str) -> Option<String> {
    props
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .and_then(|p| p.value.clone())
}

fn first_participant(
    props: &[ical::property::Property],
    name: &str,
) -> Option<IcsParticipant> {
    props
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .map(participant_from_property)
}

fn all_participants(
    props: &[ical::property::Property],
    name: &str,
) -> Vec<IcsParticipant> {
    props
        .iter()
        .filter(|p| p.name.eq_ignore_ascii_case(name))
        .map(participant_from_property)
        .collect()
}

fn participant_from_property(prop: &ical::property::Property) -> IcsParticipant {
    let email = prop
        .value
        .as_deref()
        .map(strip_mailto)
        .unwrap_or_default()
        .to_string();
    let display_name = prop.params.as_ref().and_then(|params| {
        params.iter().find_map(|(key, vals)| {
            if key.eq_ignore_ascii_case("CN") {
                vals.first().cloned()
            } else {
                None
            }
        })
    });
    IcsParticipant {
        email,
        display_name,
    }
}

fn strip_mailto(s: &str) -> &str {
    let trimmed = s.trim();
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("mailto:") {
        &trimmed[7..]
    } else {
        trimmed
    }
}

/// Build an RFC 5546 REPLY iCalendar for a single VEVENT. The output is a
/// complete VCALENDAR with `METHOD:REPLY`, suitable for attaching to the
/// outgoing mail back to the organizer.
///
/// The REPLY echoes UID, SEQUENCE, DTSTART, DTEND, SUMMARY, ORGANIZER from
/// the original (RFC 5546 §3.2.3 — the responder must keep the organizer's
/// identification of the event intact) and contributes a single ATTENDEE
/// line with our PARTSTAT.
pub fn build_reply(
    original: &ParsedIcsEvent,
    response: InvitationResponse,
    attendee_email: &str,
    attendee_name: Option<&str>,
) -> String {
    let mut out = String::new();
    push_line(&mut out, "BEGIN:VCALENDAR");
    push_line(&mut out, "VERSION:2.0");
    push_line(&mut out, "PRODID:-//CrystalMail//Calendar Phase 0//EN");
    push_line(&mut out, "METHOD:REPLY");
    push_line(&mut out, "BEGIN:VEVENT");

    let uid_line = format!("UID:{}", escape_text(&original.uid));
    push_folded(&mut out, &uid_line);

    push_line(&mut out, &format!("SEQUENCE:{}", original.sequence));
    push_line(
        &mut out,
        &format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
    );

    if let Some(dtstart) = &original.dtstart {
        push_folded(&mut out, &format!("DTSTART:{}", dtstart));
    }
    if let Some(dtend) = &original.dtend {
        push_folded(&mut out, &format!("DTEND:{}", dtend));
    }
    if let Some(summary) = &original.summary {
        push_folded(&mut out, &format!("SUMMARY:{}", escape_text(summary)));
    }
    if let Some(org) = &original.organizer {
        push_folded(&mut out, &organizer_line(org));
    }

    push_folded(
        &mut out,
        &attendee_line(attendee_email, attendee_name, response.partstat()),
    );

    push_line(&mut out, "END:VEVENT");
    push_line(&mut out, "END:VCALENDAR");
    out
}

fn organizer_line(org: &IcsParticipant) -> String {
    match org.display_name.as_deref().filter(|s| !s.is_empty()) {
        Some(cn) => format!(
            "ORGANIZER;CN={}:mailto:{}",
            escape_param(cn),
            org.email,
        ),
        None => format!("ORGANIZER:mailto:{}", org.email),
    }
}

fn attendee_line(email: &str, name: Option<&str>, partstat: &str) -> String {
    let cn = name.filter(|s| !s.is_empty());
    match cn {
        Some(name) => format!(
            "ATTENDEE;CN={};PARTSTAT={};RSVP=FALSE:mailto:{}",
            escape_param(name),
            partstat,
            email,
        ),
        None => format!(
            "ATTENDEE;PARTSTAT={};RSVP=FALSE:mailto:{}",
            partstat, email,
        ),
    }
}

fn push_line(buf: &mut String, line: &str) {
    buf.push_str(line);
    buf.push_str("\r\n");
}

/// RFC 5545 §3.1: lines must be ≤75 octets; longer ones are folded by
/// inserting CRLF + a single whitespace continuation. We fold on byte
/// boundaries that don't split a UTF-8 codepoint.
fn push_folded(buf: &mut String, line: &str) {
    const LIMIT: usize = 75;
    if line.len() <= LIMIT {
        push_line(buf, line);
        return;
    }
    let bytes = line.as_bytes();
    let mut start = 0;
    let mut first = true;
    while start < bytes.len() {
        let max = if first { LIMIT } else { LIMIT - 1 };
        let mut end = (start + max).min(bytes.len());
        // Walk back to a UTF-8 codepoint boundary.
        while end > start && (bytes[end - 1] & 0xC0) == 0x80 {
            end -= 1;
        }
        if !first {
            buf.push(' ');
        }
        buf.push_str(&line[start..end]);
        buf.push_str("\r\n");
        start = end;
        first = false;
    }
}

fn escape_text(s: &str) -> String {
    // RFC 5545 §3.3.11: escape backslash, semicolon, comma; convert newlines
    // to literal "\n".
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn escape_param(s: &str) -> String {
    // Parameter values containing ":", ";", "," or whitespace must be
    // double-quoted. Quotes themselves get stripped (RFC 5545 §3.2 reserves
    // quote as a delimiter, no in-band escape).
    let needs_quote = s
        .chars()
        .any(|c| matches!(c, ':' | ';' | ',') || c.is_whitespace());
    let stripped: String = s.chars().filter(|c| *c != '"').collect();
    if needs_quote {
        format!("\"{stripped}\"")
    } else {
        stripped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_REQUEST: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
METHOD:REQUEST\r\n\
PRODID:-//Test//EN\r\n\
BEGIN:VEVENT\r\n\
UID:abc123@example.com\r\n\
SEQUENCE:1\r\n\
DTSTAMP:20260423T080000Z\r\n\
DTSTART:20260423T090000Z\r\n\
DTEND:20260423T100000Z\r\n\
SUMMARY:Project sync\r\n\
LOCATION:Room 4\r\n\
ORGANIZER;CN=Alice:mailto:alice@example.com\r\n\
ATTENDEE;CN=Bob;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.com\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn parses_basic_request() {
        let parsed = parse(SAMPLE_REQUEST.as_bytes())
            .expect("parse ok")
            .expect("event present");
        assert_eq!(parsed.method.as_deref(), Some("REQUEST"));
        assert_eq!(parsed.uid, "abc123@example.com");
        assert_eq!(parsed.sequence, 1);
        assert_eq!(parsed.summary.as_deref(), Some("Project sync"));
        assert_eq!(parsed.location.as_deref(), Some("Room 4"));
        assert_eq!(parsed.dtstart.as_deref(), Some("20260423T090000Z"));
        assert_eq!(
            parsed.organizer.as_ref().map(|p| p.email.as_str()),
            Some("alice@example.com")
        );
        assert_eq!(
            parsed.organizer.as_ref().and_then(|p| p.display_name.as_deref()),
            Some("Alice")
        );
        assert_eq!(parsed.attendees.len(), 1);
        assert!(parsed.is_invitation);
    }

    #[test]
    fn empty_input_returns_none() {
        let parsed = parse(b"").expect("parse ok");
        assert!(parsed.is_none());
    }

    #[test]
    fn build_reply_keeps_uid_and_sequence() {
        let parsed = parse(SAMPLE_REQUEST.as_bytes()).unwrap().unwrap();
        let reply = build_reply(
            &parsed,
            InvitationResponse::Accepted,
            "bob@example.com",
            Some("Bob"),
        );
        assert!(reply.contains("METHOD:REPLY"));
        assert!(reply.contains("UID:abc123@example.com"));
        assert!(reply.contains("SEQUENCE:1"));
        assert!(reply.contains("PARTSTAT=ACCEPTED"));
        assert!(reply.contains("ATTENDEE"));
        assert!(reply.contains(":mailto:bob@example.com"));
        assert!(reply.contains("ORGANIZER"));
        assert!(reply.contains(":mailto:alice@example.com"));
        // CRLF line endings.
        assert!(reply.contains("\r\n"));
        // Round-tripped DTSTART/DTEND verbatim.
        assert!(reply.contains("DTSTART:20260423T090000Z"));
        assert!(reply.contains("DTEND:20260423T100000Z"));
    }

    #[test]
    fn build_reply_declined_uses_correct_partstat() {
        let parsed = parse(SAMPLE_REQUEST.as_bytes()).unwrap().unwrap();
        let reply = build_reply(
            &parsed,
            InvitationResponse::Declined,
            "bob@example.com",
            None,
        );
        assert!(reply.contains("PARTSTAT=DECLINED"));
        // No CN= when name is absent.
        assert!(!reply.contains("CN=;"));
    }

    #[test]
    fn folds_long_lines_at_75_octets() {
        // Construct a UID longer than 75 chars to force folding.
        let long_uid = "x".repeat(120);
        let mut original = parse(SAMPLE_REQUEST.as_bytes()).unwrap().unwrap();
        original.uid = long_uid.clone();
        let reply = build_reply(
            &original,
            InvitationResponse::Accepted,
            "bob@example.com",
            None,
        );
        // Each line in the reply must be ≤75 octets (continuation lines
        // include the leading space).
        for line in reply.split("\r\n") {
            assert!(
                line.len() <= 75,
                "line too long ({} chars): {line:?}",
                line.len()
            );
        }
    }

    #[test]
    fn escapes_special_chars_in_summary() {
        let mut original = parse(SAMPLE_REQUEST.as_bytes()).unwrap().unwrap();
        original.summary = Some("a; b, c \\ d".to_string());
        let reply = build_reply(
            &original,
            InvitationResponse::Tentative,
            "bob@example.com",
            None,
        );
        assert!(reply.contains("SUMMARY:a\\; b\\, c \\\\ d"));
    }
}
