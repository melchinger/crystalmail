// Server-side search fallback — the hybrid leg of search. The local
// FTS index only covers what's cached (30-day sync window + whatever
// the backfill/prefetch workers have pulled since). For everything
// else we ask the IMAP server directly: `UID SEARCH` is indexed
// server-side and fast even over huge archives. Matching envelopes are
// fetched header-only through the same pipeline as sync (spam rules,
// workflow match, FTS upsert included), so every server hit also
// *enriches* the local index for the next query.
//
// Scope decisions (deliberately pragmatic):
//   * Free text maps to IMAP TEXT/SUBJECT/FROM/TO/BODY keys. FTS5-only
//     syntax (OR, NEAR, parentheses, prefix `*`) is dropped — fail
//     soft, like the frontend DSL parser.
//   * Multiple criteria AND together, matching both IMAP's and FTS5's
//     default semantics.
//   * Folder scope: an explicit canonical key or folder pin maps to
//     that mailbox; unscoped searches cover Archive + INBOX + Sent per
//     account (where old mail realistically lives).
//   * `has:attachments` and `is:spam/junk` have no useful IMAP SEARCH
//     equivalent here — they're applied client-side after the fetch,
//     because the envelopes are local by then.

use std::collections::HashSet;

use tauri::AppHandle;

use crate::domain::account::AccountId;
use crate::domain::folder::FolderId;
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::queries::{self, EnvelopeSummary, SearchFilters};

use super::sync;

/// Hard cap on envelopes fetched per folder per search. The frontend's
/// `limit` may be 500 for local browsing, but pulling hundreds of
/// headers per folder per search is not what we want on the wire.
const MAX_FETCH_PER_FOLDER: usize = 100;

/// IMAP month names are fixed English abbreviations (RFC 3501) —
/// chrono's `%b` is locale-dependent in principle, so we pin our own.
const IMAP_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

#[allow(clippy::too_many_arguments)]
pub async fn search(
    app: &AppHandle,
    db: &DbHandle,
    fts: &str,
    folder_key: Option<&str>,
    folder_id: Option<&FolderId>,
    account_filter: Option<&AccountId>,
    filters: &SearchFilters,
    limit: u32,
) -> Result<Vec<EnvelopeSummary>, String> {
    let criteria = build_imap_criteria(fts, filters);
    if criteria.is_empty() {
        // Nothing the server could search for (pure client-side filter
        // query like `has:attachments`) — the local leg already covers
        // that case.
        return Ok(Vec::new());
    }

    let accounts: Vec<AccountId> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        match account_filter {
            Some(id) => vec![*id],
            None => queries::list_accounts(&conn)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|a| a.id)
                .collect(),
        }
    };

    // One task per account — sessions are independent, and accounts are
    // few (single digits), so an unbounded JoinSet is fine here.
    let mut set = tokio::task::JoinSet::new();
    for account_id in accounts {
        let app2 = app.clone();
        let db2 = db.clone();
        let criteria2 = criteria.clone();
        let folder_key2 = folder_key.map(str::to_string);
        let folder_id2 = folder_id.copied();
        let filters2 = filters.clone();
        set.spawn(async move {
            search_account(
                &app2,
                &db2,
                &account_id,
                &criteria2,
                folder_key2.as_deref(),
                folder_id2.as_ref(),
                &filters2,
            )
            .await
        });
    }

    let mut all: Vec<EnvelopeSummary> = Vec::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(mut rows)) => all.append(&mut rows),
            // A flaky account must not sink the others — log and keep
            // whatever the healthy accounts delivered.
            Ok(Err(e)) => tracing::warn!(error = %e, "server search: account failed"),
            Err(e) => tracing::warn!(error = %e, "server search: task join failed"),
        }
    }

    // Dedupe by id (a pinned folder can overlap the canonical scope),
    // then newest-first like every other result list, capped at limit.
    let mut seen = HashSet::new();
    all.retain(|r| seen.insert(r.id));
    all.sort_by(|a, b| b.date.cmp(&a.date));
    all.truncate(limit as usize);
    Ok(all)
}

#[allow(clippy::too_many_arguments)]
async fn search_account(
    app: &AppHandle,
    db: &DbHandle,
    account_id: &AccountId,
    criteria: &str,
    folder_key: Option<&str>,
    folder_id: Option<&FolderId>,
    filters: &SearchFilters,
) -> Result<Vec<EnvelopeSummary>, String> {
    let targets: Vec<(FolderId, String)> =
        resolve_targets(db, account_id, folder_key, folder_id).await?;
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let (mut session, account_name) = sync::open_session_for_account(db, account_id).await?;
    let mut out = Vec::new();
    for (fid, fname) in targets {
        let result = search_folder(
            app,
            db,
            &mut session,
            account_id,
            &account_name,
            &fid,
            &fname,
            criteria,
            filters,
        )
        .await;
        match result {
            Ok(mut rows) => out.append(&mut rows),
            // Per-folder failures (e.g. server rejects CHARSET UTF-8)
            // are logged and skipped — one bad mailbox shouldn't kill
            // the rest of the account's hits.
            Err(e) => tracing::warn!(folder = %fname, error = %e, "server search: folder failed"),
        }
    }
    let _ = session.logout().await;
    Ok(out)
}

/// SELECT + UID SEARCH + bounded header fetch for one mailbox.
/// Returns the freshly persisted hits as summaries.
#[allow(clippy::too_many_arguments)]
async fn search_folder(
    app: &AppHandle,
    db: &DbHandle,
    session: &mut async_imap::Session<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    account_id: &AccountId,
    account_name: &str,
    folder_id: &FolderId,
    folder_name: &str,
    criteria: &str,
    filters: &SearchFilters,
) -> Result<Vec<EnvelopeSummary>, String> {
    session
        .select(folder_name)
        .await
        .map_err(|e| format!("SELECT {folder_name}: {e}"))?;
    let uids: HashSet<u32> = session
        .uid_search(criteria)
        .await
        .map_err(|e| format!("UID SEARCH ({folder_name}): {e}"))?;

    // Newest hits first — same user expectation as the local search.
    let mut sorted: Vec<u32> = uids.into_iter().collect();
    sorted.sort_unstable();
    let tail = if sorted.len() > MAX_FETCH_PER_FOLDER {
        sorted.split_off(sorted.len() - MAX_FETCH_PER_FOLDER)
    } else {
        sorted
    };
    if tail.is_empty() {
        return Ok(Vec::new());
    }

    // Reuse the sync pipeline: parse headers, run spam/workflow rules,
    // upsert envelope + FTS row (body_text-preserving since the P0 fix).
    sync::drive_uid_fetch(
        app,
        db,
        session,
        account_id,
        account_name,
        *folder_id,
        folder_name,
        &tail,
    )
    .await?;

    let rows = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::summaries_by_folder_uids(&conn, folder_id, &tail)
            .map_err(|e| e.to_string())?
    };

    // Client-side filters IMAP SEARCH can't express for us.
    let rows = rows
        .into_iter()
        .filter(|r| filters.has_attachments != Some(true) || r.has_attachments)
        .filter(|r| filters.junk.map_or(true, |j| r.junk == j))
        .collect();
    Ok(rows)
}

/// Map the search scope to concrete (FolderId, IMAP name) mailboxes.
/// Folder rows that don't exist locally yet (never synced) are created
/// via `WriteCmd::EnsureFolder` so the fetch pipeline has a folder_id
/// to persist against.
async fn resolve_targets(
    db: &DbHandle,
    account_id: &AccountId,
    folder_key: Option<&str>,
    folder_id: Option<&FolderId>,
) -> Result<Vec<(FolderId, String)>, String> {
    // Explicit pin wins over everything.
    if let Some(fid) = folder_id {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let meta = queries::get_folder_meta(&conn, fid)
            .map_err(|e| e.to_string())?
            .ok_or("folder not found")?;
        return Ok(vec![(*fid, meta.name)]);
    }

    let names: Vec<String> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let account = queries::get_account(&conn, account_id)
            .map_err(|e| e.to_string())?
            .ok_or("account not found")?;
        match folder_key {
            // Starred is a flag view spanning mailboxes — there's no
            // single mailbox to scope the server search to. The local
            // leg handles it; the server leg stays out.
            Some("starred") => return Ok(Vec::new()),
            Some("inbox") => vec!["INBOX".to_string()],
            Some("archive") => vec![account.archive_folder.clone()],
            Some("sent") => vec![account.sent_folder.clone()],
            Some("drafts") => vec![account.drafts_folder.clone()],
            Some("trash") => vec![account.trash_folder.clone()],
            Some("spam") => vec![account.spam_folder.clone()],
            Some(other) => return Err(format!("unknown folder key: {other}")),
            // Unscoped: where old mail realistically lives.
            None => vec![
                account.archive_folder.clone(),
                "INBOX".to_string(),
                account.sent_folder.clone(),
            ],
        }
    };

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for name in names {
        let name = name.trim().to_string();
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        let fid = ensure_folder_id(db, account_id, &name).await?;
        out.push((fid, name));
    }
    Ok(out)
}

/// Folder id for (account, IMAP name), creating the row on first use.
async fn ensure_folder_id(
    db: &DbHandle,
    account_id: &AccountId,
    name: &str,
) -> Result<FolderId, String> {
    {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        if let Some(fid) = queries::find_folder_id(&conn, account_id, name)
            .map_err(|e| e.to_string())?
        {
            return Ok(fid);
        }
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    db.writer
        .send(WriteCmd::EnsureFolder {
            account_id: *account_id,
            name: name.to_string(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())
}

// ─── FTS5 → IMAP SEARCH translation ─────────────────────────────────

/// Translate the FTS5-flavoured query string into an IMAP SEARCH
/// criterion string (space-separated keys AND together). Returns ""
/// when nothing mappable remains — callers treat that as "no server
/// leg for this query".
fn build_imap_criteria(fts: &str, filters: &SearchFilters) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut needs_utf8 = false;

    for tok in tokenize(fts) {
        let (neg, body) = match tok.strip_prefix('-') {
            Some(rest) => ("NOT ", rest),
            None => ("", tok.as_str()),
        };
        if body.is_empty() {
            continue;
        }
        // Column selector? The frontend DSL emits `from_text:`,
        // `to_text:`, `subject:`, `body_text:` — map those; anything
        // else with a colon is FTS5-specific and gets dropped.
        let (key, value) = match split_selector(body) {
            Some((col, val)) => {
                let key = match col {
                    "from_text" => "FROM",
                    "to_text" => "TO",
                    "subject" => "SUBJECT",
                    "body_text" => "BODY",
                    _ => continue,
                };
                (key, val.to_string())
            }
            None => ("TEXT", unquote(body).to_string()),
        };
        // FTS5-only syntax we can't express: boolean operators, groups,
        // prefix wildcards. Drop the token rather than erroring.
        if value.is_empty()
            || matches!(value.as_str(), "OR" | "AND")
            || value.contains(['(', ')', '*'])
            || value.to_uppercase().starts_with("NEAR")
        {
            continue;
        }
        if !value.is_ascii() {
            needs_utf8 = true;
        }
        parts.push(format!("{neg}{key} {}", imap_quote(&value)));
    }

    if let Some(seen) = filters.seen {
        parts.push(if seen { "SEEN".into() } else { "UNSEEN".into() });
    }
    if let Some(flagged) = filters.flagged {
        parts.push(if flagged { "FLAGGED".into() } else { "UNFLAGGED".into() });
    }
    if let Some(answered) = filters.answered {
        parts.push(if answered { "ANSWERED".into() } else { "UNANSWERED".into() });
    }
    if let Some(since) = &filters.since {
        if let Some(d) = imap_date(since) {
            parts.push(format!("SENTSINCE {d}"));
        }
    }
    if let Some(before) = &filters.before {
        if let Some(d) = imap_date(before) {
            parts.push(format!("SENTBEFORE {d}"));
        }
    }

    if parts.is_empty() {
        return String::new();
    }
    // IMAP SEARCH defaults to US-ASCII; umlauts etc. need an explicit
    // charset declaration. Servers that don't support it will error —
    // the per-folder error handling above logs and skips.
    let joined = parts.join(" ");
    if needs_utf8 {
        format!("CHARSET UTF-8 {joined}")
    } else {
        joined
    }
}

/// Whitespace tokenizer that keeps `"…"` spans together (quotes
/// preserved in the token so `unquote` can find them).
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in input.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                current.push(ch);
            }
            c if c.is_whitespace() && !in_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Split `col:value` at the first colon outside quotes.
fn split_selector(tok: &str) -> Option<(&str, &str)> {
    let mut in_quote = false;
    for (idx, ch) in tok.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            ':' if !in_quote && idx > 0 => {
                return Some((&tok[..idx], unquote(&tok[idx + 1..])));
            }
            _ => {}
        }
    }
    None
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Quote a search value as an IMAP quoted string.
fn imap_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// RFC3339 → IMAP date (`1-Apr-2024`). Day has no leading zero per
/// RFC 3501 (`date-day = 1*2DIGIT` — both are accepted, but the
/// no-leading-zero form is the canonical one).
fn imap_date(rfc3339: &str) -> Option<String> {
    use chrono::Datelike;
    let dt = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    let month = IMAP_MONTHS[dt.month0() as usize];
    Some(format!("{}-{}-{}", dt.day(), month, dt.year()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_filters() -> SearchFilters {
        SearchFilters::default()
    }

    #[test]
    fn barewords_become_anded_text_keys() {
        let c = build_imap_criteria("Rechnung April", &no_filters());
        assert_eq!(c, "TEXT \"Rechnung\" TEXT \"April\"");
    }

    #[test]
    fn column_selectors_map_to_imap_keys() {
        let c = build_imap_criteria("from_text:melchinger subject:rechnung", &no_filters());
        assert_eq!(c, "FROM \"melchinger\" SUBJECT \"rechnung\"");
    }

    #[test]
    fn phrases_survive_tokenization() {
        let c = build_imap_criteria("\"Ihre Rechnung\" April", &no_filters());
        assert_eq!(c, "TEXT \"Ihre Rechnung\" TEXT \"April\"");
    }

    #[test]
    fn negation_maps_to_not() {
        let c = build_imap_criteria("rechnung -newsletter", &no_filters());
        assert_eq!(c, "TEXT \"rechnung\" NOT TEXT \"newsletter\"");
    }

    #[test]
    fn fts_only_syntax_is_dropped() {
        // OR / prefix-wildcard / NEAR have no IMAP equivalent — the
        // mappable remainder still produces a useful server query.
        let c = build_imap_criteria("rechnung OR mahnung rechnun*", &no_filters());
        assert_eq!(c, "TEXT \"rechnung\" TEXT \"mahnung\"");
    }

    #[test]
    fn dates_become_sent_keys() {
        let filters = SearchFilters {
            since: Some("2024-04-01T00:00:00+00:00".to_string()),
            before: Some("2024-05-01T00:00:00+00:00".to_string()),
            ..Default::default()
        };
        let c = build_imap_criteria("rechnung", &filters);
        assert_eq!(c, "TEXT \"rechnung\" SENTSINCE 1-Apr-2024 SENTBEFORE 1-May-2024");
    }

    #[test]
    fn non_ascii_triggers_charset_declaration() {
        let c = build_imap_criteria("größe", &no_filters());
        assert_eq!(c, "CHARSET UTF-8 TEXT \"größe\"");
    }

    #[test]
    fn empty_query_yields_empty_criteria() {
        assert_eq!(build_imap_criteria("", &no_filters()), "");
        assert_eq!(build_imap_criteria("OR", &no_filters()), "");
    }
}
