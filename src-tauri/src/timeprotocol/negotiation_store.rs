// SQLite persistence for the Phase 3 negotiation domain. Mirrors the
// structure of `store.rs` (which owns commitments) but lives in its own
// file so the two bounded contexts stay separately readable as the
// negotiation surface grows.
//
// Three tables: `negotiations` (one row per thread), `negotiation_slots`
// (one row per proposed slot), `negotiation_messages` (append-only
// envelope log). Reads hydrate children via a per-row JOIN-equivalent
// (two extra queries) — Phase-3-v1 thread counts are small enough that
// the simpler shape beats a single complex GROUP_CONCAT query.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::domain::{
    MessageDirection, Negotiation, NegotiationAction, NegotiationConstraints,
    NegotiationMessage, NegotiationSlot, NegotiationState, SlotStatus, ThreadRole,
};
use crate::infrastructure::db::DbError;

// ─── Reads ────────────────────────────────────────────────────────────────

pub fn get_by_negotiation_id(
    conn: &Connection,
    negotiation_id: &str,
) -> Result<Option<Negotiation>, DbError> {
    let row: Option<Negotiation> = conn
        .query_row(
            NEGOTIATION_SELECT,
            params![negotiation_id],
            map_negotiation_row,
        )
        .optional()?;
    let mut neg = match row {
        Some(n) => n,
        None => return Ok(None),
    };
    neg.slots = list_slots(conn, &neg.negotiation_id)?;
    neg.messages = list_messages(conn, &neg.negotiation_id)?;
    Ok(Some(neg))
}

#[allow(dead_code)]
pub fn get_by_id(conn: &Connection, id: &str) -> Result<Option<Negotiation>, DbError> {
    let row: Option<Negotiation> = conn
        .query_row(
            "SELECT id, negotiation_id, thread_role, state, duration_iso,
                    constraints_json, counterparty_email, counterparty_name,
                    confirmed_commitment_id, display_summary,
                    created_at, updated_at
             FROM negotiations WHERE id = ?1",
            params![id],
            map_negotiation_row,
        )
        .optional()?;
    let mut neg = match row {
        Some(n) => n,
        None => return Ok(None),
    };
    neg.slots = list_slots(conn, &neg.negotiation_id)?;
    neg.messages = list_messages(conn, &neg.negotiation_id)?;
    Ok(Some(neg))
}

/// Full negotiation list. Used by the (future) negotiation list view +
/// by the Reader's "show in-flight thread for this UID"-affordance.
pub fn list_all(conn: &Connection) -> Result<Vec<Negotiation>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, negotiation_id, thread_role, state, duration_iso,
                constraints_json, counterparty_email, counterparty_name,
                confirmed_commitment_id, display_summary,
                created_at, updated_at
         FROM negotiations
         ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map([], map_negotiation_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    for n in out.iter_mut() {
        n.slots = list_slots(conn, &n.negotiation_id)?;
        n.messages = list_messages(conn, &n.negotiation_id)?;
    }
    Ok(out)
}

/// Has this `message_id` already been processed? Used by the inbound
/// envelope path for cheap idempotency before doing any work — saves
/// the hashed-state-comparison + UNIQUE-constraint dance when 95% of
/// inbound mails are first-encounters.
pub fn message_id_exists(conn: &Connection, message_id: &str) -> Result<bool, DbError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(1) FROM negotiation_messages WHERE message_id = ?1",
        params![message_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

// ─── Writes (called from the writer actor in `infrastructure::db`) ────────

/// Atomically upsert the negotiation row, replace its slot set, and
/// optionally append a new envelope to the message log.
///
/// The `negotiation.messages` field is ignored on write — append-only
/// history stays in `negotiation_messages`, and you supply the *new*
/// message via `new_message`. The slot set carried in `negotiation.slots`
/// is the authoritative current state and replaces what's stored.
///
/// `new_message` may be `None` (rare; you typically have at least one
/// inbound or outbound envelope to record). Idempotency: a UNIQUE
/// violation on `message_id` is caught and treated as a no-op so
/// duplicate-delivery doesn't poison the negotiation thread.
pub fn apply_negotiation_update(
    conn: &mut Connection,
    negotiation: &Negotiation,
    new_message: Option<&NegotiationMessage>,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;

    let constraints_json = match &negotiation.constraints {
        Some(c) => Some(serde_json::to_string(c).map_err(|e| {
            DbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?),
        None => None,
    };

    tx.execute(
        "INSERT INTO negotiations
            (id, negotiation_id, thread_role, state, duration_iso,
             constraints_json, counterparty_email, counterparty_name,
             confirmed_commitment_id, display_summary,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(negotiation_id) DO UPDATE SET
            thread_role             = excluded.thread_role,
            state                   = excluded.state,
            duration_iso            = excluded.duration_iso,
            constraints_json        = excluded.constraints_json,
            counterparty_email      = excluded.counterparty_email,
            counterparty_name       = excluded.counterparty_name,
            confirmed_commitment_id = excluded.confirmed_commitment_id,
            display_summary         = excluded.display_summary,
            updated_at              = excluded.updated_at",
        params![
            negotiation.id,
            negotiation.negotiation_id,
            negotiation.thread_role.as_str(),
            negotiation.state.as_str(),
            negotiation.duration_iso,
            constraints_json,
            negotiation.counterparty_email,
            negotiation.counterparty_name,
            negotiation.confirmed_commitment_id,
            negotiation.display_summary,
            negotiation.created_at.to_rfc3339(),
            negotiation.updated_at.to_rfc3339(),
        ],
    )?;

    // Full slot replace inside the transaction. Cheaper and clearer
    // than diff-and-patch — slot count per thread is tiny (<10 in
    // practice).
    tx.execute(
        "DELETE FROM negotiation_slots WHERE negotiation_id = ?1",
        params![negotiation.negotiation_id],
    )?;
    for slot in &negotiation.slots {
        tx.execute(
            "INSERT INTO negotiation_slots
                (negotiation_id, slot_id, proposer_node_id,
                 start_at, end_at, status, proposed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                negotiation.negotiation_id,
                slot.slot_id,
                slot.proposer_node_id,
                slot.start_at,
                slot.end_at,
                slot.status.as_str(),
                slot.proposed_at.to_rfc3339(),
            ],
        )?;
    }

    if let Some(msg) = new_message {
        let envelope_json = serde_json::to_string(&msg.envelope).map_err(|e| {
            DbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?;
        match tx.execute(
            "INSERT INTO negotiation_messages
                (negotiation_id, message_id, direction, action,
                 envelope_json, source_message_id, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                negotiation.negotiation_id,
                msg.message_id,
                msg.direction.as_str(),
                msg.action.as_str(),
                envelope_json,
                msg.source_message_id,
                msg.received_at.to_rfc3339(),
            ],
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                // Duplicate `message_id`. Spec §7.1: "Duplicate
                // envelopes must not create additional state
                // transitions." We treat this as idempotent success
                // — the negotiation row + slots replacement still
                // applied (and is itself idempotent on the same
                // input).
                tracing::debug!(
                    message_id = %msg.message_id,
                    "negotiation_messages: duplicate message_id, skipped"
                );
            }
            Err(e) => return Err(e.into()),
        }
    }

    tx.commit()?;
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────

const NEGOTIATION_SELECT: &str =
    "SELECT id, negotiation_id, thread_role, state, duration_iso,
            constraints_json, counterparty_email, counterparty_name,
            confirmed_commitment_id, display_summary,
            created_at, updated_at
     FROM negotiations WHERE negotiation_id = ?1";

fn map_negotiation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Negotiation> {
    let thread_role_str: String = row.get(2)?;
    let state_str: String = row.get(3)?;
    let constraints_json: Option<String> = row.get(5)?;
    let constraints = match constraints_json {
        Some(s) => serde_json::from_str::<NegotiationConstraints>(&s).ok(),
        None => None,
    };
    let created_at: String = row.get(10)?;
    let updated_at: String = row.get(11)?;

    Ok(Negotiation {
        id: row.get(0)?,
        negotiation_id: row.get(1)?,
        thread_role: ThreadRole::from_str(&thread_role_str)
            .unwrap_or(ThreadRole::Initiator),
        state: NegotiationState::from_str(&state_str)
            .unwrap_or(NegotiationState::Released),
        duration_iso: row.get(4)?,
        constraints,
        counterparty_email: row.get(6)?,
        counterparty_name: row.get(7)?,
        confirmed_commitment_id: row.get(8)?,
        display_summary: row.get(9)?,
        slots: Vec::new(),
        messages: Vec::new(),
        created_at: parse_utc(&created_at),
        updated_at: parse_utc(&updated_at),
    })
}

fn list_slots(
    conn: &Connection,
    negotiation_id: &str,
) -> Result<Vec<NegotiationSlot>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT slot_id, proposer_node_id, start_at, end_at, status, proposed_at
         FROM negotiation_slots WHERE negotiation_id = ?1
         ORDER BY proposed_at ASC, slot_id ASC",
    )?;
    let rows = stmt.query_map(params![negotiation_id], |row| {
        let status_str: String = row.get(4)?;
        let proposed_at: String = row.get(5)?;
        Ok(NegotiationSlot {
            slot_id: row.get(0)?,
            proposer_node_id: row.get(1)?,
            start_at: row.get(2)?,
            end_at: row.get(3)?,
            status: SlotStatus::from_str(&status_str).unwrap_or(SlotStatus::Active),
            proposed_at: parse_utc(&proposed_at),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn list_messages(
    conn: &Connection,
    negotiation_id: &str,
) -> Result<Vec<NegotiationMessage>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT message_id, direction, action, envelope_json,
                source_message_id, received_at
         FROM negotiation_messages WHERE negotiation_id = ?1
         ORDER BY received_at ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![negotiation_id], |row| {
        let direction_str: String = row.get(1)?;
        let action_str: String = row.get(2)?;
        let envelope_json: String = row.get(3)?;
        let received_at: String = row.get(5)?;
        let envelope: serde_json::Value =
            serde_json::from_str(&envelope_json).unwrap_or(serde_json::Value::Null);
        Ok(NegotiationMessage {
            message_id: row.get(0)?,
            direction: MessageDirection::from_str(&direction_str)
                .unwrap_or(MessageDirection::Inbound),
            action: NegotiationAction::from_str(&action_str)
                .unwrap_or(NegotiationAction::Release),
            envelope,
            source_message_id: row.get(4)?,
            received_at: parse_utc(&received_at),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn parse_utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
