// Concrete write operations for the SQLite store.
//
// Lives in its own file so `db.rs` can stay focused on the actor /
// dispatch / open / migrate concerns. Each function here owns one
// SQL statement (or one short transaction) and is called from
// `db::dispatch` via the `WriteCmd::*` arms.
//
// Visibility: `pub(super)` keeps these helpers internal to the
// `infrastructure` module — no caller outside the writer actor or
// the import-bundle path should reach in directly.

use std::collections::HashSet;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::domain::account::{Account, AccountAlias, AccountId};
use crate::domain::contact::is_role_address;
use crate::domain::folder::FolderId;
use crate::domain::message::{Address, Envelope, Flags, MessageId};

use super::db::DbError;

pub(super) fn insert_account(conn: &Connection, a: &Account) -> Result<(), DbError> {
    let (kind, entry) = match &a.credential {
        crate::domain::auth::AuthCredential::Password { keyring_entry } => {
            ("password", keyring_entry.clone())
        }
        crate::domain::auth::AuthCredential::OAuth2 { keyring_entry, .. } => {
            ("oauth2", keyring_entry.clone())
        }
    };
    conn.execute(
        "INSERT INTO accounts (id, display_name, address, from_name, color, signature, signature_html,
                               imap_host, imap_port, imap_tls,
                               smtp_host, smtp_port, smtp_tls,
                               credential_kind, credential_entry,
                               archive_folder, sent_folder, drafts_folder, trash_folder, spam_folder,
                               archive_on_reply, prefetch_days, sync_mode, server_stores_sent,
                               created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![
            a.id.0.to_string(),
            a.display_name,
            a.address,
            a.from_name,
            a.color,
            a.signature,
            a.signature_html,
            a.imap.host,
            a.imap.port,
            a.imap.tls as i64,
            a.smtp.host,
            a.smtp.port,
            a.smtp.tls as i64,
            kind,
            entry,
            a.archive_folder,
            a.sent_folder,
            a.drafts_folder,
            a.trash_folder,
            a.spam_folder,
            a.archive_on_reply as i64,
            a.prefetch_days,
            a.sync_mode.as_db_str(),
            a.server_stores_sent as i64,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub(super) fn update_account(conn: &Connection, a: &Account) -> Result<(), DbError> {
    let (kind, entry) = match &a.credential {
        crate::domain::auth::AuthCredential::Password { keyring_entry } => {
            ("password", keyring_entry.clone())
        }
        crate::domain::auth::AuthCredential::OAuth2 { keyring_entry, .. } => {
            ("oauth2", keyring_entry.clone())
        }
    };
    let rows = conn.execute(
        "UPDATE accounts SET
            display_name = ?2,
            address = ?3,
            from_name = ?4,
            color = ?5,
            signature = ?6,
            signature_html = ?7,
            imap_host = ?8,
            imap_port = ?9,
            imap_tls = ?10,
            smtp_host = ?11,
            smtp_port = ?12,
            smtp_tls = ?13,
            credential_kind = ?14,
            credential_entry = ?15,
            archive_folder = ?16,
            sent_folder = ?17,
            drafts_folder = ?18,
            trash_folder = ?19,
            spam_folder = ?20,
            archive_on_reply = ?21,
            prefetch_days = ?22,
            sync_mode = ?23,
            server_stores_sent = ?24
          WHERE id = ?1",
        params![
            a.id.0.to_string(),
            a.display_name,
            a.address,
            a.from_name,
            a.color,
            a.signature,
            a.signature_html,
            a.imap.host,
            a.imap.port,
            a.imap.tls as i64,
            a.smtp.host,
            a.smtp.port,
            a.smtp.tls as i64,
            kind,
            entry,
            a.archive_folder,
            a.sent_folder,
            a.drafts_folder,
            a.trash_folder,
            a.spam_folder,
            a.archive_on_reply as i64,
            a.prefetch_days,
            a.sync_mode.as_db_str(),
            a.server_stores_sent as i64,
        ],
    )?;
    if rows == 0 {
        return Err(DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
    }
    Ok(())
}

pub(super) fn delete_account(
    conn: &Connection,
    id: &crate::domain::account::AccountId,
) -> Result<(), DbError> {
    conn.execute("DELETE FROM accounts WHERE id = ?1", params![id.0.to_string()])?;
    Ok(())
}

pub(super) fn ensure_folder(
    conn: &Connection,
    account_id: &AccountId,
    name: &str,
) -> Result<FolderId, DbError> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM folders WHERE account_id = ?1 AND name = ?2",
            params![account_id.0.to_string(), name],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id_str) = existing {
        let uuid = uuid::Uuid::parse_str(&id_str)
            .map_err(|_| rusqlite::Error::InvalidParameterName("folder id".into()))?;
        return Ok(FolderId(uuid));
    }

    let new_id = FolderId(uuid::Uuid::new_v4());
    conn.execute(
        "INSERT INTO folders (id, account_id, name, uid_validity, uid_next, last_sync_ts)
         VALUES (?1, ?2, ?3, 0, 0, NULL)",
        params![new_id.0.to_string(), account_id.0.to_string(), name],
    )?;
    Ok(new_id)
}

pub(super) fn delete_folder_tree(
    conn: &mut Connection,
    folder_id: &FolderId,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    // FTS5 rows first — contentless-delete mode lets us remove by
    // rowid, and the FK CASCADE on `envelopes` wouldn't touch FTS
    // for us.
    tx.execute(
        "DELETE FROM fts_envelopes
          WHERE rowid IN (SELECT rowid FROM envelopes WHERE folder_id = ?1)",
        params![folder_id.0.to_string()],
    )?;
    // Folder row — `envelopes` rows (and via them `bodies`) cascade
    // via the FKs defined in migration 0001.
    tx.execute(
        "DELETE FROM folders WHERE id = ?1",
        params![folder_id.0.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

pub(super) fn update_folder_sync_state(
    conn: &Connection,
    folder_id: &FolderId,
    uid_validity: u32,
    uid_next: u32,
    last_sync_ts: &chrono::DateTime<chrono::Utc>,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE folders
            SET uid_validity = ?2, uid_next = ?3, last_sync_ts = ?4
          WHERE id = ?1",
        params![
            folder_id.0.to_string(),
            uid_validity,
            uid_next,
            last_sync_ts.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub(super) fn set_folder_sync_enabled(
    conn: &Connection,
    folder_id: &FolderId,
    enabled: bool,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE folders SET sync_enabled = ?2 WHERE id = ?1",
        params![folder_id.0.to_string(), if enabled { 1 } else { 0 }],
    )?;
    Ok(())
}

pub(super) fn upsert_envelope(
    conn: &mut Connection,
    e: &Envelope,
    body_text: Option<&str>,
) -> Result<bool, DbError> {
    let tx = conn.transaction()?;

    let from_json = serde_json::to_string(&e.from)?;
    let to_json = serde_json::to_string(&e.to)?;
    let cc_json = serde_json::to_string(&e.cc)?;
    let refs_json = serde_json::to_string(&e.references)?;

    let from_text = address_list_to_text(&e.from);
    let to_text = address_list_to_text(&e.to);

    // Detect whether this is a brand-new envelope or a re-sync of a UID
    // we already have. Drives the "new mail arrived" chime — `fetched`
    // alone double-counts known mails that fall inside the SINCE-30d
    // window. One indexed point lookup on the (folder_id, imap_uid)
    // UNIQUE index is sub-millisecond.
    let was_new = tx
        .query_row::<i64, _, _>(
            "SELECT 1 FROM envelopes WHERE folder_id = ?1 AND imap_uid = ?2",
            params![e.folder_id.0.to_string(), e.imap_uid],
            |r| r.get(0),
        )
        .optional()?
        .is_none();

    // `body_cached` uses MAX(existing, new) on conflict so re-syncs (which
    // only carry envelope data, never bodies) don't wipe a previously
    // downloaded body's cache flag. Same for `has_attachments`: the sync
    // path supplies a heuristic (top-level Content-Type), the body-fetch
    // path supplies the authoritative answer — once flipped to 1, a
    // subsequent header-only sync must not zero it out.
    tx.execute(
        "INSERT INTO envelopes (
            id, account_id, folder_id, imap_uid, message_id_header, subject, date_utc, size_bytes,
            seen, answered, flagged, draft, deleted, forwarded, junk,
            from_json, to_json, cc_json, references_json, body_cached, has_attachments, thread_root
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22
         )
         ON CONFLICT(folder_id, imap_uid) DO UPDATE SET
            subject           = excluded.subject,
            message_id_header = excluded.message_id_header,
            date_utc          = excluded.date_utc,
            size_bytes        = excluded.size_bytes,
            seen              = excluded.seen,
            answered          = excluded.answered,
            flagged           = excluded.flagged,
            draft             = excluded.draft,
            deleted           = excluded.deleted,
            forwarded         = excluded.forwarded,
            junk              = excluded.junk,
            from_json         = excluded.from_json,
            to_json           = excluded.to_json,
            cc_json           = excluded.cc_json,
            references_json   = excluded.references_json,
            body_cached       = MAX(envelopes.body_cached, excluded.body_cached),
            has_attachments   = MAX(envelopes.has_attachments, excluded.has_attachments)",
        params![
            e.id.0.to_string(),
            e.account_id.0.to_string(),
            e.folder_id.0.to_string(),
            e.imap_uid,
            e.message_id_header,
            e.subject,
            e.date.to_rfc3339(),
            e.size_bytes,
            e.flags.seen as i64,
            e.flags.answered as i64,
            e.flags.flagged as i64,
            e.flags.draft as i64,
            e.flags.deleted as i64,
            e.flags.forwarded as i64,
            e.flags.junk as i64,
            from_json,
            to_json,
            cc_json,
            refs_json,
            e.body_cached as i64,
            e.has_attachments as i64,
            None::<String>,
        ],
    )?;

    // Refresh the FTS5 row. We `DELETE` first because fts5 contentless tables
    // don't support UPSERT; the second insert carries the current text.
    tx.execute(
        "DELETE FROM fts_envelopes WHERE rowid = (SELECT rowid FROM envelopes WHERE id = ?1)",
        params![e.id.0.to_string()],
    )?;
    tx.execute(
        "INSERT INTO fts_envelopes (rowid, subject, from_text, to_text, body_text)
         VALUES ((SELECT rowid FROM envelopes WHERE id = ?1), ?2, ?3, ?4, ?5)",
        params![
            e.id.0.to_string(),
            e.subject,
            from_text,
            to_text,
            body_text.unwrap_or(""),
        ],
    )?;

    // Adress-History side-effect: jede From/To/Cc-Adresse upserten,
    // own-account-Adressen rausfiltern, send_count vs recv_count
    // anhand der Folder-Identität (sent_folder match) entscheiden.
    // Fehler hier dürfen den Envelope-Insert nicht killen — wir
    // loggen sie und fahren weiter; History ist Convenience, nicht
    // korrektheits-kritisch.
    if let Err(err) = record_address_history(&tx, e) {
        tracing::warn!(
            envelope_id = %e.id.0,
            error = %err,
            "address_history side-effect failed"
        );
    }

    tx.commit()?;
    Ok(was_new)
}

/// Side-Effect aus `upsert_envelope`. Schreibt jede Mail-Address aus
/// From/To/Cc in `address_history`, mit Recency-/Frequency-Update.
///
/// Direction-Detection: wenn der Envelope im konfigurierten
/// sent_folder des Accounts liegt, ist die Mail ausgehend (User =
/// Absender) → To/Cc bekommen `send_count++`. Andernfalls eingehend
/// → From + To + Cc bekommen `recv_count++`. Eigene Adressen +
/// Aliase werden konsequent rausgefiltert (kein Self-Suggestion).
///
/// Läuft in derselben Transaktion wie der Envelope-Insert; Fehler
/// rollen alles zurück. Aber der Caller (`upsert_envelope`) loggt
/// nur und committed trotzdem den Envelope-Teil — siehe oben.
fn record_address_history(tx: &Transaction<'_>, e: &Envelope) -> rusqlite::Result<()> {
    // Folder-Name für direction-Check.
    let folder_name: String = tx.query_row(
        "SELECT name FROM folders WHERE id = ?1",
        params![e.folder_id.0.to_string()],
        |r| r.get(0),
    )?;
    // Account-konfigurierter sent_folder.
    let sent_folder: String = tx.query_row(
        "SELECT sent_folder FROM accounts WHERE id = ?1",
        params![e.account_id.0.to_string()],
        |r| r.get(0),
    )?;
    let is_outgoing = !sent_folder.is_empty() && folder_name == sent_folder;

    // Own-emails-Set: account.address + alle Aliase. Lower-cased für
    // case-insensitive Match (RFC 5321 lokal-part ist eigentlich
    // case-sensitive, aber in der Praxis behandeln Provider die
    // immer case-insensitive).
    let mut own_emails: HashSet<String> = HashSet::new();
    let primary: String = tx.query_row(
        "SELECT lower(address) FROM accounts WHERE id = ?1",
        params![e.account_id.0.to_string()],
        |r| r.get(0),
    )?;
    own_emails.insert(primary);
    {
        let mut stmt = tx
            .prepare("SELECT lower(email) FROM account_aliases WHERE account_id = ?1")?;
        let rows = stmt.query_map(params![e.account_id.0.to_string()], |r| {
            r.get::<_, String>(0)
        })?;
        for row in rows {
            own_emails.insert(row?);
        }
    }

    let now = Utc::now().to_rfc3339();
    let date_str = e.date.to_rfc3339();
    // Last-seen sollte das spätere von "Mail-Datum" und "jetzt" sein —
    // damit Backfills mit alten Mails nicht die neueste Recency
    // überschreiben.
    let last_seen = if date_str > now { now.clone() } else { date_str };

    // Helper: einzelne Adresse in History upserten.
    let upsert = |tx: &Transaction<'_>,
                  addr: &Address,
                  send_delta: i64,
                  recv_delta: i64|
     -> rusqlite::Result<()> {
        let email = addr.email.trim().to_lowercase();
        if email.is_empty() || !email.contains('@') {
            return Ok(());
        }
        if own_emails.contains(&email) {
            return Ok(());
        }
        let role_flag = is_role_address(&email) as i64;
        let display_name = addr.name.as_deref().unwrap_or("").trim();
        // ON CONFLICT-Logik: display_name updated nur wenn excluded
        // einen nicht-leeren liefert (sonst behalten wir den
        // bestehenden — manchmal kommt eine Mail ohne Display-Name
        // rein, das soll keinen guten alten Namen wegrasieren).
        tx.execute(
            "INSERT INTO address_history (
                email, display_name, first_seen_at, last_seen_at,
                send_count, recv_count, is_role
             ) VALUES (?1, NULLIF(?2, ''), ?3, ?3, ?4, ?5, ?6)
             ON CONFLICT(email) DO UPDATE SET
                display_name  = COALESCE(NULLIF(excluded.display_name, ''), address_history.display_name),
                first_seen_at = MIN(address_history.first_seen_at, excluded.first_seen_at),
                last_seen_at  = MAX(address_history.last_seen_at, excluded.last_seen_at),
                send_count    = address_history.send_count + ?4,
                recv_count    = address_history.recv_count + ?5,
                is_role       = excluded.is_role",
            params![email, display_name, last_seen, send_delta, recv_delta, role_flag],
        )?;
        Ok(())
    };

    if is_outgoing {
        // User = Absender → empfängerseitige Adressen bekommen send_count++.
        // Das hier sind Adressen, die der User aktiv ausgewählt hat —
        // genau das, was ins Autocomplete soll.
        for a in &e.to {
            upsert(tx, a, 1, 0)?;
        }
        for a in &e.cc {
            upsert(tx, a, 1, 0)?;
        }
    } else {
        // User = Empfänger → NUR den Absender pflegen.
        //
        // Bewusst NICHT To/Cc bei eingehenden Mails: wenn jemand einen
        // großen Verteiler bedient (oder uns auf eine Mailing-Liste cc'd),
        // sind das 50 Adressen die nichts mit "wer mich relevant ist"
        // zu tun haben — sie würden das Autocomplete mit Leuten fluten,
        // an die der User nie selbst geschrieben hat.
        for a in &e.from {
            upsert(tx, a, 0, 1)?;
        }
    }

    Ok(())
}

pub(super) fn update_flags(
    conn: &Connection,
    id: &MessageId,
    f: &Flags,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE envelopes
            SET seen = ?2, answered = ?3, flagged = ?4, draft = ?5, deleted = ?6,
                forwarded = ?7, junk = ?8
          WHERE id = ?1",
        params![
            id.0.to_string(),
            f.seen as i64,
            f.answered as i64,
            f.flagged as i64,
            f.draft as i64,
            f.deleted as i64,
            f.forwarded as i64,
            f.junk as i64,
        ],
    )?;
    Ok(())
}

pub(super) fn store_body(
    conn: &mut Connection,
    id: &MessageId,
    raw: &[u8],
    plain: Option<&str>,
    html: Option<&str>,
    has_attachments: bool,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO bodies (envelope_id, raw_rfc822, plain_text, html_text, downloaded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(envelope_id) DO UPDATE SET
            raw_rfc822    = excluded.raw_rfc822,
            plain_text    = excluded.plain_text,
            html_text     = excluded.html_text,
            downloaded_at = excluded.downloaded_at",
        params![
            id.0.to_string(),
            raw,
            plain,
            html,
            Utc::now().to_rfc3339(),
        ],
    )?;
    // Flip body_cached and authoritatively (re)set has_attachments at the
    // same time. The body-fetch path is the source of truth — it has seen
    // the actual MIME tree, unlike the sync heuristic that only has the
    // top-level Content-Type to go by.
    tx.execute(
        "UPDATE envelopes
            SET body_cached     = 1,
                has_attachments = ?2
          WHERE id = ?1",
        params![id.0.to_string(), has_attachments as i64],
    )?;

    // Refresh the FTS row now that we have a real body_text. Contentless
    // FTS5 tables don't support UPDATE, so we DELETE + INSERT (possible
    // because the 0002 migration enabled `contentless_delete=1`). We pull
    // the subject/from/to from the envelope in the same transaction so
    // the refreshed row is self-consistent.
    if let Some(body_text) = plain {
        let row: Option<(i64, String, String, String)> = tx
            .query_row(
                "SELECT rowid, subject, from_json, to_json FROM envelopes WHERE id = ?1",
                params![id.0.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        if let Some((rowid, subject, from_json, to_json)) = row {
            let from_list: Vec<crate::domain::message::Address> =
                serde_json::from_str(&from_json).unwrap_or_default();
            let to_list: Vec<crate::domain::message::Address> =
                serde_json::from_str(&to_json).unwrap_or_default();
            let from_text = address_list_to_text(&from_list);
            let to_text = address_list_to_text(&to_list);

            tx.execute(
                "DELETE FROM fts_envelopes WHERE rowid = ?1",
                params![rowid],
            )?;
            tx.execute(
                "INSERT INTO fts_envelopes (rowid, subject, from_text, to_text, body_text)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![rowid, subject, from_text, to_text, body_text],
            )?;
        }
    }

    tx.commit()?;
    Ok(())
}

pub(super) fn delete_envelopes(
    conn: &mut Connection,
    folder_id: &FolderId,
    uids: &[u32],
) -> Result<(), DbError> {
    if uids.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    {
        // Drop FTS rows first (FK cascade doesn't cover contentless FTS tables).
        let mut del_fts = tx.prepare(
            "DELETE FROM fts_envelopes
              WHERE rowid IN (SELECT rowid FROM envelopes WHERE folder_id = ?1 AND imap_uid = ?2)",
        )?;
        let mut del_env = tx.prepare(
            "DELETE FROM envelopes WHERE folder_id = ?1 AND imap_uid = ?2",
        )?;
        for uid in uids {
            del_fts.execute(params![folder_id.0.to_string(), *uid])?;
            del_env.execute(params![folder_id.0.to_string(), *uid])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn replace_aliases(
    conn: &mut Connection,
    account_id: &AccountId,
    aliases: &[AccountAlias],
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM account_aliases WHERE account_id = ?1",
        params![account_id.0.to_string()],
    )?;
    {
        let mut ins = tx.prepare(
            "INSERT INTO account_aliases (id, account_id, email, from_name)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for a in aliases {
            ins.execute(params![
                a.id.to_string(),
                account_id.0.to_string(),
                a.email,
                a.from_name,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn insert_spam_rule(
    conn: &Connection,
    r: &crate::domain::spam_rule::SpamRule,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO spam_rules (
            id, account_id, pattern_type, pattern,
            enabled, confidence, reason, created_at, hit_count
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            r.id.0.to_string(),
            r.account_id.map(|a| a.0.to_string()),
            pattern_type_to_str(r.pattern_type),
            r.pattern,
            r.enabled as i64,
            r.confidence,
            r.reason,
            r.created_at.to_rfc3339(),
            r.hit_count as i64,
        ],
    )?;
    Ok(())
}

pub(super) fn set_spam_rule_enabled(
    conn: &Connection,
    id: &crate::domain::spam_rule::SpamRuleId,
    enabled: bool,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE spam_rules SET enabled = ?2 WHERE id = ?1",
        params![id.0.to_string(), enabled as i64],
    )?;
    Ok(())
}

pub(super) fn delete_spam_rule(
    conn: &Connection,
    id: &crate::domain::spam_rule::SpamRuleId,
) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM spam_rules WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

pub(super) fn increment_spam_rule_hits(
    conn: &Connection,
    id: &crate::domain::spam_rule::SpamRuleId,
    delta: i64,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE spam_rules SET hit_count = hit_count + ?2 WHERE id = ?1",
        params![id.0.to_string(), delta],
    )?;
    Ok(())
}

fn pattern_type_to_str(t: crate::domain::spam_rule::SpamPatternType) -> &'static str {
    use crate::domain::spam_rule::SpamPatternType as P;
    match t {
        P::FromEmail => "from_email",
        P::FromDomain => "from_domain",
        P::SubjectContains => "subject_contains",
        P::SubjectRegex => "subject_regex",
        P::BodyContains => "body_contains",
        P::HeaderContains => "header_contains",
    }
}

pub(super) fn insert_workflow(
    conn: &Connection,
    w: &crate::domain::workflow::Workflow,
) -> Result<(), DbError> {
    let steps_json = serde_json::to_string(&w.steps)?;
    conn.execute(
        "INSERT INTO workflows (
            id, name, hotkey, steps_json, enabled,
            archive_after_success,
            created_at, run_count, last_run_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            w.id.0.to_string(),
            w.name,
            w.hotkey,
            steps_json,
            w.enabled as i64,
            w.archive_after_success as i64,
            w.created_at.to_rfc3339(),
            w.run_count as i64,
            w.last_run_at.map(|ts| ts.to_rfc3339()),
        ],
    )?;
    Ok(())
}

pub(super) fn update_workflow(
    conn: &Connection,
    w: &crate::domain::workflow::Workflow,
) -> Result<(), DbError> {
    let steps_json = serde_json::to_string(&w.steps)?;
    let rows = conn.execute(
        "UPDATE workflows
            SET name = ?2, hotkey = ?3, steps_json = ?4, enabled = ?5,
                archive_after_success = ?6
          WHERE id = ?1",
        params![
            w.id.0.to_string(),
            w.name,
            w.hotkey,
            steps_json,
            w.enabled as i64,
            w.archive_after_success as i64,
        ],
    )?;
    if rows == 0 {
        return Err(DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
    }
    Ok(())
}

pub(super) fn delete_workflow(
    conn: &Connection,
    id: &crate::domain::workflow::WorkflowId,
) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM workflows WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

pub(super) fn record_workflow_run(
    conn: &Connection,
    id: &crate::domain::workflow::WorkflowId,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE workflows
            SET run_count = run_count + 1, last_run_at = ?2
          WHERE id = ?1",
        params![id.0.to_string(), Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

pub(super) fn insert_workflow_rule(
    conn: &Connection,
    r: &crate::domain::workflow::WorkflowRule,
) -> Result<(), DbError> {
    let predicates_json = serde_json::to_string(&r.predicates)?;
    conn.execute(
        "INSERT INTO workflow_rules (
            id, workflow_id, account_id, folder_name, predicates_json, mode,
            enabled, created_at, hit_count, last_hit_at,
            name, action_type, action_dest, delay_minutes, dry_run
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            r.id.0.to_string(),
            r.workflow_id.map(|w| w.0.to_string()),
            r.account_id.map(|a| a.0.to_string()),
            r.folder_name,
            predicates_json,
            rule_mode_to_str(r.mode),
            r.enabled as i64,
            r.created_at.to_rfc3339(),
            r.hit_count as i64,
            r.last_hit_at.map(|ts| ts.to_rfc3339()),
            r.name,
            r.action.as_str(),
            r.action_dest,
            r.delay_minutes as i64,
            r.dry_run as i64,
        ],
    )?;
    Ok(())
}

pub(super) fn update_workflow_rule(
    conn: &Connection,
    r: &crate::domain::workflow::WorkflowRule,
) -> Result<(), DbError> {
    let predicates_json = serde_json::to_string(&r.predicates)?;
    let rows = conn.execute(
        "UPDATE workflow_rules
            SET workflow_id = ?2, account_id = ?3, folder_name = ?4,
                predicates_json = ?5, mode = ?6, enabled = ?7,
                name = ?8, action_type = ?9, action_dest = ?10,
                delay_minutes = ?11, dry_run = ?12
          WHERE id = ?1",
        params![
            r.id.0.to_string(),
            r.workflow_id.map(|w| w.0.to_string()),
            r.account_id.map(|a| a.0.to_string()),
            r.folder_name,
            predicates_json,
            rule_mode_to_str(r.mode),
            r.enabled as i64,
            r.name,
            r.action.as_str(),
            r.action_dest,
            r.delay_minutes as i64,
            r.dry_run as i64,
        ],
    )?;
    if rows == 0 {
        return Err(DbError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
    }
    Ok(())
}

pub(super) fn set_workflow_rule_dry_run(
    conn: &Connection,
    id: &crate::domain::workflow::WorkflowRuleId,
    dry_run: bool,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE workflow_rules SET dry_run = ?2 WHERE id = ?1",
        params![id.0.to_string(), dry_run as i64],
    )?;
    Ok(())
}

pub(super) fn tag_envelope_scheduled(
    conn: &Connection,
    message_id: &crate::domain::message::MessageId,
    tag: &crate::domain::workflow::ScheduledActionTag,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE envelopes
            SET scheduled_at = ?2,
                scheduled_action_type = ?3,
                scheduled_action_dest = ?4,
                scheduled_rule_id = ?5,
                scheduled_rule_name = ?6,
                scheduled_workflow_id = ?7,
                scheduled_dry_run = ?8
          WHERE id = ?1",
        params![
            message_id.0.to_string(),
            tag.scheduled_at.to_rfc3339(),
            tag.action.as_str(),
            tag.action_dest,
            tag.rule_id.map(|r| r.0.to_string()),
            tag.rule_name,
            tag.workflow_id.map(|w| w.0.to_string()),
            tag.dry_run as i64,
        ],
    )?;
    Ok(())
}

pub(super) fn clear_envelope_scheduled(
    conn: &Connection,
    message_id: &crate::domain::message::MessageId,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE envelopes
            SET scheduled_at = NULL,
                scheduled_action_type = NULL,
                scheduled_action_dest = NULL,
                scheduled_rule_id = NULL,
                scheduled_rule_name = NULL,
                scheduled_workflow_id = NULL,
                scheduled_dry_run = 0
          WHERE id = ?1",
        params![message_id.0.to_string()],
    )?;
    Ok(())
}

pub(super) fn insert_rule_action_log(
    conn: &Connection,
    e: &crate::domain::workflow::RuleActionLogEntry,
) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO workflow_rule_actions_log (
            id, rule_id, rule_name, action_type, action_dest,
            workflow_id, message_id, subject_snapshot, sender_snapshot,
            result, error_message, ran_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            e.id.to_string(),
            e.rule_id.map(|r| r.0.to_string()),
            e.rule_name,
            e.action.as_str(),
            e.action_dest,
            e.workflow_id.map(|w| w.0.to_string()),
            e.message_id.0.to_string(),
            e.subject_snapshot,
            e.sender_snapshot,
            e.result.as_str(),
            e.error_message,
            e.ran_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub(super) fn delete_workflow_rule(
    conn: &Connection,
    id: &crate::domain::workflow::WorkflowRuleId,
) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM workflow_rules WHERE id = ?1",
        params![id.0.to_string()],
    )?;
    Ok(())
}

pub(super) fn set_workflow_rule_enabled(
    conn: &Connection,
    id: &crate::domain::workflow::WorkflowRuleId,
    enabled: bool,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE workflow_rules SET enabled = ?2 WHERE id = ?1",
        params![id.0.to_string(), enabled as i64],
    )?;
    Ok(())
}

pub(super) fn increment_workflow_rule_hit(
    conn: &Connection,
    id: &crate::domain::workflow::WorkflowRuleId,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE workflow_rules
            SET hit_count = hit_count + 1, last_hit_at = ?2
          WHERE id = ?1",
        params![id.0.to_string(), Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

pub(super) fn add_workflow_training(
    conn: &mut Connection,
    ids: &[MessageId],
) -> Result<(), DbError> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO workflow_training_candidates
                (envelope_id, added_at)
             VALUES (?1, ?2)",
        )?;
        let now = Utc::now().to_rfc3339();
        for id in ids {
            stmt.execute(params![id.0.to_string(), now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn remove_workflow_training(
    conn: &mut Connection,
    ids: &[MessageId],
) -> Result<(), DbError> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "DELETE FROM workflow_training_candidates WHERE envelope_id = ?1",
        )?;
        for id in ids {
            stmt.execute(params![id.0.to_string()])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn clear_workflow_training(conn: &Connection) -> Result<(), DbError> {
    conn.execute("DELETE FROM workflow_training_candidates", [])?;
    Ok(())
}

fn rule_mode_to_str(m: crate::domain::workflow::RuleMode) -> &'static str {
    use crate::domain::workflow::RuleMode as M;
    match m {
        M::Auto => "auto",
        M::Confirm => "confirm",
    }
}

fn address_list_to_text(addrs: &[crate::domain::message::Address]) -> String {
    addrs
        .iter()
        .map(|a| match &a.name {
            Some(n) if !n.is_empty() => format!("{n} <{}>", a.email),
            _ => a.email.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Atomarer Settings-Import. Eine einzige Transaktion klammert alle
/// Inserts; bei Fehler an irgendeiner Stelle rollback. So bleibt der User
/// niemals mit halb-importierten Konten / Regeln zurück.
///
/// `Transaction` derefed `Connection` — wir reichen `&*tx` an die
/// bestehenden `insert_*`-Helfer durch, statt sie zu duplizieren. Aliase
/// werden inline inserted (kein Reuse von `replace_aliases`, weil das
/// eine eigene Transaktion öffnen würde).
pub(super) fn import_bundle(
    conn: &mut Connection,
    plan: crate::application::backup::ImportPlan,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    {
        let mut alias_stmt = tx.prepare(
            "INSERT INTO account_aliases (id, account_id, email, from_name)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for planned in &plan.accounts {
            insert_account(&tx, &planned.account)?;
            for alias in &planned.aliases {
                alias_stmt.execute(params![
                    alias.id.to_string(),
                    planned.account.id.0.to_string(),
                    alias.email,
                    alias.from_name,
                ])?;
            }
        }
    }
    for rule in &plan.spam_rules {
        insert_spam_rule(&tx, rule)?;
    }
    for wf in &plan.workflows {
        insert_workflow(&tx, wf)?;
    }
    for wfr in &plan.workflow_rules {
        insert_workflow_rule(&tx, wfr)?;
    }
    tx.commit()?;
    Ok(())
}

// ─── Contacts ─────────────────────────────────────────────────────────

use crate::domain::contact::{Contact, ContactId};

/// Insert + optionale Initial-Email-Verknüpfung in einer Transaktion.
/// Wenn `initial_email` schon einem ANDEREN Kontakt gehört, fliegt der
/// UNIQUE-Fehler von contact_emails.email zurück — der Caller muss
/// vorher checken (Frontend-Flow: erst `get_contact_for_email`, dann
/// entscheiden ob neuer Kontakt oder Edit des existierenden).
pub(super) fn create_contact(
    conn: &mut Connection,
    c: &Contact,
    initial_email: Option<&str>,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    insert_contact_row(&tx, c)?;
    fts_upsert_contact(&tx, c)?;
    if let Some(email) = initial_email {
        let normalized = email.trim().to_lowercase();
        if !normalized.is_empty() {
            // Erste Adresse ist automatisch primary — sonst hätte der
            // "Mail schreiben"-Knopf keinen Default.
            tx.execute(
                "INSERT INTO contact_emails (contact_id, email, is_primary)
                 VALUES (?1, ?2, 1)",
                params![c.id.0.to_string(), normalized],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn update_contact(conn: &mut Connection, c: &Contact) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE contacts SET
            display_name  = ?2,
            organization  = ?3,
            job_title     = ?4,
            phone         = ?5,
            mobile        = ?6,
            street        = ?7,
            zip           = ?8,
            city          = ?9,
            country       = ?10,
            website       = ?11,
            notes         = ?12,
            origin        = ?13,
            pinned        = ?14,
            last_extracted_envelope_id = ?15,
            updated_at    = ?16
         WHERE id = ?1",
        params![
            c.id.0.to_string(),
            c.display_name,
            c.organization,
            c.job_title,
            c.phone,
            c.mobile,
            c.street,
            c.zip,
            c.city,
            c.country,
            c.website,
            c.notes,
            c.origin.as_db_str(),
            c.pinned as i64,
            c.last_extracted_envelope_id,
            c.updated_at.to_rfc3339(),
        ],
    )?;
    fts_upsert_contact(&tx, c)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn delete_contact(
    conn: &mut Connection,
    contact_id: &ContactId,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    // FTS5 ist nicht FK-aware → manuell aufräumen bevor die Hauptzeile
    // weg ist (sonst keine Möglichkeit mehr, den fts-Eintrag zu finden).
    tx.execute(
        "DELETE FROM fts_contacts WHERE contact_id = ?1",
        params![contact_id.0.to_string()],
    )?;
    // contact_emails cascadet via FK ON DELETE CASCADE.
    tx.execute(
        "DELETE FROM contacts WHERE id = ?1",
        params![contact_id.0.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

pub(super) fn add_contact_email(
    conn: &mut Connection,
    contact_id: &ContactId,
    email: &str,
    is_primary: bool,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    let normalized = email.trim().to_lowercase();
    if is_primary {
        // Erst alle bisherigen primary's clearen, sonst hätten wir N>1
        // primary auf demselben Kontakt.
        tx.execute(
            "UPDATE contact_emails SET is_primary = 0 WHERE contact_id = ?1",
            params![contact_id.0.to_string()],
        )?;
    }
    tx.execute(
        "INSERT INTO contact_emails (contact_id, email, is_primary)
         VALUES (?1, ?2, ?3)",
        params![contact_id.0.to_string(), normalized, is_primary as i64],
    )?;
    // FTS-Eintrag refreshen — neue Email wird in den indexierten
    // Strings nicht reflektiert, aber das Schema indiziert eh nur
    // Stamm-Felder. Ein no-op wäre OK; wir lassen's.
    tx.commit()?;
    Ok(())
}

pub(super) fn remove_contact_email(
    conn: &Connection,
    contact_id: &ContactId,
    email: &str,
) -> Result<(), DbError> {
    let normalized = email.trim().to_lowercase();
    conn.execute(
        "DELETE FROM contact_emails WHERE contact_id = ?1 AND email = ?2",
        params![contact_id.0.to_string(), normalized],
    )?;
    Ok(())
}

pub(super) fn set_primary_contact_email(
    conn: &mut Connection,
    contact_id: &ContactId,
    email: &str,
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    let normalized = email.trim().to_lowercase();
    tx.execute(
        "UPDATE contact_emails SET is_primary = 0 WHERE contact_id = ?1",
        params![contact_id.0.to_string()],
    )?;
    let n = tx.execute(
        "UPDATE contact_emails SET is_primary = 1
         WHERE contact_id = ?1 AND email = ?2",
        params![contact_id.0.to_string(), normalized],
    )?;
    if n == 0 {
        // Adresse gehört nicht zum Kontakt — wir lassen den Tx
        // committen aber loggen es.
        tracing::warn!(
            contact_id = %contact_id.0,
            email = %normalized,
            "set_primary_contact_email: no matching row"
        );
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn record_extraction_miss(
    conn: &Connection,
    email: &str,
    envelope_id: &MessageId,
) -> Result<(), DbError> {
    let normalized = email.trim().to_lowercase();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO extraction_misses (email, last_attempted_envelope_id, attempted_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(email) DO UPDATE SET
            last_attempted_envelope_id = excluded.last_attempted_envelope_id,
            attempted_at               = excluded.attempted_at",
        params![normalized, envelope_id.0.to_string(), now],
    )?;
    Ok(())
}

/// Compose-Send-Side-Effect — typed-in addresses werden NICHT
/// own-gefiltert (User darf an sich selbst schreiben). Brand-new
/// addresses bekommen send_count=1; existierende werden nur
/// recency-refreshed (kein send_count-Bump um Doppelzählung mit
/// dem Sync-Side-Effect zu vermeiden, der auch send_count++ macht
/// wenn die Sent-Mail per IMAP zurückkommt).
pub(super) fn record_outgoing_addresses(
    conn: &mut Connection,
    addresses: &[Address],
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    let now = Utc::now().to_rfc3339();
    for addr in addresses {
        let email = addr.email.trim().to_lowercase();
        if email.is_empty() || !email.contains('@') {
            continue;
        }
        let role_flag = is_role_address(&email) as i64;
        let display_name = addr.name.as_deref().unwrap_or("").trim();
        tx.execute(
            "INSERT INTO address_history (
                email, display_name, first_seen_at, last_seen_at,
                send_count, recv_count, is_role
             ) VALUES (?1, NULLIF(?2, ''), ?3, ?3, 1, 0, ?4)
             ON CONFLICT(email) DO UPDATE SET
                display_name = COALESCE(NULLIF(excluded.display_name, ''), address_history.display_name),
                last_seen_at = MAX(address_history.last_seen_at, excluded.last_seen_at)",
            params![email, display_name, now, role_flag],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Helper: schreibt die Stammdaten-Zeile (kein FTS, kein Email-Insert).
fn insert_contact_row(tx: &Transaction<'_>, c: &Contact) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO contacts (
            id, display_name, organization, job_title, phone, mobile,
            street, zip, city, country, website, notes, origin, pinned,
            last_extracted_envelope_id, created_at, updated_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17
         )",
        params![
            c.id.0.to_string(),
            c.display_name,
            c.organization,
            c.job_title,
            c.phone,
            c.mobile,
            c.street,
            c.zip,
            c.city,
            c.country,
            c.website,
            c.notes,
            c.origin.as_db_str(),
            c.pinned as i64,
            c.last_extracted_envelope_id,
            c.created_at.to_rfc3339(),
            c.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

// ─── Tags ─────────────────────────────────────────────────────────────

use crate::domain::contact::{Tag, TagId};

/// Upsert-by-name (case-insensitive). Wenn ein Tag mit demselben Namen
/// existiert, wird seine ID zurückgegeben; sonst frischen anlegen mit
/// neuer UUID. Optional `color` wird beim Neu-Insert gesetzt; wenn der
/// Tag schon existiert, bleibt die alte color erhalten (Caller müsste
/// `update_tag` rufen um sie zu ändern — verhindert dass der pi-
/// Auto-Linker beim erneuten Match versehentlich Farben overrided).
pub(super) fn upsert_tag(
    conn: &mut Connection,
    name: &str,
    color: Option<&str>,
) -> Result<TagId, DbError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(DbError::Sqlite(rusqlite::Error::InvalidQuery));
    }
    // Versuche zuerst lookup.
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE",
            params![trimmed],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(TagId(uuid::Uuid::parse_str(&id).map_err(|e| {
            DbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?));
    }
    // Insert.
    let new_id = uuid::Uuid::new_v4();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO tags (id, name, color, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![new_id.to_string(), trimmed, color, now],
    )?;
    Ok(TagId(new_id))
}

pub(super) fn update_tag(conn: &Connection, t: &Tag) -> Result<(), DbError> {
    conn.execute(
        "UPDATE tags SET name = ?2, color = ?3 WHERE id = ?1",
        params![t.id.0.to_string(), t.name, t.color],
    )?;
    Ok(())
}

pub(super) fn delete_tag(conn: &Connection, tag_id: &TagId) -> Result<(), DbError> {
    // contact_tags cascadet via FK ON DELETE CASCADE.
    conn.execute(
        "DELETE FROM tags WHERE id = ?1",
        params![tag_id.0.to_string()],
    )?;
    Ok(())
}

/// Atomarer Replace der Tag-Membership: alle bestehenden Verknüpfungen
/// löschen, dann die neuen einfügen. Beides in einer Transaktion, sodass
/// kein "halb-gesetzter" Tag-Zustand sichtbar wird.
pub(super) fn replace_contact_tags(
    conn: &mut Connection,
    contact_id: &ContactId,
    tag_ids: &[TagId],
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM contact_tags WHERE contact_id = ?1",
        params![contact_id.0.to_string()],
    )?;
    if !tag_ids.is_empty() {
        // Wir bauen den Bulk-Insert mit ?-Pairs damit's eine Statement-
        // Kompilierung bleibt bei N Tags.
        let placeholders = tag_ids
            .iter()
            .map(|_| "(?, ?)")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO contact_tags (contact_id, tag_id) VALUES {placeholders}"
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for tid in tag_ids {
            params_vec.push(Box::new(contact_id.0.to_string()));
            params_vec.push(Box::new(tid.0.to_string()));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        tx.execute(&sql, params_refs.as_slice())?;
    }
    tx.commit()?;
    Ok(())
}

/// Helper: FTS-Index-Eintrag refreshen. Wie bei `fts_envelopes` machen
/// wir DELETE + INSERT (contentless FTS5 unterstützt kein UPSERT).
fn fts_upsert_contact(tx: &Transaction<'_>, c: &Contact) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM fts_contacts WHERE contact_id = ?1",
        params![c.id.0.to_string()],
    )?;
    tx.execute(
        "INSERT INTO fts_contacts (
            contact_id, display_name, organization, job_title, phone, city, notes
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            c.id.0.to_string(),
            c.display_name,
            c.organization.as_deref().unwrap_or(""),
            c.job_title.as_deref().unwrap_or(""),
            c.phone.as_deref().unwrap_or(""),
            c.city.as_deref().unwrap_or(""),
            c.notes,
        ],
    )?;
    Ok(())
}
