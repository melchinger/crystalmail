// SQLite persistence for stored commitments. All calendar SQL lives here
// so the boundary stays inside `timeprotocol/` — db_ops.rs covers the
// global Mail / contacts / workflow surface, this file covers ours.
//
// Reads are direct connection-pool access (matches the read-side pattern
// in `infrastructure::queries`). Writes are routed through the central
// writer actor in `infrastructure::db` to avoid concurrent-writer
// contention against the actor's own transactions; see `WriteCmd::Calendar*`
// variants.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::domain::{
    Commitment, CommitmentAttendee, CommitmentSource, CommitmentStatus, IcsParticipant,
};
use crate::infrastructure::db::DbError;

// ─── Reads ────────────────────────────────────────────────────────────────

/// List commitments overlapping the half-open interval `[from, to)` ordered
/// by start. Attendees are not loaded here — the list UI doesn't need them.
/// Use `get_with_attendees` for the detail/edit/export paths.
pub fn list_in_range(
    conn: &Connection,
    from: &str,
    to: &str,
) -> Result<Vec<Commitment>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, uid, sequence, summary, description, location,
                start_at, end_at, original_tzid,
                organizer_email, organizer_name,
                source, source_message_id, created_at, updated_at, status,
                last_published_sequence
         FROM commitments
         WHERE start_at < ?2 AND end_at > ?1
           AND status != 'CANCELLED'
         ORDER BY start_at ASC",
    )?;
    let rows = stmt.query_map(params![from, to], row_to_commitment_no_attendees)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch a single commitment with its attendees attached. Returns `None`
/// when the id is unknown.
pub fn get_with_attendees(
    conn: &Connection,
    id: &str,
) -> Result<Option<Commitment>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, uid, sequence, summary, description, location,
                start_at, end_at, original_tzid,
                organizer_email, organizer_name,
                source, source_message_id, created_at, updated_at, status,
                last_published_sequence
         FROM commitments WHERE id = ?1",
    )?;
    let mut commitment: Option<Commitment> = stmt
        .query_row(params![id], row_to_commitment_no_attendees)
        .optional()?;
    if let Some(ref mut c) = commitment {
        c.attendees = fetch_attendees(conn, &c.id)?;
    }
    Ok(commitment)
}

/// List **all** commitments (including CANCELLED) for the IMAP-sync
/// path. The list view filters CANCELLED rows out for the UI; the sync
/// path needs them so cancellation tombstones get published per
/// ADR-0011 §6.1.
pub fn list_all_with_attendees(
    conn: &Connection,
) -> Result<Vec<Commitment>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, uid, sequence, summary, description, location,
                start_at, end_at, original_tzid,
                organizer_email, organizer_name,
                source, source_message_id, created_at, updated_at, status,
                last_published_sequence
         FROM commitments
         ORDER BY uid ASC",
    )?;
    let rows = stmt.query_map([], row_to_commitment_no_attendees)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    // Hydrate attendees per row. Could be a single JOIN+GROUP query, but
    // Phase-2 v1 calendars are small (<1000 active rows) and the per-row
    // query keeps the row-mapping shared with the other read paths.
    for c in out.iter_mut() {
        c.attendees = fetch_attendees(conn, &c.id)?;
    }
    Ok(out)
}

/// Lookup by RFC 5545 UID. Currently unused at the call-site level (the
/// import upsert resolves UID inline within its transaction), but kept on
/// the read API surface for "is this invitation already in my calendar?"
/// checks the UI may want later.
#[allow(dead_code)]
pub fn get_by_uid(
    conn: &Connection,
    uid: &str,
) -> Result<Option<Commitment>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, uid, sequence, summary, description, location,
                start_at, end_at, original_tzid,
                organizer_email, organizer_name,
                source, source_message_id, created_at, updated_at, status,
                last_published_sequence
         FROM commitments WHERE uid = ?1",
    )?;
    let mut commitment: Option<Commitment> = stmt
        .query_row(params![uid], row_to_commitment_no_attendees)
        .optional()?;
    if let Some(ref mut c) = commitment {
        c.attendees = fetch_attendees(conn, &c.id)?;
    }
    Ok(commitment)
}

fn fetch_attendees(
    conn: &Connection,
    commitment_id: &str,
) -> Result<Vec<CommitmentAttendee>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT email, display_name, partstat
         FROM commitment_attendees WHERE commitment_id = ?1
         ORDER BY email ASC",
    )?;
    let rows = stmt.query_map(params![commitment_id], |row| {
        Ok(CommitmentAttendee {
            email: row.get(0)?,
            display_name: row.get(1)?,
            partstat: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn row_to_commitment_no_attendees(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Commitment> {
    let organizer_email: Option<String> = row.get(9)?;
    let organizer_name: Option<String> = row.get(10)?;
    let organizer = organizer_email.map(|email| IcsParticipant {
        email,
        display_name: organizer_name,
    });
    let source_str: String = row.get(11)?;
    let source = CommitmentSource::from_str(&source_str).unwrap_or(
        // Should never happen — CHECK constraint filters at write time.
        // But: don't panic on a malformed legacy row.
        CommitmentSource::Manual,
    );
    let created_at: String = row.get(13)?;
    let updated_at: String = row.get(14)?;
    let status_str: String = row.get(15)?;
    let status = CommitmentStatus::from_str(&status_str)
        .unwrap_or(CommitmentStatus::Confirmed);
    let last_published_sequence: Option<i64> = row.get(16)?;
    Ok(Commitment {
        id: row.get(0)?,
        uid: row.get(1)?,
        sequence: row.get::<_, i64>(2)? as u32,
        summary: row.get(3)?,
        description: row.get(4)?,
        location: row.get(5)?,
        start_at: row.get(6)?,
        end_at: row.get(7)?,
        original_tzid: row.get(8)?,
        organizer,
        attendees: Vec::new(),
        source,
        status,
        last_published_sequence: last_published_sequence.map(|n| n as u32),
        source_message_id: row.get(12)?,
        created_at: parse_utc(&created_at),
        updated_at: parse_utc(&updated_at),
    })
}

fn parse_utc(s: &str) -> DateTime<Utc> {
    // Stored as ISO 8601; if a row predates a format-decision (it shouldn't,
    // but defensively), fall back to "now" rather than failing the whole list.
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// ─── Writes (called from the writer actor in `infrastructure::db`) ────────

/// Upsert a commitment plus its attendees in a single transaction. Inserts
/// when no row matches the UID; otherwise updates the existing row in place
/// (preserving the local `id` so foreign references — e.g. UI selection
/// state, source_message_id pointers — survive a re-import).
pub fn upsert_commitment(
    conn: &mut Connection,
    commitment: &Commitment,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    let organizer_email = commitment.organizer.as_ref().map(|p| p.email.clone());
    let organizer_name = commitment
        .organizer
        .as_ref()
        .and_then(|p| p.display_name.clone());

    // Find existing row by UID — if present we keep its id so foreign
    // references stay valid. The frontend may also pass an id directly
    // (edit case); the UID-lookup wins because re-imports and edits
    // shouldn't collide.
    let existing_id: Option<String> = tx
        .query_row(
            "SELECT id FROM commitments WHERE uid = ?1",
            params![commitment.uid],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let target_id = existing_id.unwrap_or_else(|| commitment.id.clone());

    tx.execute(
        "INSERT INTO commitments
            (id, uid, sequence, summary, description, location,
             start_at, end_at, original_tzid,
             organizer_email, organizer_name,
             source, source_message_id, created_at, updated_at, status,
             last_published_sequence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(id) DO UPDATE SET
            uid                     = excluded.uid,
            sequence                = excluded.sequence,
            summary                 = excluded.summary,
            description             = excluded.description,
            location                = excluded.location,
            start_at                = excluded.start_at,
            end_at                  = excluded.end_at,
            original_tzid           = excluded.original_tzid,
            organizer_email         = excluded.organizer_email,
            organizer_name          = excluded.organizer_name,
            source                  = excluded.source,
            source_message_id       = excluded.source_message_id,
            updated_at              = excluded.updated_at,
            status                  = excluded.status,
            last_published_sequence = excluded.last_published_sequence",
        params![
            target_id,
            commitment.uid,
            commitment.sequence as i64,
            commitment.summary,
            commitment.description,
            commitment.location,
            commitment.start_at,
            commitment.end_at,
            commitment.original_tzid,
            organizer_email,
            organizer_name,
            commitment.source.as_str(),
            commitment.source_message_id,
            commitment.created_at.to_rfc3339(),
            commitment.updated_at.to_rfc3339(),
            commitment.status.as_str(),
            commitment.last_published_sequence.map(|n| n as i64),
        ],
    )?;

    // Attendees: full replace inside the same transaction. Cheaper and
    // simpler than diffing — there are typically <10 per event.
    tx.execute(
        "DELETE FROM commitment_attendees WHERE commitment_id = ?1",
        params![target_id],
    )?;
    for a in &commitment.attendees {
        tx.execute(
            "INSERT INTO commitment_attendees (commitment_id, email, display_name, partstat)
             VALUES (?1, ?2, ?3, ?4)",
            params![target_id, a.email, a.display_name, a.partstat],
        )?;
    }

    tx.commit()?;
    Ok(())
}

