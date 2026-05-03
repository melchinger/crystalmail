// Auto-trigger matcher for workflow rules.
//
// Fires at one point in the pipeline: right after a message body has
// been stored in the DB (see the call sites in `prefetch::*` and
// `body::*`). At that moment every predicate we support is resolvable
// — envelope metadata has been in the DB since the sync pass, and the
// attachments can be inspected by re-parsing the raw RFC822 we just
// wrote.
//
// Behaviour per rule:
//
//   * `auto` — spawn `apply_workflow` in the background. Counted as a
//     rule hit on success; failure is logged but doesn't surface a
//     UI prompt (the user didn't initiate this).
//   * `confirm` — emit a `workflow-rule-match` Tauri event with the
//     workflow name and matched-envelope metadata. Frontend renders a
//     toast; applying happens via the normal `apply_workflow` command,
//     so the rule hit is counted when the user clicks "Anwenden".
//
// The matcher is deliberately non-fatal: any internal error logs at
// `warn` and returns. Matcher breakage must never gate mail sync or
// body download.

use chrono::Duration;
use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::domain::message::MessageId;
use crate::domain::workflow::{
    RuleAction, RuleMode, RulePredicate, ScheduledActionTag, WorkflowId, WorkflowRule,
    WorkflowRuleId,
};
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::queries::{self, EnvelopeDetail};

/// Fields derived from one message, used to evaluate every enabled
/// rule against it. Built once per matcher call, shared across rules.
pub struct MatchContext {
    pub from_email: String,
    pub from_domain: String,
    pub subject: String,
    /// IMAP folder the envelope currently lives in. Used to honour
    /// `rule.folder_name` scope. Stored verbatim — rule folder names
    /// match case-sensitively the same way the IMAP spec treats them.
    pub folder_name: String,
    /// Extensions of non-inline attachments, lower-cased, no leading
    /// dot, in parse order. Compound suffixes appear here intact
    /// (`tar.gz`) because `attachment_name()` returns the full name
    /// and we take whatever follows the first dot after the last
    /// slash-free segment.
    pub attachment_extensions: Vec<String>,
}

impl MatchContext {
    /// Build from the stored envelope + the freshly-written raw body.
    /// The body is the trust-source for attachments — we don't rely
    /// on any pre-parsed attachment list since the matcher runs
    /// right after `store_body`, when that list is guaranteed fresh.
    pub fn from_envelope_and_raw(
        envelope: &EnvelopeDetail,
        raw: Option<&[u8]>,
    ) -> Self {
        let from_email = envelope
            .from
            .first()
            .map(|a| a.email.to_ascii_lowercase())
            .unwrap_or_default();
        let from_domain = from_email
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();
        let subject = envelope.subject.clone();

        let mut exts: Vec<String> = Vec::new();
        if let Some(bytes) = raw {
            if let Some(msg) = MessageParser::default().parse(bytes) {
                for part in msg.attachments() {
                    if matches!(part.body, PartType::InlineBinary(_)) {
                        continue;
                    }
                    let Some(name) = part.attachment_name() else {
                        continue;
                    };
                    if let Some(ext) = extract_extensions(name) {
                        exts.push(ext);
                    }
                }
            }
        }

        Self {
            from_email,
            from_domain,
            subject,
            folder_name: envelope.folder_name.clone(),
            attachment_extensions: exts,
        }
    }
}

/// Extract the extension chain from a filename. Greedy at the right
/// side: `report.2024.tar.gz` yields `tar.gz` because the matcher
/// rule can say `has_attachment_extension: tar.gz`. Callers that
/// want just `gz` still get a hit via the suffix comparison inside
/// `predicate_matches`.
fn extract_extensions(filename: &str) -> Option<String> {
    let n = filename.to_ascii_lowercase();
    // Find last slash-free part.
    let last = n.rsplit(['/', '\\']).next().unwrap_or(&n);
    let first_dot = last.find('.')?;
    // Skip a leading dot ("hidden file with no extension") — first
    // dot at position 0 means there is no filename stem, so no ext.
    if first_dot == 0 {
        return None;
    }
    let ext = &last[first_dot + 1..];
    if ext.is_empty() { None } else { Some(ext.to_string()) }
}

pub fn predicate_matches(p: &RulePredicate, ctx: &MatchContext) -> bool {
    match p {
        RulePredicate::FromEmail { value } => {
            ctx.from_email.eq_ignore_ascii_case(value.trim())
        }
        RulePredicate::FromDomain { value } => {
            ctx.from_domain.eq_ignore_ascii_case(value.trim())
        }
        RulePredicate::FromDomainIn { values } => values
            .iter()
            .any(|v| !v.trim().is_empty() && ctx.from_domain.eq_ignore_ascii_case(v.trim())),
        RulePredicate::SubjectContains { value } => {
            let needle = value.trim().to_ascii_lowercase();
            if needle.is_empty() {
                return false;
            }
            ctx.subject.to_ascii_lowercase().contains(&needle)
        }
        RulePredicate::HasAttachmentExtension { extension } => {
            let want = extension
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase();
            if want.is_empty() {
                return false;
            }
            // Support compound suffixes: for each attachment's full
            // extension chain (e.g. `tar.gz`), a rule looking for
            // `gz` still wins via suffix match; `tar.gz` also wins.
            let wanted_suffix = format!(".{want}");
            ctx.attachment_extensions.iter().any(|ae| {
                let dotted = format!(".{ae}");
                dotted.ends_with(&wanted_suffix)
            })
        }
    }
}

/// A rule matches iff it's enabled, non-empty, folder-scoped
/// correctly, and *every* predicate matches. Empty predicate list =
/// no match (a rule that fires on every mail would be a foot-gun,
/// not a feature). Folder scope is case-sensitive literal match,
/// same as IMAP.
pub fn rule_matches(rule: &WorkflowRule, ctx: &MatchContext) -> bool {
    if !rule.enabled || rule.predicates.is_empty() {
        return false;
    }
    if let Some(ref wanted) = rule.folder_name {
        // Empty string acts like None (shouldn't happen — the query
        // layer filters those out — but cheap to guard against).
        if !wanted.is_empty() && wanted != &ctx.folder_name {
            return false;
        }
    }
    rule.predicates
        .iter()
        .all(|p| predicate_matches(p, ctx))
}

/// Payload for the `workflow-rule-match` Tauri event. Slim enough
/// for a toast row without a second invoke round-trip: workflow
/// name, from-address, subject, rule id (for bookkeeping), message
/// id (for the applying click).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMatchEvent {
    pub rule_id: WorkflowRuleId,
    pub workflow_id: WorkflowId,
    pub workflow_name: String,
    pub message_id: MessageId,
    pub from_email: String,
    pub subject: String,
}

/// Main entry point: runs every enabled rule against the message
/// identified by `message_id`, honours auto vs confirm, and logs
/// warnings on any internal failure. Never returns an error — the
/// caller pipes this as fire-and-forget.
pub async fn evaluate_and_trigger(
    app: AppHandle,
    db: DbHandle,
    message_id: MessageId,
) {
    if let Err(e) = evaluate_and_trigger_inner(&app, &db, message_id).await {
        tracing::warn!(
            message_id = %message_id.0,
            "workflow-rule matcher failed: {e}"
        );
    }
}

async fn evaluate_and_trigger_inner(
    app: &AppHandle,
    db: &DbHandle,
    message_id: MessageId,
) -> Result<(), String> {
    let (envelope, raw, rules) = load_match_inputs(db, &message_id)?;
    if rules.is_empty() {
        return Ok(());
    }

    let ctx = MatchContext::from_envelope_and_raw(&envelope, raw.as_deref());

    // De-dupe: when two rules with the same RunWorkflow target both
    // match, we still only fire the workflow once. Direkt-Action-Rules
    // (Archive/Delete/Move) sind ohnehin separat und kollidieren nicht
    // mit der Workflow-Set-Logik.
    let mut fired_workflows: std::collections::HashSet<WorkflowId> =
        std::collections::HashSet::new();

    for rule in rules.into_iter().filter(|r| rule_matches(r, &ctx)) {
        match rule.action {
            // Direkt-Aktionen (Archive/Delete/Move): immer den
            // ScheduledActionTag setzen — der Sweeper kümmert sich.
            // Bei delay_minutes = 0 ist `scheduled_at` = envelope.date,
            // also sofort fällig; der nächste Sweep-Tick (kommt
            // detached vom Sync, oder beim manuellen Trigger) führt
            // die Action durch. Ein redundanter Tag (Sync-Hook hat
            // schon getaggt) wird vom UPDATE einfach überschrieben —
            // gleicher Wert, kein Schaden.
            RuleAction::Archive | RuleAction::Delete | RuleAction::Move => {
                let scheduled_at =
                    envelope.date + Duration::minutes(rule.delay_minutes as i64);
                let tag = ScheduledActionTag {
                    scheduled_at,
                    action: rule.action,
                    action_dest: rule.action_dest.clone(),
                    rule_id: Some(rule.id),
                    rule_name: rule.name.clone(),
                    workflow_id: rule.workflow_id,
                    dry_run: rule.dry_run,
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = db
                    .writer
                    .send(WriteCmd::TagEnvelopeScheduled {
                        message_id,
                        tag,
                        ack: tx,
                    })
                    .await;
                let _ = rx.await;
            }
            RuleAction::RunWorkflow => {
                // RunWorkflow ohne workflow_id ist Schrott — Rule wurde
                // schief gespeichert. Loggen und weiter.
                let Some(workflow_id) = rule.workflow_id else {
                    tracing::warn!(
                        rule_id = %rule.id.0,
                        "matcher: RunWorkflow ohne workflow_id — skip"
                    );
                    continue;
                };
                if !fired_workflows.insert(workflow_id) {
                    continue;
                }
                // delay_minutes > 0: nicht direkt feuern, sondern als
                // ScheduledActionTag merken. Der Sweeper feuert den
                // Workflow zum richtigen Zeitpunkt.
                if rule.delay_minutes > 0 {
                    let scheduled_at =
                        envelope.date + Duration::minutes(rule.delay_minutes as i64);
                    let tag = ScheduledActionTag {
                        scheduled_at,
                        action: RuleAction::RunWorkflow,
                        action_dest: None,
                        rule_id: Some(rule.id),
                        rule_name: rule.name.clone(),
                        workflow_id: Some(workflow_id),
                        dry_run: rule.dry_run,
                    };
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = db
                        .writer
                        .send(WriteCmd::TagEnvelopeScheduled {
                            message_id,
                            tag,
                            ack: tx,
                        })
                        .await;
                    let _ = rx.await;
                    continue;
                }
                // delay_minutes == 0: alte Logik — sofort feuern (auto)
                // oder Toast (confirm).
                match rule.mode {
                    RuleMode::Auto => {
                        spawn_auto_apply(
                            app.clone(),
                            db.clone(),
                            rule.id,
                            workflow_id,
                            message_id,
                        );
                    }
                    RuleMode::Confirm => {
                        let conn = match db.reads.get() {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("matcher: read pool empty: {e}");
                                continue;
                            }
                        };
                        let wf = queries::get_workflow(&conn, &workflow_id)
                            .ok()
                            .flatten();
                        let Some(wf) = wf else {
                            continue;
                        };
                        drop(conn);

                        let payload = RuleMatchEvent {
                            rule_id: rule.id,
                            workflow_id,
                            workflow_name: wf.name,
                            message_id,
                            from_email: ctx.from_email.clone(),
                            subject: ctx.subject.clone(),
                        };
                        if let Err(e) = app.emit("workflow-rule-match", &payload) {
                            tracing::warn!("matcher: emit failed: {e}");
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn load_match_inputs(
    db: &DbHandle,
    message_id: &MessageId,
) -> Result<(EnvelopeDetail, Option<Vec<u8>>, Vec<WorkflowRule>), String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let envelope = queries::get_envelope(&conn, message_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "envelope gone".to_string())?;
    let raw = queries::get_body_raw(&conn, message_id)
        .map_err(|e| e.to_string())?;
    let rules = queries::list_enabled_workflow_rules_for_account(
        &conn,
        &envelope.account_id,
    )
    .map_err(|e| e.to_string())?;
    Ok((envelope, raw, rules))
}

fn spawn_auto_apply(
    app: AppHandle,
    db: DbHandle,
    rule_id: WorkflowRuleId,
    workflow_id: WorkflowId,
    message_id: MessageId,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            // Auto-Trigger-Pfad: kein User-Dialog möglich, Prompt-
            // Params müssen per `defaultTemplate` einen Fallback haben.
            // Sonst schlägt der Apply mit einer beschreibenden Fehler-
            // meldung auf — das landet im Workflow-Run-Log.
            super::workflows::apply_with_lifecycle(
                &app,
                &db,
                workflow_id,
                message_id,
                std::collections::HashMap::new(),
            )
            .await
        {
            tracing::warn!(
                message_id = %message_id.0,
                "auto workflow apply failed: {e}"
            );
            return;
        }
        // Hit counting only happens on a successful apply. We count
        // even partial successes (allOk = false inside the result) as
        // a rule hit because the rule correctly fired — the failure
        // is on the workflow steps, not on the matcher.
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = db
            .writer
            .send(WriteCmd::IncrementWorkflowRuleHit {
                rule_id,
                ack: tx,
            })
            .await;
        let _ = rx.await;
    });
}
