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
                last_published_sequence, series_uid
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

/// List commitments touching at least one of the given email addresses,
/// either as ORGANIZER or as an ATTENDEE row. Used by the ContactDetail
/// "Termine"-Section to show a contact's upcoming/recent meetings.
///
/// - Emails are matched case-insensitively. Caller may pass them in any
///   case; we lower-case both sides in the comparison.
/// - The `[from, to)` window is checked against `start_at` only (not the
///   start..end overlap rule `list_in_range` uses) — the calling UX
///   ("Termine in den letzten 30 Tagen" / "Anstehend") is about *when
///   the meeting begins*, not whether it spans the window.
/// - CANCELLED rows are excluded.
/// - Attendees are attached to each row so the UI can render
///   PARTSTAT-Badges without a per-row round-trip.
/// - Result limit guards against runaway queries on contacts with very
///   long histories — practical max 200, the caller picks tighter.
pub fn list_for_emails(
    conn: &Connection,
    emails: &[String],
    from: &str,
    to: &str,
    limit: i64,
) -> Result<Vec<Commitment>, DbError> {
    if emails.is_empty() {
        return Ok(Vec::new());
    }
    // Build a parameter list of LOWER-cased emails so we can re-use the
    // same `?N` slot for the organizer-equality check and for the
    // inner attendee IN-clause. Saves us from binding the same vector
    // twice.
    let lowered: Vec<String> = emails
        .iter()
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    if lowered.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (0..lowered.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    // Positional `?`: SQLite consumes one bind per `?` in left-to-right
    // appearance. The email list appears in TWO IN-clauses (organizer
    // + attendee EXISTS), so we bind it twice. Slot order:
    //   from, to, emails×N, emails×N, limit.
    let sql = format!(
        "SELECT id, uid, sequence, summary, description, location,
                start_at, end_at, original_tzid,
                organizer_email, organizer_name,
                source, source_message_id, created_at, updated_at, status,
                last_published_sequence, series_uid
         FROM commitments c
         WHERE c.status != 'CANCELLED'
           AND c.start_at >= ? AND c.start_at < ?
           AND (
             LOWER(IFNULL(c.organizer_email, '')) IN ({placeholders})
             OR EXISTS (
               SELECT 1 FROM commitment_attendees a
               WHERE a.commitment_id = c.id
                 AND LOWER(a.email) IN ({placeholders})
             )
           )
         ORDER BY c.start_at ASC
         LIMIT ?",
        placeholders = placeholders,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<&dyn rusqlite::ToSql> =
        Vec::with_capacity(2 * lowered.len() + 3);
    binds.push(&from);
    binds.push(&to);
    for e in &lowered {
        binds.push(e);
    }
    for e in &lowered {
        binds.push(e);
    }
    binds.push(&limit);
    let rows = stmt.query_map(
        rusqlite::params_from_iter(binds.iter().copied()),
        row_to_commitment_no_attendees,
    )?;
    let mut out = Vec::new();
    for r in rows {
        let mut c = r?;
        c.attendees = fetch_attendees(conn, &c.id)?;
        out.push(c);
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
                last_published_sequence, series_uid
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
                last_published_sequence, series_uid
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

/// Lookup by RFC 5545 UID. Used by the inbound-REPLY path to find the
/// local commitment a responder's PARTSTAT update applies to. The import
/// upsert resolves UID inline within its own transaction (write-side),
/// so this lives only on the read API.
pub fn get_by_uid(
    conn: &Connection,
    uid: &str,
) -> Result<Option<Commitment>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, uid, sequence, summary, description, location,
                start_at, end_at, original_tzid,
                organizer_email, organizer_name,
                source, source_message_id, created_at, updated_at, status,
                last_published_sequence, series_uid
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
        // Stored row's organizer never carries PARTSTAT (organizer ≠
        // attendee). The field exists purely for inbound-REPLY parsing.
        partstat: None,
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
    let series_uid: Option<String> = row.get(17)?;
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
        series_uid,
        // The DB has no subscription_id column — these rows always
        // come from SQLite, never the subscription overlay.
        subscription_id: None,
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
             last_published_sequence, series_uid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
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
            last_published_sequence = excluded.last_published_sequence,
            series_uid              = excluded.series_uid",
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
            commitment.series_uid,
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

/// Hard-delete every row whose `series_uid` matches. Used when the user
/// chooses "ganze Serie absagen" on an RRULE-expanded occurrence. We hard
/// delete instead of marking CANCELLED because series rows are excluded
/// from IMAP publish anyway (see `sync.rs`) — there's no cancellation
/// envelope to emit, so a tombstone would just clutter the table.
/// Attendees cascade via the FK on `commitment_attendees.commitment_id`.
/// Returns the row count actually removed.
pub fn delete_series_by_uid(
    conn: &mut Connection,
    series_uid: &str,
) -> Result<usize, DbError> {
    let tx = conn.transaction()?;
    // Drop attendees first — the schema doesn't declare ON DELETE CASCADE
    // (Phase-1 store.rs doesn't), so we sweep them by hand inside the
    // same transaction.
    tx.execute(
        "DELETE FROM commitment_attendees
         WHERE commitment_id IN (
            SELECT id FROM commitments WHERE series_uid = ?1
         )",
        params![series_uid],
    )?;
    let n = tx.execute(
        "DELETE FROM commitments WHERE series_uid = ?1",
        params![series_uid],
    )?;
    tx.commit()?;
    Ok(n)
}

