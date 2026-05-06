// iCalendar parsing + REPLY/REQUEST building.
//
// Parser is the third-party `ical` crate. Builders (REPLY for invitation
// responses; REQUEST for export-as-ICS of a stored commitment) are
// hand-rolled — the shapes are small and well-defined, and writing them
// ourselves avoids pulling another writer dependency.
//
// Time-zone handling for Phase 1 (per project memory):
//   * `Z` suffix    → UTC offset
//   * TZID present  → ignore the TZID name, treat the wall clock as local
//                     and apply the system's current local offset. This is
//                     wrong for events spanning DST transitions, but it's
//                     the documented Phase 1 limitation; full IANA-zone
//                     resolution lands later (probably with chrono-tz).
//   * no suffix     → same as TZID — local wall clock, local offset

use std::io::Cursor;

use chrono::{FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use ical::parser::ical::component::IcalEvent;
use ical::IcalParser;
use uuid::Uuid;

use super::domain::{
    Commitment, CommitmentAttendee, CommitmentSource, CommitmentStatus,
    IcsParticipant, InvitationResponse, ParsedIcsEvent,
};

/// PRODID emitted in every CrystalMail-built VCALENDAR. Convention is
/// `-//<org>//<product><version>//EN`. Bumping the version segment as
/// the calendar feature evolves makes downstream-debugging easier and
/// gives the iMIP-receiver-side a hint about which builder produced the
/// payload.
const PRODID: &str = "-//CrystalMail//Calendar 1.0//EN";

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
    let status = first_property(&event.properties, "STATUS").map(|s| s.to_ascii_uppercase());
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
        status,
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
    push_line(&mut out, &format!("PRODID:{PRODID}"));
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

// ─── Phase 1: ICS ↔ Commitment conversion ────────────────────────────────

/// Convert a parsed ICS event into a stored Commitment. Used by the import
/// path (Annehmen + manual import). The caller passes the originating
/// message id (so we can deep-link "view source") and optionally our own
/// PARTSTAT — when set, we mark our attendee row with that response.
pub fn ics_to_commitment(
    parsed: &ParsedIcsEvent,
    source_message_id: Option<String>,
    my_email: Option<&str>,
    my_partstat: Option<InvitationResponse>,
) -> Result<Commitment, String> {
    let dtstart = parsed
        .dtstart
        .as_deref()
        .ok_or("ICS event has no DTSTART")?;
    let dtend = parsed
        .dtend
        .as_deref()
        .ok_or("ICS event has no DTEND")?;

    let (start_at, original_tzid) = ics_time_to_rfc3339(dtstart)?;
    let (end_at, _) = ics_time_to_rfc3339(dtend)?;

    // Mirror the ICS attendee list, stamping our PARTSTAT in place if the
    // caller told us we just responded. We match by email lowercase.
    let lowered = my_email.map(|s| s.to_ascii_lowercase());
    let my_partstat_str = my_partstat.map(|r| r.partstat().to_string());
    let attendees = parsed
        .attendees
        .iter()
        .map(|a| {
            let is_me = lowered
                .as_deref()
                .map(|m| a.email.eq_ignore_ascii_case(m))
                .unwrap_or(false);
            CommitmentAttendee {
                email: a.email.clone(),
                display_name: a.display_name.clone(),
                partstat: if is_me {
                    my_partstat_str.clone()
                } else {
                    None
                },
            }
        })
        .collect::<Vec<_>>();

    let now = Utc::now();
    Ok(Commitment {
        id: Uuid::new_v4().to_string(),
        uid: parsed.uid.clone(),
        sequence: parsed.sequence,
        summary: parsed.summary.clone(),
        description: parsed.description.clone(),
        location: parsed.location.clone(),
        start_at,
        end_at,
        original_tzid,
        organizer: parsed.organizer.clone(),
        attendees,
        source: CommitmentSource::IcsImport,
        status: parsed
            .status
            .as_deref()
            .and_then(CommitmentStatus::from_str)
            .unwrap_or(CommitmentStatus::Confirmed),
        // Fresh import — has not been published from this device yet,
        // so the sync diff treats it as a candidate for initial publish
        // (or recognizes it as already on the IMAP folder if a sync
        // round subsequently sees the same UID present remotely).
        last_published_sequence: None,
        source_message_id,
        created_at: now,
        updated_at: now,
    })
}

/// Build a fresh Commitment from a manual-create form. UID gets a freshly
/// minted UUID so the event can later be exported as ICS without collision.
pub fn manual_commitment(
    summary: Option<String>,
    description: Option<String>,
    location: Option<String>,
    start_at: String,
    end_at: String,
    organizer: Option<IcsParticipant>,
    attendees: Vec<CommitmentAttendee>,
) -> Commitment {
    let now = Utc::now();
    Commitment {
        id: Uuid::new_v4().to_string(),
        uid: format!("{}@crystalmail", Uuid::new_v4()),
        sequence: 0,
        summary,
        description,
        location,
        start_at,
        end_at,
        original_tzid: None,
        organizer,
        attendees,
        source: CommitmentSource::Manual,
        status: CommitmentStatus::Confirmed,
        last_published_sequence: None,
        source_message_id: None,
        created_at: now,
        updated_at: now,
    }
}

/// Build a REQUEST-flavored ICS for a stored commitment. Used by the
/// "export this event" path so the user can share it with non-CrystalMail
/// peers as a plain `.ics` file or attach it manually to a mail.
///
/// Re-uses the line-folding + escape helpers already used by `build_reply`.
pub fn build_ics_for_commitment(c: &Commitment) -> String {
    let mut out = String::new();
    push_line(&mut out, "BEGIN:VCALENDAR");
    push_line(&mut out, "VERSION:2.0");
    push_line(&mut out, &format!("PRODID:{PRODID}"));
    // ADR-0011 §3 mandates METHOD:PUBLISH for the IMAP carriage profile.
    // Same VCALENDAR shape works for the user-driven export-as-file path
    // (Phase 1) and the future IMAP-folder publish path (Phase 2) — keeps
    // the builder single-purposed.
    push_line(&mut out, "METHOD:PUBLISH");
    push_line(&mut out, "BEGIN:VEVENT");

    push_folded(&mut out, &format!("UID:{}", escape_text(&c.uid)));
    push_line(&mut out, &format!("SEQUENCE:{}", c.sequence));
    push_line(
        &mut out,
        &format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
    );

    push_folded(&mut out, &format!("DTSTART:{}", rfc3339_to_ics(&c.start_at)));
    push_folded(&mut out, &format!("DTEND:{}", rfc3339_to_ics(&c.end_at)));

    if let Some(s) = &c.summary {
        push_folded(&mut out, &format!("SUMMARY:{}", escape_text(s)));
    }
    if let Some(d) = &c.description {
        push_folded(&mut out, &format!("DESCRIPTION:{}", escape_text(d)));
    }
    if let Some(l) = &c.location {
        push_folded(&mut out, &format!("LOCATION:{}", escape_text(l)));
    }
    if let Some(org) = &c.organizer {
        push_folded(&mut out, &organizer_line(org));
    }
    for a in &c.attendees {
        push_folded(&mut out, &commitment_attendee_line(a));
    }
    // STATUS is SHOULD-level per ADR-0011 §3 but always informative.
    // Required for cancellation round-trip into IMAP (Phase 2).
    push_line(&mut out, &format!("STATUS:{}", c.status.as_str()));

    push_line(&mut out, "END:VEVENT");
    push_line(&mut out, "END:VCALENDAR");
    out
}

fn commitment_attendee_line(a: &CommitmentAttendee) -> String {
    let cn = a.display_name.as_deref().filter(|s| !s.is_empty());
    let partstat = a
        .partstat
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("NEEDS-ACTION");
    match cn {
        Some(name) => format!(
            "ATTENDEE;CN={};PARTSTAT={}:mailto:{}",
            escape_param(name),
            partstat,
            a.email,
        ),
        None => format!(
            "ATTENDEE;PARTSTAT={}:mailto:{}",
            partstat, a.email,
        ),
    }
}

/// Parse one of the four ICS time shapes into an RFC 3339 string with an
/// explicit offset, plus the original TZID (if any) for round-trip export.
///
/// Shapes:
///   `19970714T133000Z`              → UTC
///   `19970714T133000`               → floating, treated as system-local
///   `TZID=Europe/Berlin:19970714T133000` → local (TZID name kept for export)
///   `19970714`                      → date only, midnight system-local
fn ics_time_to_rfc3339(raw: &str) -> Result<(String, Option<String>), String> {
    let trimmed = raw.trim();
    let (tzid, body) = if let Some(rest) = trimmed.strip_prefix("TZID=") {
        match rest.split_once(':') {
            Some((tz, t)) => (Some(tz.to_string()), t),
            None => return Err(format!("malformed TZID expression: {trimmed}")),
        }
    } else {
        (None, trimmed)
    };

    if body.len() == 8 {
        // Date-only.
        let date = NaiveDate::parse_from_str(body, "%Y%m%d")
            .map_err(|e| format!("invalid date {body}: {e}"))?;
        let naive = date.and_hms_opt(0, 0, 0).unwrap();
        let offset = Local.offset_from_local_datetime(&naive).single().ok_or(
            "ambiguous local time at midnight near a DST transition",
        )?;
        let dt = offset.from_local_datetime(&naive).single().ok_or(
            "ambiguous local time at midnight near a DST transition",
        )?;
        return Ok((dt.to_rfc3339(), tzid));
    }

    let (stripped, is_utc) = match body.strip_suffix('Z') {
        Some(s) => (s, true),
        None => (body, false),
    };
    let naive = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S")
        .map_err(|e| format!("invalid datetime {body}: {e}"))?;

    let dt_with_offset = if is_utc {
        Utc.from_utc_datetime(&naive)
            .with_timezone(&FixedOffset::east_opt(0).unwrap())
    } else {
        // TZID present or floating: use local offset. See the Phase 1
        // limitation noted at the top of this file.
        let offset = Local
            .offset_from_local_datetime(&naive)
            .single()
            .ok_or("ambiguous local time near a DST transition")?;
        offset
            .from_local_datetime(&naive)
            .single()
            .ok_or("ambiguous local time near a DST transition")?
    };
    Ok((dt_with_offset.to_rfc3339(), tzid))
}

/// Inverse of `ics_time_to_rfc3339`: turn our stored RFC 3339 timestamp
/// back into an ICS DTSTART/DTEND value. We always emit UTC form
/// (`YYYYMMDDTHHMMSSZ`) — lossy versus original_tzid, but unambiguous and
/// accepted by every calendar consumer. Round-tripping the original TZID
/// is a Phase-2+ concern.
fn rfc3339_to_ics(rfc: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc)
        .map(|dt| dt.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_else(|_| rfc.to_string())
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
