// Tauri commands for the spam-rule engine. CRUD on the rule table,
// preview (dry-run match against the current envelope set), and apply
// (go through matches, mark+move via the existing mark_as_spam path).
//
// No LLM in this module — pi lives one level up and feeds this via
// `RuleDraft` structures.

use chrono::Utc;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::application::message_ops;
use crate::application::spam_analysis::{self, CandidateFeatures};
use crate::application::spam_rules::{self, MatchFeatures, RuleDraft};
use crate::domain::message::MessageId;
use crate::domain::spam_rule::{SpamRule, SpamRuleId};
use crate::infrastructure::db::WriteCmd;
use crate::infrastructure::queries;
use crate::llm::pi_rpc::PiRpc;
use crate::state::{AppState, PiConfig};

/// Row-level info returned by `preview_rule` / `apply_rule`. Slim enough
/// to render a list in the dialog without a second invoke.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMatch {
    pub message_id: String,
    pub subject: String,
    pub from_email: String,
    pub folder_name: String,
    pub account_id: String,
}

#[tauri::command]
pub async fn list_spam_rules(app: AppHandle) -> Result<Vec<SpamRule>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_spam_rules(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_spam_rule(
    app: AppHandle,
    draft: RuleDraft,
) -> Result<SpamRule, String> {
    spam_rules::validate_pattern(draft.pattern_type, &draft.pattern)?;

    let rule = SpamRule {
        id: SpamRuleId(Uuid::new_v4()),
        account_id: draft.account_id,
        pattern_type: draft.pattern_type,
        pattern: draft.pattern.trim().to_string(),
        enabled: true,
        confidence: draft.confidence,
        reason: draft.reason,
        created_at: Utc::now(),
        hit_count: 0,
    };

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::InsertSpamRule {
            rule: rule.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db insert: {e}"))?;

    Ok(rule)
}

#[tauri::command]
pub async fn set_spam_rule_enabled(
    app: AppHandle,
    rule_id: SpamRuleId,
    enabled: bool,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::SetSpamRuleEnabled {
            rule_id,
            enabled,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update: {e}"))
}

#[tauri::command]
pub async fn delete_spam_rule(
    app: AppHandle,
    rule_id: SpamRuleId,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteSpamRule { rule_id, ack: tx })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete: {e}"))
}

/// Dry-run: show which envelopes the given draft would hit, without
/// making any changes. Iterates all non-spam/non-trash folders of the
/// relevant account (or all accounts if the rule is global). Uses the
/// cached `bodies.plain_text` when available; falls back to subject-only
/// matching when the body isn't cached yet.
#[tauri::command]
pub async fn preview_spam_rule(
    app: AppHandle,
    draft: RuleDraft,
) -> Result<Vec<RuleMatch>, String> {
    spam_rules::validate_pattern(draft.pattern_type, &draft.pattern)?;

    // Construct a temporary rule for the matcher — same shape, just no id.
    let temp_rule = SpamRule {
        id: SpamRuleId(Uuid::new_v4()),
        account_id: draft.account_id,
        pattern_type: draft.pattern_type,
        pattern: draft.pattern.trim().to_string(),
        enabled: true,
        confidence: draft.confidence,
        reason: draft.reason,
        created_at: Utc::now(),
        hit_count: 0,
    };

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    collect_matches(db, &temp_rule).await
}

/// Persist + apply in one go. Equivalent to `add_spam_rule` + (for every
/// matching envelope) `mark_as_spam`. Returns the list of affected rows
/// so the UI can summarise.
#[tauri::command]
pub async fn apply_spam_rule(
    app: AppHandle,
    draft: RuleDraft,
) -> Result<ApplyResult, String> {
    let rule = add_spam_rule(app.clone(), draft).await?;

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let matches = collect_matches(db, &rule).await?;

    // Iteriere und werfe jede Treffer-Mail in den Spam-Ordner. Drei
    // Dispositionen pro Match:
    //   * `Move(dest)` — Mail liegt aktuell nicht in Spam; wir flaggen
    //     sie als Junk und verschieben.
    //   * `AlreadyInSpam` — Regel erkennt sie korrekt, sie ist aber
    //     bereits dort, wo sie hingehört. Wir zählen sie als "match"
    //     (damit der User sieht, dass die Regel greift), aber ohne
    //     IMAP-Roundtrip.
    //   * `NoSpamFolder` — Account hat keinen Spam-Ordner konfiguriert.
    //     Auf der Server-Seite gibt's nichts zu tun; wir lassen die
    //     Mail komplett aus der Bilanz raus.
    //
    // Jeder `move_to`-Call öffnet seine eigene IMAP-Session — akzeptabel
    // für einen einmaligen Sweep, und robust wenn das Set mehrere
    // Konten/Ordner umspannt.
    enum Disposition {
        Move(String),
        AlreadyInSpam,
        NoSpamFolder,
    }

    let mut moved: Vec<RuleMatch> = Vec::new();
    let mut already_in_spam: usize = 0;
    for m in &matches {
        let msg_uuid = match Uuid::parse_str(&m.message_id) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let message_id = crate::domain::message::MessageId(msg_uuid);

        // Disposition zuerst — spart uns einen IMAP-Flag-Roundtrip pro
        // Mail die schon im Spam liegt. Bei einer Erst-Anwendung einer
        // Regel auf 26 schon-markierte Mails sind das 26 gesparte
        // Roundtrips und damit eine fast instantane UI-Antwort.
        let disposition = {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            let Some(envelope) = queries::get_envelope(&conn, &message_id)
                .map_err(|e| e.to_string())?
            else {
                continue;
            };
            let Some(account) = queries::get_account(&conn, &envelope.account_id)
                .map_err(|e| e.to_string())?
            else {
                continue;
            };
            if account.spam_folder.trim().is_empty() {
                Disposition::NoSpamFolder
            } else if envelope.folder_name == account.spam_folder {
                Disposition::AlreadyInSpam
            } else {
                Disposition::Move(account.spam_folder)
            }
        };

        match disposition {
            Disposition::AlreadyInSpam => {
                already_in_spam += 1;
                continue;
            }
            Disposition::NoSpamFolder => continue,
            Disposition::Move(dest) => {
                // Flag + move, gleiche Semantik wie die `!`-Hotkey-Aktion.
                if let Err(e) = crate::application::flags::apply(
                    db,
                    message_id,
                    crate::domain::message::FlagChanges {
                        junk: Some(true),
                        ..Default::default()
                    },
                )
                .await
                {
                    tracing::warn!(message_id = %m.message_id, "apply_rule: flag failed: {e}");
                    continue;
                }
                if let Err(e) = message_ops::move_to(db, message_id, dest).await {
                    tracing::warn!(message_id = %m.message_id, "apply_rule: move failed: {e}");
                    continue;
                }
                moved.push(m.clone());
            }
        }
    }

    // Bump the rule's hit counter by how many rows it actually moved.
    // Already-in-spam matches *don't* count here — the matching mails
    // were originally moved by mark_as_spam, which never bumps a rule
    // hit-count (no rule existed yet). Counting them now would inflate
    // the metric and make new rules look more effective than they are.
    if !moved.is_empty() {
        let (tx, rx) = oneshot::channel();
        let _ = db
            .writer
            .send(WriteCmd::IncrementSpamRuleHits {
                rule_id: rule.id,
                delta: moved.len() as i64,
                ack: tx,
            })
            .await;
        let _ = rx.await;
    }

    Ok(ApplyResult {
        rule,
        matched: matches.len(),
        moved: moved.len(),
        already_in_spam,
        moved_rows: moved,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub rule: SpamRule,
    /// Anzahl Mails, die das Regel-Pattern erkannt hat — egal ob danach
    /// verschoben oder schon im Spam-Ordner. Macht den User-sichtbaren
    /// Eindruck "die Regel greift" deckungsgleich mit der DB-Realität.
    pub matched: usize,
    /// Tatsächlich vom Inbox/Archive in den Spam-Ordner verschoben.
    pub moved: usize,
    /// Vom Pattern erkannt, aber bereits im Spam-Ordner — kein Move
    /// nötig. Macht "0 von 0 verschoben"-Verwirrung obsolet, indem
    /// das UI zeigt: "X erkannt, Y schon dort, Z verschoben".
    pub already_in_spam: usize,
    pub moved_rows: Vec<RuleMatch>,
}

async fn collect_matches(
    db: &crate::infrastructure::db::DbHandle,
    rule: &SpamRule,
) -> Result<Vec<RuleMatch>, String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    // Pull candidate envelopes — all non-deleted, across the relevant
    // accounts, skipping the trash (those mails are about to be expunged
    // anyway). The spam folder is *not* skipped here: we still want to
    // *count* mails the rule recognises that already live in spam, just
    // not move them again. The apply loop further down handles that
    // distinction via the `already_in_spam` tally.
    let account_clause = match rule.account_id {
        Some(_) => "AND e.account_id = ?1",
        None => "",
    };
    let sql = format!(
        "SELECT e.id, e.subject, e.from_json, f.name, e.account_id
           FROM envelopes e
           JOIN folders  f ON f.id = e.folder_id
           JOIN accounts a ON a.id = e.account_id
          WHERE e.deleted = 0
            AND f.name != a.trash_folder
            {account_clause}"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows: Vec<(String, String, String, String, String)> = if let Some(a) = rule.account_id {
        stmt.query_map(
            rusqlite::params![a.0.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
    } else {
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
    };

    // Pull cached body previews separately in a single batch so the main
    // query above doesn't pay the JOIN cost for rows that won't match.
    let mut body_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT envelope_id, plain_text FROM bodies WHERE plain_text IS NOT NULL")
            .map_err(|e| e.to_string())?;
        let iter = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in iter {
            let (id, plain) = r.map_err(|e| e.to_string())?;
            if let Some(s) = plain {
                body_map.insert(id, s);
            }
        }
    }

    // For HeaderContains rules we need raw_rfc822 (to extract the
    // header block). Lazy per-envelope so a huge account's body blobs
    // never all land in memory at once. For all other pattern types
    // this column stays untouched.
    let needs_headers =
        rule.pattern_type == crate::domain::spam_rule::SpamPatternType::HeaderContains;
    let mut header_stmt = if needs_headers {
        // SUBSTR keeps the IO bounded to the header region for most
        // mails — 16KB comfortably covers all headers we've seen in the
        // wild. Bigger mails just get truncated (unlikely to hurt since
        // spam indicators sit near the top).
        Some(
            conn.prepare(
                "SELECT SUBSTR(raw_rfc822, 1, 16384) FROM bodies WHERE envelope_id = ?1",
            )
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };

    // Pre-compile the regel **einmal** außerhalb des envelope-Loops.
    // Bei einem Konto mit ein paar tausend Mails und einer SubjectRegex
    // sparen wir damit ein paar tausend `Regex::new()`-Aufrufe.
    let compiled = spam_rules::Compiled::new(rule.clone());

    let mut out: Vec<RuleMatch> = Vec::new();
    for (id, subject, from_json, folder_name, account_id) in rows {
        let from_email = extract_first_email(&from_json).unwrap_or_default();
        let body = body_map.get(&id).map(String::as_str);

        let raw_bytes: Option<Vec<u8>> = if let Some(stmt) = header_stmt.as_mut() {
            stmt.query_row(rusqlite::params![&id], |row| {
                row.get::<_, Option<Vec<u8>>>(0)
            })
            .ok()
            .flatten()
        } else {
            None
        };

        let features = MatchFeatures::from_parts(
            &from_email,
            &subject,
            body,
            raw_bytes.as_deref(),
        );
        if spam_rules::matches_compiled(&features, &compiled) {
            out.push(RuleMatch {
                message_id: id,
                subject,
                from_email,
                folder_name,
                account_id,
            });
        }
    }

    Ok(out)
}

/// Full result of a single "Regel lernen"-Aktion. Returns the raw pi
/// response alongside the parsed drafts so the UI can show pi's wording
/// as rationale, and so a debug view can expose the raw JSON if parsing
/// produced fewer drafts than expected.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestResult {
    pub drafts: Vec<RuleDraft>,
    pub features: Vec<CandidateFeatures>,
    pub raw_response: String,
}

/// Call pi with the given spam candidates and return proposed rule drafts.
/// The drafts aren't persisted here — the user decides per-draft whether
/// to `apply_spam_rule` it.
#[tauri::command]
pub async fn suggest_spam_rules(
    app: AppHandle,
    message_ids: Vec<MessageId>,
) -> Result<SuggestResult, String> {
    if message_ids.is_empty() {
        return Err("Keine Kandidaten übergeben.".into());
    }
    let state = app.state::<AppState>();
    if !crate::commands::pi::ai_enabled(state.inner()) {
        return Err(crate::commands::pi::AI_DISABLED_ERR.to_string());
    }
    let db = state.db.get().ok_or("database not ready")?;

    let features = spam_analysis::collect_candidate_features(db, &message_ids).await?;
    if features.is_empty() {
        return Err("Zu den Message-IDs wurden keine Envelopes gefunden.".into());
    }

    let prompt = spam_analysis::build_prompt(&features);
    let raw_response = run_spam_pi_oneshot(&app, state.inner(), prompt).await?;

    let drafts = spam_analysis::parse_pi_response(&raw_response).unwrap_or_else(|e| {
        tracing::warn!("pi response parse failed: {e}");
        Vec::new()
    });

    Ok(SuggestResult {
        drafts,
        features,
        raw_response,
    })
}

/// Maximum time we let pi chew on a single spam-analysis prompt. Local
/// gemma 4B on CPU has been observed to sit in thinking/tool-retry loops
/// for minutes; 90s is a generous cap that still fails the UI cleanly.
const SPAM_PI_TIMEOUT_SECS: u64 = 90;

/// Run pi as a **one-shot** subprocess dedicated to spam analysis. Why
/// separate from the main chat pi:
///
///   * Tools are disabled — we ship the entire context inside the
///     prompt, pi has nothing to read/grep/find that would actually
///     help. With tools on, gemma happily enters retry loops probing
///     the filesystem for "more context" and never emits `agent_end`.
///   * Thinking is forced off — spam pattern matching is not reasoning
///     work, it's tokenization. thinking=medium/high on a small local
///     model burns minutes on no added quality.
///   * Process is dropped immediately after the turn — `kill_on_drop`
///     on the `Command` gives us a guaranteed clean shutdown.
///
/// When cloud-pi-for-spam lands (commit 3), this function gets a
/// per-call provider/model override. For now it inherits bin_path +
/// provider + model from the main PiConfig.
async fn run_spam_pi_oneshot(
    app: &AppHandle,
    state: &AppState,
    prompt: String,
) -> Result<String, String> {
    use std::sync::atomic::Ordering;

    let base_cfg = {
        let guard = state.pi_config.lock().unwrap();
        guard.clone()
    };
    // Per-field override for spam: if the user picked a dedicated
    // provider/model for analysis in PiSettings, use it here; otherwise
    // fall through to whatever the main chat pi is configured with.
    let provider = if base_cfg.spam_provider.trim().is_empty() {
        base_cfg.provider.clone()
    } else {
        base_cfg.spam_provider.clone()
    };
    let model = if base_cfg.spam_model.trim().is_empty() {
        base_cfg.model.clone()
    } else {
        base_cfg.spam_model.clone()
    };
    let cfg = PiConfig {
        provider,
        model,
        tools: String::new(),
        thinking: "off".into(),
        show_thinking: false,
        // Fresh session file so the one-shot isn't polluted by (or
        // pollutes) the user's chat history.
        session_file: "pi_spam_session.jsonl".into(),
        prompt_prefix: String::new(),
        // Inherit the rest (bin_path, session_dir, extra_args) verbatim.
        ..base_cfg
    };

    // Reset the cancel latch — a previous aborted run mustn't poison
    // the fresh one.
    state.spam_cancel_requested.store(false, Ordering::Relaxed);

    let rpc = PiRpc::spawn(app.clone(), &cfg).await?;
    // Publish the handle so `cancel_spam_analysis` can reach it. We
    // clone the Arc instead of moving so this function keeps the local
    // reference needed for `prompt_collect_json` below.
    *state.active_spam_pi.lock().await = Some(rpc.clone());

    // `prompt_collect_json` returns as soon as the response contains a
    // top-level JSON object with a `rules` array — it doesn't wait for
    // pi's own `agent_end`. gemma in agent mode regularly regenerates
    // the answer 2–3× before declaring itself done; we don't care, the
    // first pass is the one that matters.
    //
    // Timeout fallback: if the detector didn't find a matching object
    // within our budget, we *don't* throw the partial response away.
    // Returning whatever pi already wrote gives the parser a second
    // chance (maybe it's a slightly off-schema structure we can still
    // salvage) and gives the user the raw text to read. Dropping `rpc`
    // right after kills the lingering subprocess via `kill_on_drop`.
    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(SPAM_PI_TIMEOUT_SECS),
        rpc.prompt_collect_json(prompt),
    )
    .await;

    let response_or_partial: Result<String, String> = match timeout_result {
        Ok(result) => result,
        Err(_) => {
            let partial = rpc.collected_snapshot().await;
            if partial.trim().is_empty() {
                Err(format!(
                    "pi-Analyse dauerte länger als {SPAM_PI_TIMEOUT_SECS}s \
                     ohne eine einzige Token-Ausgabe. Tipps: kleineres Modell, \
                     Provider auf Cloud umstellen."
                ))
            } else {
                tracing::warn!(
                    "pi-Analyse hit timeout with partial buffer ({} bytes) — \
                     returning partial for parsing.",
                    partial.len()
                );
                Ok(partial)
            }
        }
    };

    // Clear the published handle regardless of how we exit.
    *state.active_spam_pi.lock().await = None;

    if state.spam_cancel_requested.load(Ordering::Relaxed) {
        return Err(CANCELLED_BY_USER.into());
    }

    response_or_partial.map_err(|e| format!("pi-Aufruf fehlgeschlagen: {e}"))
}

/// Stable sentinel — surfaced to the frontend so the dialog can
/// distinguish "user clicked cancel" from "something else went wrong".
pub const CANCELLED_BY_USER: &str = "cancelled_by_user";

/// Abort an in-flight spam-analysis call. Sets the cancel latch and
/// kills the pi child process; the blocked `suggest_spam_rules` call
/// then unwinds with `CANCELLED_BY_USER`. Idempotent — calling this
/// when no analysis is running is a no-op.
#[tauri::command]
pub async fn cancel_spam_analysis(app: AppHandle) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let state = app.state::<AppState>();
    state.spam_cancel_requested.store(true, Ordering::Relaxed);
    let guard = state.active_spam_pi.lock().await;
    if let Some(rpc) = guard.as_ref() {
        rpc.kill().await;
    }
    Ok(())
}

fn extract_first_email(json_str: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Addr {
        email: String,
    }
    let parsed: Vec<Addr> = serde_json::from_str(json_str).ok()?;
    parsed.into_iter().next().map(|a| a.email)
}
