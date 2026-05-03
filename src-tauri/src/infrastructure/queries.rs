// Read-side query helpers. Every function takes a pool-checkout connection;
// WAL mode lets these run concurrently with the writer and with each other.
// Heavier queries can be moved onto `tokio::task::spawn_blocking` by callers.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, types::ToSql, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::account::{AccountAlias, AccountId, SyncMode};
use crate::domain::folder::FolderId;
use crate::domain::message::MessageId;

use super::db::DbError;

pub type ReadConn = PooledConnection<SqliteConnectionManager>;

/// Public shape of an account for the UI — deliberately omits the keyring
/// entry name and any credential material.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummary {
    pub id: AccountId,
    pub display_name: String,
    pub address: String,
    pub from_name: String,
    pub color: String,
    pub signature: Option<String>,
    pub signature_html: Option<String>,
    pub archive_folder: String,
    pub sent_folder: String,
    pub drafts_folder: String,
    pub trash_folder: String,
    pub spam_folder: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    /// Workflow flag: auto-archive the parent message after a successful reply.
    #[serde(default)]
    pub archive_on_reply: bool,
    /// Background-prefetch window in days (0 = disabled).
    #[serde(default)]
    pub prefetch_days: i64,
    /// Wie der Background-Sync läuft (IDLE/Polling/beides). Frontend
    /// rendert ein Dropdown im Account-Edit-Form, das diesen Wert
    /// schreibt; der per-Account-Actor wird beim Speichern respawned
    /// damit die Änderung sofort wirkt.
    #[serde(default)]
    pub sync_mode: SyncMode,
    /// Provider-Verhalten: speichert der SMTP-Server Sent-Kopien
    /// automatisch? Wird beim Account-Setup einmalig per Probe-Mail
    /// ermittelt und kann manuell überschrieben werden.
    #[serde(default)]
    pub server_stores_sent: bool,
    /// Additional sender identities (e.g. support@, sales@). Fetched in the
    /// same round-trip as the account row so the Compose dropdown is
    /// populated without a second invoke.
    #[serde(default)]
    pub aliases: Vec<AccountAlias>,
}

fn map_account_summary(row: &Row<'_>) -> rusqlite::Result<AccountSummary> {
    let id_str: String = row.get(0)?;
    let sync_mode_str: String = row.get(20)?;
    Ok(AccountSummary {
        id: AccountId(parse_uuid_rs(&id_str)?),
        display_name: row.get(1)?,
        address: row.get(2)?,
        from_name: row.get(3)?,
        color: row.get(4)?,
        signature: row.get(5)?,
        signature_html: row.get(6)?,
        archive_folder: row.get(7)?,
        sent_folder: row.get(8)?,
        drafts_folder: row.get(9)?,
        trash_folder: row.get(10)?,
        spam_folder: row.get(11)?,
        imap_host: row.get(12)?,
        imap_port: row.get::<_, i64>(13)? as u16,
        imap_tls: row.get::<_, i64>(14)? != 0,
        smtp_host: row.get(15)?,
        smtp_port: row.get::<_, i64>(16)? as u16,
        smtp_tls: row.get::<_, i64>(17)? != 0,
        archive_on_reply: row.get::<_, i64>(18)? != 0,
        prefetch_days: row.get::<_, i64>(19)?,
        sync_mode: SyncMode::from_db_str(&sync_mode_str),
        server_stores_sent: row.get::<_, i64>(21)? != 0,
        // Aliases are filled in by the enclosing function; map_account_summary
        // just does a single row and doesn't see the alias table.
        aliases: Vec::new(),
    })
}

const ACCOUNT_COLS: &str =
    "id, display_name, address, from_name, color, signature, signature_html,
     archive_folder, sent_folder, drafts_folder, trash_folder, spam_folder,
     imap_host, imap_port, imap_tls, smtp_host, smtp_port, smtp_tls,
     archive_on_reply, prefetch_days, sync_mode, server_stores_sent";

pub fn list_accounts(conn: &ReadConn) -> Result<Vec<AccountSummary>, DbError> {
    let sql = format!("SELECT {ACCOUNT_COLS} FROM accounts ORDER BY created_at ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_account_summary)?;
    let mut accounts: Vec<AccountSummary> = rows.collect::<Result<Vec<_>, _>>()?;
    // One batch fetch of all aliases, grouped in memory — cheaper than N queries.
    let aliases_by_account = list_aliases_grouped(conn)?;
    for a in &mut accounts {
        if let Some(list) = aliases_by_account.get(&a.id) {
            a.aliases = list.clone();
        }
    }
    Ok(accounts)
}

pub fn get_account(
    conn: &ReadConn,
    id: &AccountId,
) -> Result<Option<AccountSummary>, DbError> {
    let sql = format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params![id.0.to_string()])?;
    if let Some(row) = rows.next()? {
        let mut summary = map_account_summary(row)?;
        summary.aliases = list_aliases_for(conn, id)?;
        Ok(Some(summary))
    } else {
        Ok(None)
    }
}

fn list_aliases_for(
    conn: &ReadConn,
    account_id: &AccountId,
) -> Result<Vec<AccountAlias>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, email, from_name
           FROM account_aliases
          WHERE account_id = ?1
          ORDER BY email",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![account_id.0.to_string()],
        |row| {
            let id_str: String = row.get(0)?;
            let account_str: String = row.get(1)?;
            Ok(AccountAlias {
                id: parse_uuid_rs(&id_str)?,
                account_id: AccountId(parse_uuid_rs(&account_str)?),
                email: row.get(2)?,
                from_name: row.get(3)?,
            })
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

fn list_aliases_grouped(
    conn: &ReadConn,
) -> Result<std::collections::HashMap<AccountId, Vec<AccountAlias>>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, email, from_name
           FROM account_aliases
          ORDER BY account_id, email",
    )?;
    let rows = stmt.query_map([], |row| {
        let id_str: String = row.get(0)?;
        let account_str: String = row.get(1)?;
        Ok(AccountAlias {
            id: parse_uuid_rs(&id_str)?,
            account_id: AccountId(parse_uuid_rs(&account_str)?),
            email: row.get(2)?,
            from_name: row.get(3)?,
        })
    })?;
    let mut out: std::collections::HashMap<AccountId, Vec<AccountAlias>> =
        std::collections::HashMap::new();
    for r in rows {
        let a = r?;
        out.entry(a.account_id).or_default().push(a);
    }
    Ok(out)
}

/// Light-weight row for list views. The full `Envelope` is only materialized
/// when the user opens a single message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvelopeSummary {
    pub id: MessageId,
    pub account_id: AccountId,
    pub account_color: String,
    pub folder_id: FolderId,
    pub subject: String,
    pub from_first: String,
    pub date: DateTime<Utc>,
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub forwarded: bool,
    pub junk: bool,
    pub body_cached: bool,
    /// Drives the paperclip glyph in the inbox list. Heuristic at sync
    /// time, exact once the body has been parsed and stored — see
    /// `db::store_body`.
    pub has_attachments: bool,
    /// Optional ScheduledAction-Tag — wenn gesetzt, rendert das Frontend
    /// einen Auto-Rule-Marker mit Hover-Tooltip ("Aktion fällig in 4 Tagen
    /// → Archiv durch Regel 'Newsletter'"). `None` bei Mails, die keine
    /// Workflow-Regel mit delay_minutes > 0 erfasst hat (oder die sofort
    /// ausgeführt wurde). Siehe Migration 0025.
    #[serde(default)]
    pub scheduled: Option<crate::domain::workflow::ScheduledActionTag>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    pub message_id: MessageId,
    pub plain_text: Option<String>,
    pub html_text: Option<String>,
    pub downloaded_at: DateTime<Utc>,
}

const ENVELOPE_SUMMARY_COLS: &str =
    "e.id, e.account_id, a.color, e.folder_id, e.subject, e.from_json,
     e.date_utc, e.seen, e.answered, e.flagged, e.forwarded, e.junk,
     e.body_cached, e.has_attachments,
     e.scheduled_at, e.scheduled_action_type, e.scheduled_action_dest,
     e.scheduled_rule_id, e.scheduled_rule_name, e.scheduled_workflow_id,
     e.scheduled_dry_run";

/// Whitelist of unified-folder view names that are safe to interpolate into
/// SQL via `format!`. Every site that builds a query with a view name must
/// route the name through `assert_unified_view` so a future maintainer who
/// accidentally lets a non-static string flow into here gets a loud failure
/// instead of an SQL-injection footgun.
///
/// If you add a new `unified_<name>` view in a migration, add the name here.
const UNIFIED_VIEW_NAMES: &[&str] = &[
    "unified_inbox",
    "unified_archive",
    "unified_sent",
    "unified_drafts",
    "unified_trash",
    "unified_spam",
    "unified_starred",
];

/// Defense-in-depth check before string-interpolating a view name into SQL.
///
/// The two existing call sites (`list_unified_folder`,
/// `list_unified_unread_counts`) already pick the view from a static
/// `match` / `const` table, so today this is belt-and-suspenders. The
/// value is in the future-proofing: if anyone ever adds a code path that
/// derives the view name from input, this function fails closed instead
/// of constructing an injectable statement.
///
/// Debug builds panic to make the bug immediately visible during testing;
/// release builds fall back to a parameter-name error so the request
/// surfaces as "Suchausdruck ungültig" instead of corrupting state.
fn assert_unified_view(view: &str) -> Result<&str, DbError> {
    if UNIFIED_VIEW_NAMES.iter().any(|v| *v == view) {
        return Ok(view);
    }
    debug_assert!(
        false,
        "view name `{view}` is not in UNIFIED_VIEW_NAMES — \
         did you skip the whitelist?"
    );
    Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
        format!("unsafe view name: {view}"),
    )))
}

/// Server-known IMAP folder for a given account. Shown in the sidebar's
/// per-account expander so the user can dive into any mailbox, not just the
/// canonical five. Counts are computed inline — fast on WAL reads and saves
/// a second round-trip per expander.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSummary {
    pub id: FolderId,
    pub account_id: AccountId,
    pub name: String,
    pub total: u32,
    pub unread: u32,
    /// Per-folder Sync-Opt-out. Default `true` beim Entdecken via IMAP-LIST.
    /// Der Sync respektiert das Flag sowohl im Eager-Pass für Specials als
    /// auch in der Lazy-on-Open-Logik für alle anderen Ordner.
    pub sync_enabled: bool,
}

/// Metadata block the lazy-sync path needs to decide whether to
/// round-trip the server at all. Keeping it in one row keeps the
/// on-open hot path from fanning out into three separate queries.
#[derive(Debug, Clone)]
pub struct FolderMeta {
    pub account_id: AccountId,
    pub name: String,
    pub sync_enabled: bool,
    pub last_sync_ts: Option<DateTime<Utc>>,
}

pub fn get_folder_meta(
    conn: &ReadConn,
    folder_id: &FolderId,
) -> Result<Option<FolderMeta>, DbError> {
    conn.query_row(
        "SELECT account_id, name, sync_enabled, last_sync_ts
           FROM folders
          WHERE id = ?1",
        params![folder_id.0.to_string()],
        |row| {
            let account_id_str: String = row.get(0)?;
            let last: Option<String> = row.get(3)?;
            Ok(FolderMeta {
                account_id: AccountId(parse_uuid_rs(&account_id_str)?),
                name: row.get(1)?,
                sync_enabled: row.get::<_, i64>(2)? != 0,
                last_sync_ts: last
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc)),
            })
        },
    )
    .optional()
    .map_err(DbError::from)
}

/// All locally-cached IMAP UIDs for a folder. Used by the sync
/// reconciliation pass: the server-side set is compared against this
/// list and any UID present locally but absent on the server is
/// pruned. Soft-deleted rows are included — they still occupy the
/// `(folder_id, imap_uid)` unique slot and need to be cleared out
/// when the server confirms the message is gone.
pub fn list_envelope_uids(
    conn: &ReadConn,
    folder_id: &FolderId,
) -> Result<Vec<u32>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT imap_uid FROM envelopes WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map(params![folder_id.0.to_string()], |row| {
        Ok(row.get::<_, i64>(0)? as u32)
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// The persisted `UIDVALIDITY` for a folder, if any. Returns `None`
/// when the row exists but the value is still the placeholder 0
/// (initial insert before the first SELECT) — callers treat that as
/// "we have no prior validity to compare against, don't trigger the
/// purge path".
pub fn get_folder_uid_validity(
    conn: &ReadConn,
    folder_id: &FolderId,
) -> Result<Option<u32>, DbError> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT uid_validity FROM folders WHERE id = ?1",
            params![folder_id.0.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(v.and_then(|n| if n > 0 { Some(n as u32) } else { None }))
}

/// Smallest cached IMAP UID for a folder, ignoring soft-deleted rows.
/// Used by the scroll-to-bottom pager: pass this as the pivot to
/// `sync_folder_older`, which then asks the server for `UID 1:N-1`.
/// `None` means the folder has nothing cached yet — the caller
/// shouldn't try to page, it should kick off `sync_folder_recent` first.
pub fn oldest_cached_uid(
    conn: &ReadConn,
    folder_id: &FolderId,
) -> Result<Option<u32>, DbError> {
    let min: Option<i64> = conn.query_row(
        "SELECT MIN(imap_uid) FROM envelopes
          WHERE folder_id = ?1 AND deleted = 0",
        params![folder_id.0.to_string()],
        |row| row.get(0),
    )?;
    Ok(min.map(|v| v as u32))
}

/// Resolve canonical folder keys ("inbox", "archive", …) to the matching
/// `FolderId` for each account. Returned tuples are `(account_id, folder_id)`,
/// in deterministic insertion order across accounts. Skips accounts that
/// have no matching folder synced yet (e.g. an Archive folder that the
/// server hasn't exposed via LIST). Used by the canonical-archive paging
/// path: `sync_unified_folder_older` iterates these and calls
/// `sync_folder_older` per account.
///
/// The `starred` pseudo-folder isn't a real folder — it's the `flagged`
/// flag across the account. Returning anything for it would be misleading,
/// so the caller is expected to skip it (we return an empty Vec).
pub fn folder_ids_for_canonical(
    conn: &ReadConn,
    folder_key: &str,
    account_filter: Option<&AccountId>,
) -> Result<Vec<(AccountId, FolderId)>, DbError> {
    // Map the canonical key to the predicate that matches the per-account
    // folder. Mirrors the `search_advanced` table — keep them in sync.
    let predicate = match folder_key {
        "inbox" => "UPPER(f.name) = 'INBOX'",
        "archive" => "f.name = a.archive_folder",
        "sent" => "f.name = a.sent_folder",
        "drafts" => "f.name = a.drafts_folder",
        "trash" => "f.name = a.trash_folder",
        "spam" => "f.name = a.spam_folder",
        // Starred is a flag filter, not a folder — no row to return.
        "starred" => return Ok(Vec::new()),
        _ => {
            return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                format!("unknown folder key: {folder_key}"),
            )))
        }
    };

    let sql = if account_filter.is_some() {
        format!(
            "SELECT a.id, f.id
               FROM folders f
               JOIN accounts a ON a.id = f.account_id
              WHERE {predicate}
                AND a.id = ?1
              ORDER BY a.created_at, f.name"
        )
    } else {
        format!(
            "SELECT a.id, f.id
               FROM folders f
               JOIN accounts a ON a.id = f.account_id
              WHERE {predicate}
              ORDER BY a.created_at, f.name"
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(id) = account_filter {
        stmt.query_map(params![id.0.to_string()], |row| {
            let acc: String = row.get(0)?;
            let fol: String = row.get(1)?;
            Ok((AccountId(parse_uuid_rs(&acc)?), FolderId(parse_uuid_rs(&fol)?)))
        })?
        .collect::<Result<Vec<_>, _>>()
    } else {
        stmt.query_map([], |row| {
            let acc: String = row.get(0)?;
            let fol: String = row.get(1)?;
            Ok((AccountId(parse_uuid_rs(&acc)?), FolderId(parse_uuid_rs(&fol)?)))
        })?
        .collect::<Result<Vec<_>, _>>()
    };
    rows.map_err(DbError::from)
}

pub fn list_account_folders(
    conn: &ReadConn,
    account_id: &AccountId,
) -> Result<Vec<FolderSummary>, DbError> {
    // Left-join lets us show folders that have been seen via LIST but haven't
    // synced messages yet (fresh account, Archive folder you never use, …).
    let mut stmt = conn.prepare(
        "SELECT f.id, f.account_id, f.name, f.sync_enabled,
                COALESCE(SUM(CASE WHEN e.deleted = 0 THEN 1 ELSE 0 END), 0) AS total,
                COALESCE(SUM(CASE WHEN e.deleted = 0 AND e.seen = 0 THEN 1 ELSE 0 END), 0) AS unread
           FROM folders f
           LEFT JOIN envelopes e ON e.folder_id = f.id
          WHERE f.account_id = ?1
          GROUP BY f.id
          ORDER BY f.name",
    )?;
    let rows = stmt.query_map(params![account_id.0.to_string()], |row| {
        let folder_id_str: String = row.get(0)?;
        let account_id_str: String = row.get(1)?;
        Ok(FolderSummary {
            id: FolderId(parse_uuid_rs(&folder_id_str)?),
            account_id: AccountId(parse_uuid_rs(&account_id_str)?),
            name: row.get(2)?,
            sync_enabled: row.get::<_, i64>(3)? != 0,
            total: row.get::<_, i64>(4)? as u32,
            unread: row.get::<_, i64>(5)? as u32,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// One message that the background worker should prefetch. Only the fields
/// needed for the IMAP round-trip, grouped by folder upstream.
#[derive(Debug, Clone)]
pub struct PrefetchCandidate {
    pub message_id: MessageId,
    pub folder_name: String,
    pub imap_uid: u32,
}

pub fn list_prefetch_candidates(
    conn: &ReadConn,
    account_id: &AccountId,
    since: &DateTime<Utc>,
    max_size_bytes: i64,
) -> Result<Vec<PrefetchCandidate>, DbError> {
    // Spam + Trash are excluded — the "keep inbox lean" workflow actively
    // avoids those folders, prefetching their bodies would waste bandwidth.
    // Archive stays in because re-reading archived mail is a core path.
    let mut stmt = conn.prepare(
        "SELECT e.id, f.name, e.imap_uid
           FROM envelopes e
           JOIN folders  f ON f.id = e.folder_id
           JOIN accounts a ON a.id = e.account_id
          WHERE e.account_id  = ?1
            AND e.deleted     = 0
            AND e.body_cached = 0
            AND e.date_utc    >= ?2
            AND e.size_bytes  <= ?3
            AND f.name        != a.spam_folder
            AND f.name        != a.trash_folder
          ORDER BY e.date_utc DESC",
    )?;
    let rows = stmt.query_map(
        params![account_id.0.to_string(), since.to_rfc3339(), max_size_bytes],
        |row| {
            let id_str: String = row.get(0)?;
            Ok(PrefetchCandidate {
                message_id: MessageId(parse_uuid_rs(&id_str)?),
                folder_name: row.get(1)?,
                imap_uid: row.get::<_, i64>(2)? as u32,
            })
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

pub fn list_envelopes_in_folder(
    conn: &ReadConn,
    folder_id: &FolderId,
    limit: u32,
    offset: u32,
) -> Result<Vec<EnvelopeSummary>, DbError> {
    let sql = format!(
        "SELECT {ENVELOPE_SUMMARY_COLS}
           FROM envelopes e
           JOIN accounts  a ON a.id = e.account_id
          WHERE e.folder_id = ?1 AND e.deleted = 0
          ORDER BY e.date_utc DESC
          LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![folder_id.0.to_string(), limit, offset],
        map_envelope_summary,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Unified list across accounts for a canonical folder key. The key is one of
/// `"inbox" | "archive" | "sent" | "drafts" | "trash"`. Anything else returns
/// an `InvalidParameterName` so an unknown key from the frontend fails loudly
/// instead of silently mapping to something like INBOX.
///
/// Optional `account_filter` narrows the list to a single account (used by
/// the account-filter bar); `None` means "all accounts".
pub fn list_unified_folder(
    conn: &ReadConn,
    folder_key: &str,
    account_filter: Option<&AccountId>,
    limit: u32,
    offset: u32,
) -> Result<Vec<EnvelopeSummary>, DbError> {
    // View name is constrained to a known-static set so string interpolation
    // into the SQL is safe (no user input reaches the statement). The
    // `assert_unified_view` re-check below is defense-in-depth: if a future
    // maintainer ever adds a non-literal arm here, the whitelist still
    // refuses to let it through.
    let view = match folder_key {
        "inbox" => "unified_inbox",
        "archive" => "unified_archive",
        "sent" => "unified_sent",
        "drafts" => "unified_drafts",
        "trash" => "unified_trash",
        "spam" => "unified_spam",
        "starred" => "unified_starred",
        _ => {
            return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                format!("unknown folder key: {folder_key}"),
            )))
        }
    };
    let view = assert_unified_view(view)?;

    if let Some(id) = account_filter {
        let sql = format!(
            "SELECT {ENVELOPE_SUMMARY_COLS}
               FROM {view} e
               JOIN accounts a ON a.id = e.account_id
              WHERE e.account_id = ?1
              ORDER BY e.date_utc DESC
              LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![id.0.to_string(), limit, offset],
            map_envelope_summary,
        )?;
        return rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from);
    }

    let sql = format!(
        "SELECT {ENVELOPE_SUMMARY_COLS}
           FROM {view} e
           JOIN accounts a ON a.id = e.account_id
          ORDER BY e.date_utc DESC
          LIMIT ?1 OFFSET ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit, offset], map_envelope_summary)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

pub fn search_envelopes(
    conn: &ReadConn,
    query: &str,
    limit: u32,
) -> Result<Vec<EnvelopeSummary>, DbError> {
    let sql = format!(
        "SELECT {ENVELOPE_SUMMARY_COLS}
           FROM fts_envelopes f
           JOIN envelopes     e ON e.rowid = f.rowid
           JOIN accounts      a ON a.id = e.account_id
          WHERE fts_envelopes MATCH ?1 AND e.deleted = 0
          ORDER BY bm25(fts_envelopes)
          LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![query, limit], map_envelope_summary)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Full-text search scoped to one of the canonical folders. FTS5 gives us
/// column selectors (`subject:rechnung`, `from_text:melchinger`), phrase
/// matching (`"exact phrase"`), and boolean operators (`foo OR bar`,
/// `-newsletter`) out of the box — we pass the user's query straight through.
///
/// We can't JOIN the `unified_<folder>` views on `f.rowid` because SQLite
/// views don't pass rowid through transparently — rusqlite then complains
/// about the malformed match, which the caller surfaces as a generic
/// "Suchausdruck ungültig" message. Instead, join directly onto the base
/// tables (envelopes + folders + accounts) and inline the per-folder
/// predicate here.
pub fn search_in_folder(
    conn: &ReadConn,
    folder_key: &str,
    account_filter: Option<&AccountId>,
    query: &str,
    limit: u32,
) -> Result<Vec<EnvelopeSummary>, DbError> {
    let folder_clause = match folder_key {
        "inbox" => "UPPER(fo.name) = 'INBOX'",
        "archive" => "fo.name = a.archive_folder",
        "sent" => "fo.name = a.sent_folder",
        "drafts" => "fo.name = a.drafts_folder",
        "trash" => "fo.name = a.trash_folder",
        "spam" => "fo.name = a.spam_folder",
        // Starred isn't folder-bound — it's a flag filter that
        // excludes trash/spam. FTS query AND with e.flagged + the
        // two exclusions.
        "starred" => "e.flagged = 1 AND fo.name != a.trash_folder AND fo.name != a.spam_folder",
        _ => {
            return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                format!("unknown folder key: {folder_key}"),
            )))
        }
    };

    if let Some(id) = account_filter {
        let sql = format!(
            "SELECT {ENVELOPE_SUMMARY_COLS}
               FROM fts_envelopes f
               JOIN envelopes e ON e.rowid = f.rowid
               JOIN accounts a ON a.id = e.account_id
               JOIN folders  fo ON fo.id = e.folder_id
              WHERE fts_envelopes MATCH ?1
                AND e.deleted = 0
                AND {folder_clause}
                AND e.account_id = ?2
              ORDER BY bm25(fts_envelopes)
              LIMIT ?3"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![query, id.0.to_string(), limit],
            map_envelope_summary,
        )?;
        return rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from);
    }

    let sql = format!(
        "SELECT {ENVELOPE_SUMMARY_COLS}
           FROM fts_envelopes f
           JOIN envelopes e ON e.rowid = f.rowid
           JOIN accounts a ON a.id = e.account_id
           JOIN folders  fo ON fo.id = e.folder_id
          WHERE fts_envelopes MATCH ?1
            AND e.deleted = 0
            AND {folder_clause}
          ORDER BY bm25(fts_envelopes)
          LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![query, limit], map_envelope_summary)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Structured filter set ANDed onto the envelope-list query by
/// `search_advanced`. Each `Option` is opt-in — `None` means "don't
/// constrain". The DSL parser on the frontend turns `is:unread`,
/// `has:attachments`, `since:14 june 2022` etc. into one of these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFilters {
    pub seen: Option<bool>,
    pub flagged: Option<bool>,
    pub answered: Option<bool>,
    pub junk: Option<bool>,
    pub has_attachments: Option<bool>,
    /// RFC3339 inclusive lower bound on `date_utc`.
    pub since: Option<String>,
    /// RFC3339 exclusive upper bound on `date_utc`.
    pub before: Option<String>,
}

/// One-stop search across the four orthogonal axes:
///   * FTS5 free-text (optional — empty `fts` skips the FTS join)
///   * canonical folder key (optional — `None` = across all folders)
///   * account scope (optional — `None` = across all accounts)
///   * structured filter set (flags, attachments, date window)
///
/// Replaces the old `search_envelopes` + `search_in_folder` pair: those
/// each handled two of the four axes, leaving the others untouchable.
/// The DSL parser on the frontend (utils/searchDsl.ts) splits a single
/// query string into these four inputs.
pub fn search_advanced(
    conn: &ReadConn,
    fts: &str,
    folder_key: Option<&str>,
    folder_id: Option<&FolderId>,
    account_filter: Option<&AccountId>,
    filters: &SearchFilters,
    limit: u32,
) -> Result<Vec<EnvelopeSummary>, DbError> {
    let fts_trimmed = fts.trim();
    let use_fts = !fts_trimmed.is_empty();

    // Folder predicate. Same canonical-key set as `search_in_folder`,
    // factored so both the FTS and non-FTS branches use it. None of
    // these strings is user-controlled — they're literal keys from
    // the DSL alias table — so direct interpolation is safe.
    let folder_clause = match folder_key {
        None => None,
        Some("inbox") => Some("UPPER(fo.name) = 'INBOX'".to_string()),
        Some("archive") => Some("fo.name = a.archive_folder".to_string()),
        Some("sent") => Some("fo.name = a.sent_folder".to_string()),
        Some("drafts") => Some("fo.name = a.drafts_folder".to_string()),
        Some("trash") => Some("fo.name = a.trash_folder".to_string()),
        Some("spam") => Some("fo.name = a.spam_folder".to_string()),
        Some("starred") => Some(
            "e.flagged = 1 AND fo.name != a.trash_folder AND fo.name != a.spam_folder"
                .to_string(),
        ),
        Some(other) => {
            return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                format!("unknown folder key: {other}"),
            )))
        }
    };

    // Build dynamic WHERE chain. Positional params (?1, ?2, …) are
    // assigned in the order we push values into `binds`. The first
    // param is reserved for the FTS5 MATCH clause when present —
    // it must come first because the SQL has it in the FROM-side
    // expression.
    let mut where_parts: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn ToSql>> = Vec::new();
    let mut next_idx: usize = 1;

    if use_fts {
        where_parts.push(format!("fts_envelopes MATCH ?{next_idx}"));
        binds.push(Box::new(fts_trimmed.to_string()));
        next_idx += 1;
    }
    where_parts.push("e.deleted = 0".to_string());
    if let Some(clause) = &folder_clause {
        where_parts.push(format!("({clause})"));
    }
    // Optional DB-folder-id pin. Used by the ad-hoc-sub-folder search
    // path: the canonical `folder_key` whitelist only knows the six
    // unified buckets, so a sub-folder pinned via the sidebar passes
    // its `FolderId` through here instead. Both can be set, in which
    // case the AND collapses naturally.
    if let Some(fid) = folder_id {
        where_parts.push(format!("e.folder_id = ?{next_idx}"));
        binds.push(Box::new(fid.0.to_string()));
        next_idx += 1;
    }
    if let Some(id) = account_filter {
        where_parts.push(format!("e.account_id = ?{next_idx}"));
        binds.push(Box::new(id.0.to_string()));
        next_idx += 1;
    }
    if let Some(v) = filters.seen {
        where_parts.push(format!("e.seen = ?{next_idx}"));
        binds.push(Box::new(v as i64));
        next_idx += 1;
    }
    if let Some(v) = filters.flagged {
        where_parts.push(format!("e.flagged = ?{next_idx}"));
        binds.push(Box::new(v as i64));
        next_idx += 1;
    }
    if let Some(v) = filters.answered {
        where_parts.push(format!("e.answered = ?{next_idx}"));
        binds.push(Box::new(v as i64));
        next_idx += 1;
    }
    if let Some(v) = filters.junk {
        where_parts.push(format!("e.junk = ?{next_idx}"));
        binds.push(Box::new(v as i64));
        next_idx += 1;
    }
    if let Some(v) = filters.has_attachments {
        where_parts.push(format!("e.has_attachments = ?{next_idx}"));
        binds.push(Box::new(v as i64));
        next_idx += 1;
    }
    if let Some(s) = &filters.since {
        where_parts.push(format!("e.date_utc >= ?{next_idx}"));
        binds.push(Box::new(s.clone()));
        next_idx += 1;
    }
    if let Some(s) = &filters.before {
        where_parts.push(format!("e.date_utc < ?{next_idx}"));
        binds.push(Box::new(s.clone()));
        next_idx += 1;
    }

    // LIMIT is the last positional arg in both branches.
    let limit_idx = next_idx;
    binds.push(Box::new(limit));

    // FTS branch joins on rowid; non-FTS branch goes directly against
    // `envelopes`. Folder branch needs a `folders` JOIN either way.
    // Order-by differs: FTS branch ranks by bm25, non-FTS goes by date
    // (descending — newest first matches list-view convention).
    let join_folders = folder_clause.is_some();
    let from_clause = if use_fts {
        if join_folders {
            "fts_envelopes f
              JOIN envelopes e ON e.rowid = f.rowid
              JOIN accounts  a ON a.id = e.account_id
              JOIN folders   fo ON fo.id = e.folder_id"
        } else {
            "fts_envelopes f
              JOIN envelopes e ON e.rowid = f.rowid
              JOIN accounts  a ON a.id = e.account_id"
        }
    } else if join_folders {
        "envelopes e
          JOIN accounts a ON a.id = e.account_id
          JOIN folders  fo ON fo.id = e.folder_id"
    } else {
        "envelopes e
          JOIN accounts a ON a.id = e.account_id"
    };
    let order_by = if use_fts {
        "bm25(fts_envelopes)"
    } else {
        "e.date_utc DESC"
    };

    let where_sql = where_parts.join(" AND ");
    let sql = format!(
        "SELECT {ENVELOPE_SUMMARY_COLS}
           FROM {from_clause}
          WHERE {where_sql}
          ORDER BY {order_by}
          LIMIT ?{limit_idx}"
    );

    let mut stmt = conn.prepare(&sql)?;
    let bind_refs: Vec<&dyn ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(bind_refs.as_slice(), map_envelope_summary)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Everything the Reader view and on-demand body fetcher need from an
/// envelope row. Mirrors `domain::message::Envelope` but flattens the
/// address JSON on the way out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvelopeDetail {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub folder_name: String,
    pub imap_uid: u32,
    pub message_id_header: Option<String>,
    pub subject: String,
    pub date: chrono::DateTime<chrono::Utc>,
    pub from: Vec<crate::domain::message::Address>,
    pub to: Vec<crate::domain::message::Address>,
    pub cc: Vec<crate::domain::message::Address>,
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub forwarded: bool,
    pub junk: bool,
    pub body_cached: bool,
}

pub fn get_envelope(conn: &ReadConn, id: &MessageId) -> Result<Option<EnvelopeDetail>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.account_id, e.folder_id, f.name, e.imap_uid,
                e.message_id_header, e.subject, e.date_utc,
                e.from_json, e.to_json, e.cc_json,
                e.seen, e.answered, e.flagged, e.forwarded, e.junk, e.body_cached
           FROM envelopes e
           JOIN folders   f ON f.id = e.folder_id
          WHERE e.id = ?1",
    )?;
    let mut rows = stmt.query(rusqlite::params![id.0.to_string()])?;
    if let Some(row) = rows.next()? {
        let id_str: String = row.get(0)?;
        let account_str: String = row.get(1)?;
        let folder_str: String = row.get(2)?;
        let folder_name: String = row.get(3)?;
        let date_str: String = row.get(7)?;
        let from_json: String = row.get(8)?;
        let to_json: String = row.get(9)?;
        let cc_json: String = row.get(10)?;
        let seen: i64 = row.get(11)?;
        let answered: i64 = row.get(12)?;
        let flagged: i64 = row.get(13)?;
        let forwarded: i64 = row.get(14)?;
        let junk: i64 = row.get(15)?;
        let body_cached: i64 = row.get(16)?;
        Ok(Some(EnvelopeDetail {
            id: MessageId(parse_uuid(&id_str)?),
            account_id: AccountId(parse_uuid(&account_str)?),
            folder_id: FolderId(parse_uuid(&folder_str)?),
            folder_name,
            imap_uid: row.get::<_, i64>(4)? as u32,
            message_id_header: row.get(5)?,
            subject: row.get(6)?,
            date: parse_dt(&date_str)?,
            from: serde_json::from_str(&from_json).unwrap_or_default(),
            to: serde_json::from_str(&to_json).unwrap_or_default(),
            cc: serde_json::from_str(&cc_json).unwrap_or_default(),
            seen: seen != 0,
            answered: answered != 0,
            flagged: flagged != 0,
            forwarded: forwarded != 0,
            junk: junk != 0,
            body_cached: body_cached != 0,
        }))
    } else {
        Ok(None)
    }
}

pub fn get_body(conn: &ReadConn, id: &MessageId) -> Result<Option<Body>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT envelope_id, plain_text, html_text, downloaded_at
           FROM bodies WHERE envelope_id = ?1",
    )?;
    let mut rows = stmt.query(params![id.0.to_string()])?;
    if let Some(row) = rows.next()? {
        let id_str: String = row.get(0)?;
        let downloaded_str: String = row.get(3)?;
        Ok(Some(Body {
            message_id: MessageId(parse_uuid(&id_str)?),
            plain_text: row.get(1)?,
            html_text: row.get(2)?,
            downloaded_at: parse_dt(&downloaded_str)?,
        }))
    } else {
        Ok(None)
    }
}

/// Unread-counts per canonical unified-folder key. Returned as a small
/// Vec so the order is deterministic and JSON-serializable as an object
/// literal on the frontend (we map it to a keyed lookup there).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifiedUnreadCount {
    pub folder: String, // "inbox" | "archive" | "sent" | "drafts" | "trash" | "spam"
    pub unread: u32,
}

/// Count of unread envelopes per unified view. One SQL query per folder
/// — cheap since each view is already filtered by `e.deleted=0` and the
/// right folder-name join. Returns all six folders even when zero, so
/// the UI can show "0" (or hide) without extra lookup logic.
pub fn list_unified_unread_counts(
    conn: &ReadConn,
) -> Result<Vec<UnifiedUnreadCount>, DbError> {
    const FOLDERS: &[(&str, &str)] = &[
        ("inbox", "unified_inbox"),
        ("archive", "unified_archive"),
        ("sent", "unified_sent"),
        ("drafts", "unified_drafts"),
        ("trash", "unified_trash"),
        ("spam", "unified_spam"),
        ("starred", "unified_starred"),
    ];
    let mut out = Vec::with_capacity(FOLDERS.len());
    for (key, view) in FOLDERS {
        // Belt-and-suspenders: even though FOLDERS is a `const`, route the
        // view name through the central whitelist. If someone later swaps
        // the const for a runtime-built table the whitelist still holds.
        let view = assert_unified_view(view)?;
        let sql = format!("SELECT COUNT(*) FROM {view} WHERE seen = 0");
        let count: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
        out.push(UnifiedUnreadCount {
            folder: (*key).to_string(),
            unread: count.max(0) as u32,
        });
    }
    Ok(out)
}

pub fn list_spam_rules(
    conn: &ReadConn,
) -> Result<Vec<crate::domain::spam_rule::SpamRule>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, pattern_type, pattern, enabled, confidence,
                reason, created_at, hit_count
           FROM spam_rules
          ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], map_spam_rule)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

pub fn list_enabled_spam_rules(
    conn: &ReadConn,
    account_filter: Option<&AccountId>,
) -> Result<Vec<crate::domain::spam_rule::SpamRule>, DbError> {
    // Account-scoped rules + global rules (account_id IS NULL) both apply
    // when the caller has a specific account in mind.
    let sql = if account_filter.is_some() {
        "SELECT id, account_id, pattern_type, pattern, enabled, confidence,
                reason, created_at, hit_count
           FROM spam_rules
          WHERE enabled = 1 AND (account_id IS NULL OR account_id = ?1)
          ORDER BY created_at DESC"
    } else {
        "SELECT id, account_id, pattern_type, pattern, enabled, confidence,
                reason, created_at, hit_count
           FROM spam_rules
          WHERE enabled = 1
          ORDER BY created_at DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(id) = account_filter {
        stmt.query_map(params![id.0.to_string()], map_spam_rule)?
            .collect::<Result<Vec<_>, _>>()
    } else {
        stmt.query_map([], map_spam_rule)?
            .collect::<Result<Vec<_>, _>>()
    };
    rows.map_err(DbError::from)
}

fn map_spam_rule(
    row: &Row<'_>,
) -> rusqlite::Result<crate::domain::spam_rule::SpamRule> {
    use crate::domain::spam_rule::{SpamPatternType, SpamRule, SpamRuleId};
    let id_str: String = row.get(0)?;
    let account_opt: Option<String> = row.get(1)?;
    let pattern_type_str: String = row.get(2)?;
    let created_str: String = row.get(7)?;
    let hit_count_i: i64 = row.get(8)?;

    let pattern_type = match pattern_type_str.as_str() {
        "from_email" => SpamPatternType::FromEmail,
        "from_domain" => SpamPatternType::FromDomain,
        "subject_contains" => SpamPatternType::SubjectContains,
        "subject_regex" => SpamPatternType::SubjectRegex,
        "body_contains" => SpamPatternType::BodyContains,
        "header_contains" => SpamPatternType::HeaderContains,
        other => {
            // Unknown variant in DB — surface as sqlite type-error so the
            // row is skipped rather than silently misclassified.
            return Err(rusqlite::Error::InvalidColumnType(
                2,
                format!("unknown pattern_type: {other}"),
                rusqlite::types::Type::Text,
            ));
        }
    };

    Ok(SpamRule {
        id: SpamRuleId(parse_uuid_rs(&id_str)?),
        account_id: account_opt
            .map(|s| parse_uuid_rs(&s).map(AccountId))
            .transpose()?,
        pattern_type,
        pattern: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        confidence: row.get(5)?,
        reason: row.get(6)?,
        created_at: parse_dt_rs(&created_str)?,
        hit_count: hit_count_i.max(0) as u64,
    })
}

// ─── Scheduled-Action-Sweep ────────────────────────────────────────────────

/// Snapshot der Mails, die der Sweeper jetzt anpacken sollte: Tag ist
/// fällig, Mail liegt noch im (passenden) Folder, nicht soft-deleted.
/// Skip-Logik (flagged/answered/dry_run/Folder-Mismatch) macht der
/// Caller in Rust — die DB liefert nur die Kandidaten-Reihen.
#[derive(Debug, Clone)]
pub struct ScheduledEnvelopeRow {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub folder_name: String,
    pub subject: String,
    pub from_first: String,
    pub action: crate::domain::workflow::RuleAction,
    pub action_dest: Option<String>,
    pub rule_id: Option<crate::domain::workflow::WorkflowRuleId>,
    pub rule_name: String,
    pub workflow_id: Option<crate::domain::workflow::WorkflowId>,
    pub dry_run: bool,
    pub flagged: bool,
    pub answered: bool,
}

pub fn list_due_scheduled_envelopes(
    conn: &ReadConn,
    now: &DateTime<Utc>,
    limit: u32,
) -> Result<Vec<ScheduledEnvelopeRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.account_id, e.folder_id, f.name,
                e.subject, e.from_json,
                e.scheduled_action_type, e.scheduled_action_dest,
                e.scheduled_rule_id, e.scheduled_rule_name,
                e.scheduled_workflow_id, e.scheduled_dry_run,
                e.flagged, e.answered
           FROM envelopes e
           JOIN folders f ON f.id = e.folder_id
          WHERE e.scheduled_at IS NOT NULL
            AND e.scheduled_at <= ?1
            AND e.deleted = 0
          ORDER BY e.scheduled_at ASC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![now.to_rfc3339(), limit],
        |row| -> rusqlite::Result<ScheduledEnvelopeRow> {
            use crate::domain::workflow::{RuleAction, WorkflowId, WorkflowRuleId};
            let id_str: String = row.get(0)?;
            let acc_str: String = row.get(1)?;
            let fid_str: String = row.get(2)?;
            let from_json: String = row.get(5)?;
            let action_str: String = row.get(6)?;
            let action = RuleAction::parse(&action_str).ok_or_else(|| {
                rusqlite::Error::InvalidColumnType(
                    6,
                    format!("unknown action_type: {action_str}"),
                    rusqlite::types::Type::Text,
                )
            })?;
            let rule_id_opt: Option<String> = row.get(8)?;
            let workflow_id_opt: Option<String> = row.get(10)?;
            Ok(ScheduledEnvelopeRow {
                id: MessageId(parse_uuid_rs(&id_str)?),
                account_id: AccountId(parse_uuid_rs(&acc_str)?),
                folder_id: FolderId(parse_uuid_rs(&fid_str)?),
                folder_name: row.get(3)?,
                subject: row.get(4)?,
                from_first: first_address_text(&from_json),
                action,
                action_dest: row.get(7)?,
                rule_id: rule_id_opt
                    .as_deref()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .map(WorkflowRuleId),
                rule_name: row
                    .get::<_, Option<String>>(9)?
                    .unwrap_or_default(),
                workflow_id: workflow_id_opt
                    .as_deref()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .map(WorkflowId),
                dry_run: row.get::<_, i64>(11)? != 0,
                flagged: row.get::<_, i64>(12)? != 0,
                answered: row.get::<_, i64>(13)? != 0,
            })
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

pub fn list_rule_action_log(
    conn: &ReadConn,
    rule_filter: Option<&crate::domain::workflow::WorkflowRuleId>,
    limit: u32,
) -> Result<Vec<crate::domain::workflow::RuleActionLogEntry>, DbError> {
    let sql = if rule_filter.is_some() {
        "SELECT id, rule_id, rule_name, action_type, action_dest, workflow_id,
                message_id, subject_snapshot, sender_snapshot,
                result, error_message, ran_at
           FROM workflow_rule_actions_log
          WHERE rule_id = ?1
          ORDER BY ran_at DESC
          LIMIT ?2"
    } else {
        "SELECT id, rule_id, rule_name, action_type, action_dest, workflow_id,
                message_id, subject_snapshot, sender_snapshot,
                result, error_message, ran_at
           FROM workflow_rule_actions_log
          ORDER BY ran_at DESC
          LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(rid) = rule_filter {
        stmt.query_map(
            params![rid.0.to_string(), limit],
            map_rule_action_log,
        )?
        .collect::<Result<Vec<_>, _>>()
    } else {
        stmt.query_map(params![limit], map_rule_action_log)?
            .collect::<Result<Vec<_>, _>>()
    };
    rows.map_err(DbError::from)
}

fn map_rule_action_log(
    row: &Row<'_>,
) -> rusqlite::Result<crate::domain::workflow::RuleActionLogEntry> {
    use crate::domain::workflow::{
        RuleAction, RuleActionLogEntry, RuleActionResult, WorkflowId, WorkflowRuleId,
    };
    let id_str: String = row.get(0)?;
    let rule_id_opt: Option<String> = row.get(1)?;
    let rule_name: String = row.get(2)?;
    let action_str: String = row.get(3)?;
    let action_dest: Option<String> = row.get(4)?;
    let workflow_id_opt: Option<String> = row.get(5)?;
    let message_id_str: String = row.get(6)?;
    let subject_snapshot: String = row.get(7)?;
    let sender_snapshot: String = row.get(8)?;
    let result_str: String = row.get(9)?;
    let error_message: Option<String> = row.get(10)?;
    let ran_at_str: String = row.get(11)?;

    let action = RuleAction::parse(&action_str).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(
            3,
            format!("unknown action_type: {action_str}"),
            rusqlite::types::Type::Text,
        )
    })?;
    let result = match result_str.as_str() {
        "ok" => RuleActionResult::Ok,
        "skipped" => RuleActionResult::Skipped,
        "failed" => RuleActionResult::Failed,
        other => {
            return Err(rusqlite::Error::InvalidColumnType(
                9,
                format!("unknown result: {other}"),
                rusqlite::types::Type::Text,
            ));
        }
    };

    Ok(RuleActionLogEntry {
        id: parse_uuid_rs(&id_str)?,
        rule_id: rule_id_opt
            .map(|s| parse_uuid_rs(&s).map(WorkflowRuleId))
            .transpose()?,
        rule_name,
        action,
        action_dest,
        workflow_id: workflow_id_opt
            .map(|s| parse_uuid_rs(&s).map(WorkflowId))
            .transpose()?,
        message_id: MessageId(parse_uuid_rs(&message_id_str)?),
        subject_snapshot,
        sender_snapshot,
        result,
        error_message,
        ran_at: parse_dt_rs(&ran_at_str)?,
    })
}

pub fn list_workflows(
    conn: &ReadConn,
) -> Result<Vec<crate::domain::workflow::Workflow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, hotkey, steps_json, enabled,
                archive_after_success,
                created_at, run_count, last_run_at
           FROM workflows
          ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], map_workflow)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

pub fn get_workflow(
    conn: &ReadConn,
    id: &crate::domain::workflow::WorkflowId,
) -> Result<Option<crate::domain::workflow::Workflow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, hotkey, steps_json, enabled,
                archive_after_success,
                created_at, run_count, last_run_at
           FROM workflows
          WHERE id = ?1",
    )?;
    stmt.query_row(params![id.0.to_string()], map_workflow)
        .optional()
        .map_err(DbError::from)
}

fn map_workflow(
    row: &Row<'_>,
) -> rusqlite::Result<crate::domain::workflow::Workflow> {
    use crate::domain::workflow::{Step, Workflow, WorkflowId};
    let id_str: String = row.get(0)?;
    let hotkey: Option<String> = row.get(2)?;
    let steps_json: String = row.get(3)?;
    let archive_after_success_i: i64 = row.get(5)?;
    let created_str: String = row.get(6)?;
    let run_count_i: i64 = row.get(7)?;
    let last_run_str: Option<String> = row.get(8)?;

    let steps: Vec<Step> = serde_json::from_str(&steps_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    })?;

    Ok(Workflow {
        id: WorkflowId(parse_uuid_rs(&id_str)?),
        name: row.get(1)?,
        hotkey,
        steps,
        enabled: row.get::<_, i64>(4)? != 0,
        archive_after_success: archive_after_success_i != 0,
        created_at: parse_dt_rs(&created_str)?,
        run_count: run_count_i.max(0) as u64,
        last_run_at: last_run_str
            .map(|s| parse_dt_rs(&s))
            .transpose()?,
    })
}

pub fn list_workflow_rules(
    conn: &ReadConn,
) -> Result<Vec<crate::domain::workflow::WorkflowRule>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_id, account_id, folder_name, predicates_json, mode,
                enabled, created_at, hit_count, last_hit_at,
                name, action_type, action_dest, delay_minutes, dry_run
           FROM workflow_rules
          ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], map_workflow_rule)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

pub fn list_workflow_rules_for_workflow(
    conn: &ReadConn,
    workflow_id: &crate::domain::workflow::WorkflowId,
) -> Result<Vec<crate::domain::workflow::WorkflowRule>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_id, account_id, folder_name, predicates_json, mode,
                enabled, created_at, hit_count, last_hit_at,
                name, action_type, action_dest, delay_minutes, dry_run
           FROM workflow_rules
          WHERE workflow_id = ?1
          ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![workflow_id.0.to_string()], map_workflow_rule)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Enabled rules restricted to rules applicable to `account`. A NULL
/// `account_id` means "matches any account" — so an enabled global
/// rule and an enabled account-scoped rule both surface here. Used by
/// the matcher; bulk UI rendering goes through `list_workflow_rules`.
pub fn list_enabled_workflow_rules_for_account(
    conn: &ReadConn,
    account_id: &AccountId,
) -> Result<Vec<crate::domain::workflow::WorkflowRule>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_id, account_id, folder_name, predicates_json, mode,
                enabled, created_at, hit_count, last_hit_at,
                name, action_type, action_dest, delay_minutes, dry_run
           FROM workflow_rules
          WHERE enabled = 1
            AND (account_id IS NULL OR account_id = ?1)",
    )?;
    let rows = stmt.query_map(params![account_id.0.to_string()], map_workflow_rule)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

pub fn get_workflow_rule(
    conn: &ReadConn,
    id: &crate::domain::workflow::WorkflowRuleId,
) -> Result<Option<crate::domain::workflow::WorkflowRule>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, workflow_id, account_id, folder_name, predicates_json, mode,
                enabled, created_at, hit_count, last_hit_at,
                name, action_type, action_dest, delay_minutes, dry_run
           FROM workflow_rules
          WHERE id = ?1",
    )?;
    stmt.query_row(params![id.0.to_string()], map_workflow_rule)
        .optional()
        .map_err(DbError::from)
}

fn map_workflow_rule(
    row: &Row<'_>,
) -> rusqlite::Result<crate::domain::workflow::WorkflowRule> {
    use crate::domain::workflow::{
        RuleAction, RuleMode, RulePredicate, WorkflowId, WorkflowRule, WorkflowRuleId,
    };
    let id_str: String = row.get(0)?;
    let workflow_str: Option<String> = row.get(1)?;
    let account_opt: Option<String> = row.get(2)?;
    let folder_opt: Option<String> = row.get(3)?;
    let predicates_json: String = row.get(4)?;
    let mode_str: String = row.get(5)?;
    let created_str: String = row.get(7)?;
    let hit_count_i: i64 = row.get(8)?;
    let last_hit_str: Option<String> = row.get(9)?;
    let name: String = row.get(10)?;
    let action_str: String = row.get(11)?;
    let action_dest: Option<String> = row.get(12)?;
    let delay_minutes_i: i64 = row.get(13)?;
    let dry_run: i64 = row.get(14)?;

    let predicates: Vec<RulePredicate> =
        serde_json::from_str(&predicates_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?;

    let mode = match mode_str.as_str() {
        "auto" => RuleMode::Auto,
        "confirm" => RuleMode::Confirm,
        other => {
            return Err(rusqlite::Error::InvalidColumnType(
                5,
                format!("unknown rule mode: {other}"),
                rusqlite::types::Type::Text,
            ))
        }
    };

    let action = RuleAction::parse(&action_str).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(
            11,
            format!("unknown action_type: {action_str}"),
            rusqlite::types::Type::Text,
        )
    })?;

    Ok(WorkflowRule {
        id: WorkflowRuleId(parse_uuid_rs(&id_str)?),
        workflow_id: workflow_str
            .as_deref()
            .map(parse_uuid_rs)
            .transpose()?
            .map(WorkflowId),
        account_id: account_opt
            .map(|s| parse_uuid_rs(&s).map(AccountId))
            .transpose()?,
        folder_name: folder_opt.filter(|s| !s.is_empty()),
        predicates,
        mode,
        enabled: row.get::<_, i64>(6)? != 0,
        created_at: parse_dt_rs(&created_str)?,
        hit_count: hit_count_i.max(0) as u64,
        last_hit_at: last_hit_str.map(|s| parse_dt_rs(&s)).transpose()?,
        name,
        action,
        action_dest,
        delay_minutes: delay_minutes_i.max(0) as u32,
        dry_run: dry_run != 0,
    })
}

/// Per-candidate summary served to the Training dialog: just enough
/// to render one readable row without re-opening the message. The
/// heavier fields (full body, all headers) are loaded later by the
/// actual learner during feature collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTrainingCandidate {
    pub message_id: MessageId,
    pub subject: String,
    pub from_email: String,
    pub from_domain: String,
    pub folder_name: String,
    pub account_id: AccountId,
    pub added_at: chrono::DateTime<chrono::Utc>,
}

pub fn list_workflow_training_candidates(
    conn: &ReadConn,
) -> Result<Vec<WorkflowTrainingCandidate>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.subject, e.from_json, e.account_id, f.name, t.added_at
           FROM workflow_training_candidates t
           JOIN envelopes e ON e.id = t.envelope_id
           JOIN folders   f ON f.id = e.folder_id
          ORDER BY t.added_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let id_str: String = row.get(0)?;
        let subject: String = row.get(1)?;
        let from_json: String = row.get(2)?;
        let account_str: String = row.get(3)?;
        let folder_name: String = row.get(4)?;
        let added_str: String = row.get(5)?;

        let from_list: Vec<crate::domain::message::Address> =
            serde_json::from_str(&from_json).unwrap_or_default();
        let from_email = from_list
            .first()
            .map(|a| a.email.to_ascii_lowercase())
            .unwrap_or_default();
        let from_domain = from_email
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();

        Ok(WorkflowTrainingCandidate {
            message_id: MessageId(parse_uuid_rs(&id_str)?),
            subject,
            from_email,
            from_domain,
            folder_name,
            account_id: AccountId(parse_uuid_rs(&account_str)?),
            added_at: parse_dt_rs(&added_str)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Flat list of message IDs currently marked as workflow-training
/// candidates. Served to the frontend on each inbox refresh — the
/// list is tiny (tens of rows at most, typically single-digit) and
/// cheaper to pull as a Set than to JOIN into every envelope query.
pub fn list_workflow_training_ids(
    conn: &ReadConn,
) -> Result<Vec<MessageId>, DbError> {
    let mut stmt =
        conn.prepare("SELECT envelope_id FROM workflow_training_candidates")?;
    let rows = stmt.query_map([], |row| {
        let s: String = row.get(0)?;
        Ok(MessageId(parse_uuid_rs(&s)?))
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
}

/// Cheap existence probe used by the Reader to decide whether to
/// render the "marked for training" badge without fetching all rows.
pub fn is_workflow_training_candidate(
    conn: &ReadConn,
    id: &MessageId,
) -> Result<bool, DbError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM workflow_training_candidates WHERE envelope_id = ?1",
            params![id.0.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

/// Raw RFC822 bytes for the message, as originally downloaded. Used for
/// attachment extraction (where we re-parse on demand) and any future
/// forward-as-original feature.
pub fn get_body_raw(conn: &ReadConn, id: &MessageId) -> Result<Option<Vec<u8>>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT raw_rfc822 FROM bodies WHERE envelope_id = ?1",
    )?;
    let mut rows = stmt.query(params![id.0.to_string()])?;
    if let Some(row) = rows.next()? {
        let raw: Option<Vec<u8>> = row.get(0)?;
        Ok(raw)
    } else {
        Ok(None)
    }
}

// ─── row mappers ─────────────────────────────────────────────────────────────

fn map_envelope_summary(row: &Row<'_>) -> rusqlite::Result<EnvelopeSummary> {
    let id_str: String = row.get(0)?;
    let account_id_str: String = row.get(1)?;
    let folder_id_str: String = row.get(3)?;
    let from_json: String = row.get(5)?;
    let date_str: String = row.get(6)?;
    let seen: i64 = row.get(7)?;
    let answered: i64 = row.get(8)?;
    let flagged: i64 = row.get(9)?;
    let forwarded: i64 = row.get(10)?;
    let junk: i64 = row.get(11)?;
    let body_cached: i64 = row.get(12)?;
    let has_attachments: i64 = row.get(13)?;
    // Scheduled-Action-Snapshot. Sieben Spalten — wenn `scheduled_at`
    // NULL ist, gibt's keinen Tag. Defensiv: ungültige Action-Strings
    // (DB-Korruption oder Zukunfts-Migration ohne Code-Update) werden
    // als "kein Tag" behandelt — besser kein Marker als ein crashender
    // Mapper.
    let scheduled_at_str: Option<String> = row.get(14)?;
    let scheduled_action_str: Option<String> = row.get(15)?;
    let scheduled_action_dest: Option<String> = row.get(16)?;
    let scheduled_rule_id_str: Option<String> = row.get(17)?;
    let scheduled_rule_name: Option<String> = row.get(18)?;
    let scheduled_workflow_id_str: Option<String> = row.get(19)?;
    let scheduled_dry_run: i64 = row.get(20)?;

    let from_first = first_address_text(&from_json);

    let scheduled = match (scheduled_at_str, scheduled_action_str) {
        (Some(at), Some(action)) => {
            use crate::domain::workflow::{
                RuleAction, ScheduledActionTag, WorkflowId, WorkflowRuleId,
            };
            match RuleAction::parse(&action) {
                Some(action) => {
                    let rule_id = scheduled_rule_id_str
                        .as_deref()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .map(WorkflowRuleId);
                    let workflow_id = scheduled_workflow_id_str
                        .as_deref()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .map(WorkflowId);
                    Some(ScheduledActionTag {
                        scheduled_at: parse_dt_rs(&at)?,
                        action,
                        action_dest: scheduled_action_dest,
                        rule_id,
                        rule_name: scheduled_rule_name.unwrap_or_default(),
                        workflow_id,
                        dry_run: scheduled_dry_run != 0,
                    })
                }
                None => None,
            }
        }
        _ => None,
    };

    Ok(EnvelopeSummary {
        id: MessageId(parse_uuid_rs(&id_str)?),
        account_id: AccountId(parse_uuid_rs(&account_id_str)?),
        account_color: row.get(2)?,
        folder_id: FolderId(parse_uuid_rs(&folder_id_str)?),
        subject: row.get(4)?,
        from_first,
        date: parse_dt_rs(&date_str)?,
        seen: seen != 0,
        answered: answered != 0,
        flagged: flagged != 0,
        forwarded: forwarded != 0,
        junk: junk != 0,
        body_cached: body_cached != 0,
        has_attachments: has_attachments != 0,
        scheduled,
    })
}

fn first_address_text(json_str: &str) -> String {
    #[derive(Deserialize)]
    struct Addr {
        name: Option<String>,
        email: String,
    }
    let parsed: Vec<Addr> = serde_json::from_str(json_str).unwrap_or_default();
    parsed
        .into_iter()
        .next()
        .map(|a| a.name.filter(|n| !n.is_empty()).unwrap_or(a.email))
        .unwrap_or_default()
}

// DbError-flavored helpers for direct-query paths.
fn parse_uuid(s: &str) -> Result<Uuid, DbError> {
    Uuid::from_str(s).map_err(|_| {
        DbError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "uuid")),
        ))
    })
}

fn parse_dt(s: &str) -> Result<DateTime<Utc>, DbError> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| {
            DbError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "datetime",
                )),
            ))
        })
}

// rusqlite-flavored helpers for use inside query_map closures.
fn parse_uuid_rs(s: &str) -> rusqlite::Result<Uuid> {
    Uuid::from_str(s).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "uuid")),
        )
    })
}

fn parse_dt_rs(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "datetime",
                )),
            )
        })
}

// ─── Address-Book / Contacts ──────────────────────────────────────────

/// Compose-Autocomplete-Backend. Liefert Top-N Adress-Kandidaten für
/// einen Prefix, gerankt nach gewichteter Frequency × Recency:
///
/// - **Score** = `send_count * 3 + recv_count` — "ich habe an X
///   geschrieben" wiegt 3x mehr als "X erschien irgendwo in Cc",
///   weil das ein viel stärkeres Adressier-Signal ist.
/// - **Tiebreak** = `last_seen_at DESC` — bei gleichem Score gewinnt
///   die jüngste Aktivität.
/// - **Filter**: `is_role = 0` (no-reply / bounces / etc. raus).
///
/// `prefix` matcht case-insensitive gegen `email` UND `display_name`.
/// 2-Zeichen-Schwelle vom Caller ist üblich; die Query selbst akzeptiert
/// auch 1-Zeichen, ist aber dann teurer und liefert oft Müll.
///
/// Wenn ein Contact für die Adresse existiert, fließt sein
/// `display_name` in die Antwort ein (überschreibt history-Wert) —
/// die UI zeigt dann den kuratierten Namen statt was im letzten
/// Mail-Header stand.
pub fn list_address_completions(
    conn: &ReadConn,
    prefix: &str,
    limit: i64,
) -> Result<Vec<crate::domain::contact::AddressCompletion>, DbError> {
    let trimmed = prefix.trim().to_lowercase();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let like_pattern = format!("%{}%", trimmed.replace('%', "\\%").replace('_', "\\_"));

    let mut stmt = conn.prepare(
        "SELECT
            ah.email,
            ah.display_name,
            c.id              AS contact_id,
            c.display_name    AS contact_display_name,
            ah.send_count,
            ah.recv_count,
            ah.last_seen_at
         FROM address_history ah
         LEFT JOIN contact_emails ce ON ce.email = ah.email
         LEFT JOIN contacts       c  ON c.id = ce.contact_id
         WHERE ah.is_role = 0
           AND (lower(ah.email)        LIKE ?1 ESCAPE '\\'
             OR lower(ah.display_name) LIKE ?1 ESCAPE '\\'
             OR lower(c.display_name)  LIKE ?1 ESCAPE '\\')
         ORDER BY (ah.send_count * 3 + ah.recv_count) DESC,
                  ah.last_seen_at DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![like_pattern, limit], |row| {
        let contact_id_str: Option<String> = row.get("contact_id")?;
        let contact_id = contact_id_str
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok())
            .map(crate::domain::contact::ContactId);
        Ok(crate::domain::contact::AddressCompletion {
            email: row.get("email")?,
            display_name: row.get("display_name")?,
            contact_id,
            contact_display_name: row.get("contact_display_name")?,
            send_count: row.get("send_count")?,
            recv_count: row.get("recv_count")?,
            last_seen_at: parse_dt_rs(&row.get::<_, String>("last_seen_at")?)?,
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Liste aller Contacts mit Stats. Optional FTS-gefiltert via `query`
/// (matcht display_name + organization + job_title + phone + city +
/// notes). `limit`/`offset` für Pagination — Default 200/0 bei
/// Frontend-Aufruf.
///
/// Sort-Reihenfolge: pinned-first, dann nach `last_message_at DESC`
/// (häufige aktuelle Korrespondenzen oben), Tiebreak alphabetisch.
pub fn list_contacts(
    conn: &ReadConn,
    query: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::domain::contact::ContactSummary>, DbError> {
    let trimmed = query.map(|s| s.trim()).filter(|s| !s.is_empty());

    // Wenn ein Such-Query gegeben ist, filtern wir via FTS5; sonst
    // Full-Scan (Tabellen sind klein, kein Bottleneck).
    // Match-Regel für message_count + last_message_at: from-Treffer
    // immer, to-Treffer nur in outgoing Envelopes, cc gar nicht. Cc'd
    // Mails würden den Count irreführend hochziehen — der User würde
    // 50 zählen, in der Detail-Liste aber nur 5 sehen.
    let sent_folder_check =
        " AND EXISTS (
              SELECT 1 FROM folders f
              JOIN accounts a2 ON a2.id = f.account_id
              WHERE f.id = e.folder_id
                AND f.name = a2.sent_folder
                AND a2.sent_folder <> ''
         )";
    let envelope_join = format!(
        "LEFT JOIN envelopes e ON e.deleted = 0 AND (
                e.from_json LIKE '%\"' || ce.email || '\"%'
             OR (e.to_json LIKE '%\"' || ce.email || '\"%'{sent_folder_check})
         )"
    );
    let sql = if trimmed.is_some() {
        format!(
            "SELECT
                c.id,
                c.display_name,
                c.organization,
                c.city,
                c.pinned,
                (SELECT email FROM contact_emails WHERE contact_id = c.id AND is_primary = 1 LIMIT 1) AS primary_email,
                COUNT(DISTINCT e.id) AS message_count,
                MAX(e.date_utc) AS last_message_at
             FROM contacts c
             JOIN fts_contacts fc ON fc.contact_id = c.id
             LEFT JOIN contact_emails ce ON ce.contact_id = c.id
             {envelope_join}
             WHERE fc.fts_contacts MATCH ?1
             GROUP BY c.id
             ORDER BY c.pinned DESC,
                      COALESCE(MAX(e.date_utc), c.created_at) DESC,
                      c.display_name COLLATE NOCASE
             LIMIT ?2 OFFSET ?3"
        )
    } else {
        format!(
            "SELECT
                c.id,
                c.display_name,
                c.organization,
                c.city,
                c.pinned,
                (SELECT email FROM contact_emails WHERE contact_id = c.id AND is_primary = 1 LIMIT 1) AS primary_email,
                COUNT(DISTINCT e.id) AS message_count,
                MAX(e.date_utc) AS last_message_at
             FROM contacts c
             LEFT JOIN contact_emails ce ON ce.contact_id = c.id
             {envelope_join}
             GROUP BY c.id
             ORDER BY c.pinned DESC,
                      COALESCE(MAX(e.date_utc), c.created_at) DESC,
                      c.display_name COLLATE NOCASE
             LIMIT ?1 OFFSET ?2"
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let map_row = |row: &Row<'_>| -> rusqlite::Result<crate::domain::contact::ContactSummary> {
        let last_at_str: Option<String> = row.get("last_message_at")?;
        let last_message_at = match last_at_str {
            Some(s) if !s.is_empty() => Some(parse_dt_rs(&s)?),
            _ => None,
        };
        let id_str: String = row.get("id")?;
        Ok(crate::domain::contact::ContactSummary {
            id: crate::domain::contact::ContactId(parse_uuid_rs(&id_str)?),
            display_name: row.get("display_name")?,
            organization: row.get("organization")?,
            city: row.get("city")?,
            primary_email: row.get("primary_email")?,
            pinned: row.get::<_, i64>("pinned")? != 0,
            message_count: row.get("message_count")?,
            last_message_at,
        })
    };

    let rows = if let Some(q) = trimmed {
        // FTS5-Query escapen — User-Input darf keine FTS-Operatoren
        // als syntaktisch wirksam haben (sonst Fehler bei Anführungs-
        // zeichen etc.). Quoten als Phrase ist die einfachste robuste
        // Variante: matcht "Müller GmbH" als zusammenhängend.
        let fts_q = format!("\"{}\"", q.replace('"', "\"\""));
        let iter = stmt.query_map(params![fts_q, limit, offset], map_row)?;
        iter.collect::<Result<Vec<_>, _>>()?
    } else {
        let iter = stmt.query_map(params![limit, offset], map_row)?;
        iter.collect::<Result<Vec<_>, _>>()?
    };

    Ok(rows)
}

/// Voller Detail-Datensatz für einen Contact, inkl. seiner
/// E-Mail-Adressen + Mail-Stats.
pub fn get_contact(
    conn: &ReadConn,
    id: &crate::domain::contact::ContactId,
) -> Result<Option<crate::domain::contact::ContactDetail>, DbError> {
    let id_str = id.0.to_string();
    let contact = conn
        .query_row(
            "SELECT id, display_name, organization, job_title, phone, mobile,
                    street, zip, city, country, website, notes, origin, pinned,
                    last_extracted_envelope_id, created_at, updated_at
             FROM contacts WHERE id = ?1",
            params![id_str],
            map_contact,
        )
        .optional()?;
    let Some(contact) = contact else { return Ok(None) };

    let mut stmt = conn.prepare(
        "SELECT id, contact_id, email, is_primary
         FROM contact_emails
         WHERE contact_id = ?1
         ORDER BY is_primary DESC, id ASC",
    )?;
    let emails: Vec<crate::domain::contact::ContactEmail> = stmt
        .query_map(params![id_str], map_contact_email)?
        .collect::<Result<Vec<_>, _>>()?;

    // Tags des Contacts: JOIN über contact_tags, sortiert nach Name
    // (case-insensitive) damit die UI-Reihenfolge stabil bleibt.
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color, t.created_at
         FROM tags t
         JOIN contact_tags ct ON ct.tag_id = t.id
         WHERE ct.contact_id = ?1
         ORDER BY t.name COLLATE NOCASE",
    )?;
    let tags: Vec<crate::domain::contact::Tag> = stmt
        .query_map(params![id_str], map_tag)?
        .collect::<Result<Vec<_>, _>>()?;

    // Mail-Stats: gleiche Filter-Regel wie list_messages_for_contact —
    // From-Treffer + outgoing-To-Treffer, kein Cc. Sonst zeigt das UI
    // einen Count der größer ist als die sichtbare Liste.
    //
    // Die Single-Pass-Variante via UNION ALL der vorherigen Implementierung
    // ergab bei Mails wo Contact gleichzeitig From UND To war doppelt-
    // gezählte Counts — wir pinnen das mit einem outer DISTINCT-SELECT
    // jetzt fest.
    let (message_count, last_message_at): (i64, Option<DateTime<Utc>>) = {
        if emails.is_empty() {
            (0, None)
        } else {
            let placeholders = (0..emails.len())
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT COUNT(DISTINCT e.id), MAX(e.date_utc)
                 FROM envelopes e
                 WHERE e.deleted = 0
                   AND (
                        EXISTS (SELECT 1 FROM json_each(e.from_json) addr
                                WHERE lower(json_extract(addr.value, '$.email')) IN ({ph}))
                     OR (
                          EXISTS (SELECT 1 FROM json_each(e.to_json) addr
                                  WHERE lower(json_extract(addr.value, '$.email')) IN ({ph}))
                          AND EXISTS (
                               SELECT 1 FROM folders f
                               JOIN accounts ac ON ac.id = f.account_id
                               WHERE f.id = e.folder_id
                                 AND f.name = ac.sent_folder
                                 AND ac.sent_folder <> ''
                          )
                        )
                   )",
                ph = placeholders
            );
            let mut stmt = conn.prepare(&sql)?;
            // 2x das Email-Set (from-pass + to-pass).
            let mut params_vec: Vec<Box<dyn ToSql>> = Vec::new();
            for _ in 0..2 {
                for em in &emails {
                    params_vec.push(Box::new(em.email.clone()));
                }
            }
            let params_refs: Vec<&dyn ToSql> =
                params_vec.iter().map(|p| p.as_ref()).collect();
            let row = stmt.query_row(params_refs.as_slice(), |row| {
                let cnt: i64 = row.get(0)?;
                let max: Option<String> = row.get(1)?;
                Ok((cnt, max))
            })?;
            let max_dt = match row.1 {
                Some(s) if !s.is_empty() => Some(parse_dt(&s)?),
                _ => None,
            };
            (row.0, max_dt)
        }
    };

    Ok(Some(crate::domain::contact::ContactDetail {
        contact,
        emails,
        tags,
        message_count,
        last_message_at,
    }))
}

/// Liste aller Tags, alphabetisch sortiert. Für die Tag-Auswahl im
/// Contact-Editor + Settings-Tag-Mgmt-Panel.
pub fn list_tags(conn: &ReadConn) -> Result<Vec<crate::domain::contact::Tag>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, created_at
         FROM tags
         ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], map_tag)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn map_tag(row: &Row<'_>) -> rusqlite::Result<crate::domain::contact::Tag> {
    let id_str: String = row.get(0)?;
    Ok(crate::domain::contact::Tag {
        id: crate::domain::contact::TagId(parse_uuid_rs(&id_str)?),
        name: row.get(1)?,
        color: row.get(2)?,
        created_at: parse_dt_rs(&row.get::<_, String>(3)?)?,
    })
}

/// Reader-Header-Lookup: gibt's einen Contact für diese Adresse oder
/// nur einen History-Eintrag oder ist die Adresse komplett unbekannt?
/// Drives das Person-Icon im Mail-Header (3 Visual-Zustände).
pub fn contact_lookup_for_email(
    conn: &ReadConn,
    email: &str,
) -> Result<crate::domain::contact::ContactLookup, DbError> {
    let normalized = email.trim().to_lowercase();
    if normalized.is_empty() {
        return Ok(crate::domain::contact::ContactLookup::Unknown);
    }

    // Erst: gibt's einen Contact für diese Adresse?
    let contact = conn
        .query_row(
            "SELECT c.id, c.display_name, c.organization, c.job_title,
                    c.phone, c.mobile, c.street, c.zip, c.city, c.country,
                    c.website, c.notes, c.origin, c.pinned,
                    c.last_extracted_envelope_id, c.created_at, c.updated_at
             FROM contacts c
             JOIN contact_emails ce ON ce.contact_id = c.id
             WHERE ce.email = ?1
             LIMIT 1",
            params![normalized],
            map_contact,
        )
        .optional()?;
    if let Some(c) = contact {
        return Ok(crate::domain::contact::ContactLookup::Contact { contact: c });
    }

    // Sonst: gibt's einen History-Eintrag?
    let hist = conn
        .query_row(
            "SELECT display_name, send_count, recv_count
             FROM address_history WHERE email = ?1",
            params![normalized],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((display_name, send_count, recv_count)) = hist {
        return Ok(crate::domain::contact::ContactLookup::HistoryOnly {
            display_name,
            send_count,
            recv_count,
        });
    }

    Ok(crate::domain::contact::ContactLookup::Unknown)
}

/// Liste aller Mails an/von einem Contact (über alle ihm zugeordneten
/// Adressen). Liefert Frontend-Standard-EnvelopeSummary, sortiert nach
/// Datum DESC.
pub fn list_messages_for_contact(
    conn: &ReadConn,
    contact_id: &crate::domain::contact::ContactId,
    limit: i64,
    offset: i64,
) -> Result<Vec<EnvelopeSummary>, DbError> {
    // Erst: Adressen des Contacts holen.
    let mut stmt = conn.prepare(
        "SELECT email FROM contact_emails WHERE contact_id = ?1",
    )?;
    let emails: Vec<String> = stmt
        .query_map(params![contact_id.0.to_string()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if emails.is_empty() {
        return Ok(Vec::new());
    }

    // Match-Regel:
    //   * From-Treffer immer (eingehende Mail vom Kontakt) ODER
    //   * To-Treffer NUR wenn die Envelope im sent_folder liegt
    //     (= ich habe aktiv an die Person geschickt).
    // CC bewusst NICHT — wenn jemand mich auf einen Verteiler zu der
    // Person packt, war das nicht "ich habe der Person geschrieben".
    let placeholders = (0..emails.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    // Spalten-Set zentral aus `ENVELOPE_SUMMARY_COLS` ziehen — sonst
    // läuft der `map_envelope_summary`-Mapper bei den neuen Lifetime/
    // Scheduled-Action-Spalten in einen Index-Mismatch ("Invalid column
    // index: 14"). DISTINCT bleibt nötig wegen der EXISTS-Pfade auf
    // from/to.
    let sql = format!(
        "SELECT DISTINCT {ENVELOPE_SUMMARY_COLS}
         FROM envelopes e
         JOIN accounts a ON a.id = e.account_id
         WHERE e.deleted = 0
           AND (
                EXISTS (SELECT 1 FROM json_each(e.from_json) addr
                        WHERE lower(json_extract(addr.value, '$.email')) IN ({ph}))
             OR (
                  EXISTS (SELECT 1 FROM json_each(e.to_json) addr
                          WHERE lower(json_extract(addr.value, '$.email')) IN ({ph}))
                  AND EXISTS (
                       SELECT 1 FROM folders f
                       JOIN accounts ac ON ac.id = f.account_id
                       WHERE f.id = e.folder_id
                         AND f.name = ac.sent_folder
                         AND ac.sent_folder <> ''
                  )
                )
           )
         ORDER BY e.date_utc DESC
         LIMIT ? OFFSET ?",
        ph = placeholders
    );
    let mut stmt = conn.prepare(&sql)?;

    // 2x das Email-Set (from + to) + limit + offset. Cc-Pass ist weg.
    let mut params_vec: Vec<Box<dyn ToSql>> = Vec::new();
    for _ in 0..2 {
        for em in &emails {
            params_vec.push(Box::new(em.clone()));
        }
    }
    params_vec.push(Box::new(limit));
    params_vec.push(Box::new(offset));
    let params_refs: Vec<&dyn ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(params_refs.as_slice(), map_envelope_summary)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Liefert die jüngste envelope_id für eine Liste von Adressen. Vom
/// Auto-Extraction-Code genutzt um zu entscheiden ob ein vorhandener
/// `extraction_misses`-Eintrag stale ist (neuere Mail seit letztem
/// Versuch → nochmal probieren).
#[allow(dead_code)]
pub fn latest_envelope_id_for_email(
    conn: &ReadConn,
    email: &str,
) -> Result<Option<MessageId>, DbError> {
    let normalized = email.trim().to_lowercase();
    let sql = "SELECT e.id
               FROM envelopes e, json_each(e.from_json) addr
               WHERE lower(json_extract(addr.value, '$.email')) = ?1
               ORDER BY e.date_utc DESC
               LIMIT 1";
    let id_str: Option<String> = conn
        .query_row(sql, params![normalized], |r| r.get(0))
        .optional()?;
    Ok(id_str.and_then(|s| Uuid::parse_str(&s).ok()).map(MessageId))
}

/// Helper: parsing einer Contact-Zeile aus rusqlite. Spalten in
/// derselben Reihenfolge wie im SELECT oben.
fn map_contact(row: &Row<'_>) -> rusqlite::Result<crate::domain::contact::Contact> {
    let id_str: String = row.get(0)?;
    Ok(crate::domain::contact::Contact {
        id: crate::domain::contact::ContactId(parse_uuid_rs(&id_str)?),
        display_name: row.get(1)?,
        organization: row.get(2)?,
        job_title: row.get(3)?,
        phone: row.get(4)?,
        mobile: row.get(5)?,
        street: row.get(6)?,
        zip: row.get(7)?,
        city: row.get(8)?,
        country: row.get(9)?,
        website: row.get(10)?,
        notes: row.get(11)?,
        origin: crate::domain::contact::ContactOrigin::from_db_str(
            &row.get::<_, String>(12)?,
        ),
        pinned: row.get::<_, i64>(13)? != 0,
        last_extracted_envelope_id: row.get(14)?,
        created_at: parse_dt_rs(&row.get::<_, String>(15)?)?,
        updated_at: parse_dt_rs(&row.get::<_, String>(16)?)?,
    })
}

fn map_contact_email(row: &Row<'_>) -> rusqlite::Result<crate::domain::contact::ContactEmail> {
    let cid_str: String = row.get(1)?;
    Ok(crate::domain::contact::ContactEmail {
        id: row.get(0)?,
        contact_id: crate::domain::contact::ContactId(parse_uuid_rs(&cid_str)?),
        email: row.get(2)?,
        is_primary: row.get::<_, i64>(3)? != 0,
    })
}

