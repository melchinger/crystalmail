// Rule-Scheduler: zwei Verantwortlichkeiten, beide um workflow_rules
// aufgehängt:
//
//   1. Sync-Time Fast-Path — beim Empfang einer neuen Mail (in
//      `drive_uid_fetch`) werden Rules mit envelope-auflösbaren
//      Predicates direkt geprüft. Trifft eine Direktaktion-Rule
//      (Archive/Delete/Move) zu UND `delay_minutes = 0`, läuft die Aktion
//      sofort. Bei `delay_minutes > 0` wird der Envelope getaggt; der
//      Sweeper räumt später ab. RunWorkflow-Rules werden hier *nicht*
//      gefeuert — die brauchen den Body und übernimmt der bestehende
//      `workflow_rules::evaluate_and_trigger`-Matcher beim Body-Store.
//
//   2. Sweeper — periodisch (nach jedem Sync) eingesammelte
//      Envelope-Tags abarbeiten: action durchführen, audit-loggen,
//      Skip-Bedingungen respektieren (flagged/answered/Folder-Mismatch).

use chrono::{DateTime, Duration, Utc};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::domain::message::Envelope;
use crate::domain::workflow::{
    RuleAction, RuleActionLogEntry, RuleActionResult, RulePredicate, ScheduledActionTag,
    WorkflowRule, WorkflowRuleId,
};
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::queries::{self, ScheduledEnvelopeRow};

/// Maximale Anzahl Mails pro Sweep-Run. Großzügig — typisch laufen pro
/// Tick eh nur eine Handvoll, aber nach langem App-Standby oder einem
/// Backfill kann's auch dreistellig werden. 500 ist immer noch
/// überschaubar pro Run.
const SWEEP_BATCH: u32 = 500;

// ─── Sync-Time Fast-Path ────────────────────────────────────────────────

/// Predicate-Set einer Rule lässt sich allein aus dem Envelope (ohne
/// Body) auflösen? `HasAttachmentExtension` braucht den Body — die
/// Rule wartet dann auf den Body-Store-Matcher. Alle anderen Predicate-
/// Typen (FromEmail/FromDomain/FromDomainIn/SubjectContains) sind sync-fähig.
fn predicates_envelope_resolvable(predicates: &[RulePredicate]) -> bool {
    predicates.iter().all(|p| {
        !matches!(p, RulePredicate::HasAttachmentExtension { .. })
    })
}

/// Match-Features aus einem (frisch geparsten) Envelope. Schmaler als
/// `workflow_rules::MatchContext` — wir haben hier keinen Body, also
/// auch keine Attachments. Folder-Name kommt vom Caller.
struct EnvelopeFeatures<'a> {
    from_email: String,
    from_domain: String,
    subject: &'a str,
    folder_name: &'a str,
}

impl<'a> EnvelopeFeatures<'a> {
    fn from_envelope(envelope: &'a Envelope, folder_name: &'a str) -> Self {
        let from_email = envelope
            .from
            .first()
            .map(|a| a.email.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let from_domain = from_email
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();
        Self {
            from_email,
            from_domain,
            subject: &envelope.subject,
            folder_name,
        }
    }
}

fn predicate_matches_envelope(p: &RulePredicate, f: &EnvelopeFeatures<'_>) -> bool {
    match p {
        RulePredicate::FromEmail { value } => {
            f.from_email.eq_ignore_ascii_case(value.trim())
        }
        RulePredicate::FromDomain { value } => {
            f.from_domain.eq_ignore_ascii_case(value.trim())
        }
        RulePredicate::FromDomainIn { values } => values.iter().any(|v| {
            !v.trim().is_empty() && f.from_domain.eq_ignore_ascii_case(v.trim())
        }),
        RulePredicate::SubjectContains { value } => {
            let needle = value.trim().to_ascii_lowercase();
            !needle.is_empty() && f.subject.to_ascii_lowercase().contains(&needle)
        }
        // Body-abhängig — kann am Sync-Pfad nie matchen, der Body-Store-
        // Matcher übernimmt.
        RulePredicate::HasAttachmentExtension { .. } => false,
    }
}

fn rule_matches_envelope(rule: &WorkflowRule, f: &EnvelopeFeatures<'_>) -> bool {
    if !rule.enabled || rule.predicates.is_empty() {
        return false;
    }
    if let Some(ref wanted) = rule.folder_name {
        if !wanted.is_empty() && wanted != f.folder_name {
            return false;
        }
    }
    if !predicates_envelope_resolvable(&rule.predicates) {
        // Mindestens ein Predicate braucht den Body — nicht hier matchen.
        return false;
    }
    rule.predicates
        .iter()
        .all(|p| predicate_matches_envelope(p, f))
}

/// Erste matchende Rule gewinnt. Liefert ein vorbereitetes
/// `ScheduledActionTag` zurück — egal ob `delay_minutes = 0` (sofort
/// auszuführen) oder >0 (zu speichern). Bei sofort+RunWorkflow gibt's
/// `None`, weil wir den Workflow erst beim Body-Store anstoßen.
pub fn match_at_sync_time(
    envelope: &Envelope,
    folder_name: &str,
    rules: &[WorkflowRule],
) -> Option<ScheduledActionTag> {
    if rules.is_empty() {
        return None;
    }
    let features = EnvelopeFeatures::from_envelope(envelope, folder_name);
    for rule in rules {
        if !rule_matches_envelope(rule, &features) {
            continue;
        }
        // RunWorkflow + delay=0 ist Body-Store-Matcher-Sache — hier
        // schweigen, der Body kommt eh später durch den Prefetch oder
        // beim manuellen Öffnen, dann triggert der andere Matcher.
        if rule.action == RuleAction::RunWorkflow && rule.delay_minutes == 0 {
            continue;
        }
        let scheduled_at = envelope.date + Duration::minutes(rule.delay_minutes as i64);
        return Some(ScheduledActionTag {
            scheduled_at,
            action: rule.action,
            action_dest: rule.action_dest.clone(),
            rule_id: Some(rule.id),
            rule_name: rule.name.clone(),
            workflow_id: rule.workflow_id,
            dry_run: rule.dry_run,
        });
    }
    None
}

/// Idempotenter Tag-Setter: schickt das Tag in den Writer, ignoriert
/// Fehler (geloggt). Caller hat die `message_id` schon nach erfolgreichem
/// Envelope-Upsert in der Hand.
pub async fn tag_after_upsert(
    db: &DbHandle,
    message_id: crate::domain::message::MessageId,
    tag: ScheduledActionTag,
) {
    let (tx, rx) = oneshot::channel();
    if db
        .writer
        .send(WriteCmd::TagEnvelopeScheduled {
            message_id,
            tag,
            ack: tx,
        })
        .await
        .is_err()
    {
        tracing::warn!("rule_scheduler: writer channel closed");
        return;
    }
    let _ = rx.await;
}

// ─── Sweeper ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct SweepCounts {
    pub ok: u32,
    pub skipped: u32,
    pub failed: u32,
}

/// Skip-Logik. Reasoning siehe Doku-Kommentare an den Match-Punkten.
fn should_skip(row: &ScheduledEnvelopeRow) -> Option<&'static str> {
    if row.dry_run {
        return Some("dry_run");
    }
    if row.flagged {
        return Some("flagged");
    }
    if row.answered {
        return Some("answered");
    }
    // Mail liegt nicht mehr im Inbox — User hat sie manuell verschoben.
    // Action wäre redundant.
    if !row.folder_name.eq_ignore_ascii_case("INBOX") {
        return Some("not_in_inbox");
    }
    None
}

pub async fn sweep_once(
    app: &tauri::AppHandle,
    db: &DbHandle,
) -> SweepCounts {
    let now = Utc::now();
    let candidates = match {
        let conn = match db.reads.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "rule sweep: db read failed");
                return SweepCounts { ok: 0, skipped: 0, failed: 0 };
            }
        };
        queries::list_due_scheduled_envelopes(&conn, &now, SWEEP_BATCH)
    } {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!(error = %e, "rule sweep: list_due failed");
            return SweepCounts { ok: 0, skipped: 0, failed: 0 };
        }
    };

    if candidates.is_empty() {
        return SweepCounts { ok: 0, skipped: 0, failed: 0 };
    }

    tracing::info!(
        candidates = candidates.len(),
        "rule sweep: working through scheduled actions"
    );

    let mut counts = SweepCounts { ok: 0, skipped: 0, failed: 0 };

    for row in candidates {
        let (result, error_message) = if let Some(_reason) = should_skip(&row) {
            counts.skipped += 1;
            (RuleActionResult::Skipped, None)
        } else {
            match dispatch_action(app, db, &row).await {
                Ok(()) => {
                    counts.ok += 1;
                    (RuleActionResult::Ok, None)
                }
                Err(e) => {
                    counts.failed += 1;
                    tracing::warn!(
                        message_id = %row.id.0,
                        action = row.action.as_str(),
                        error = %e,
                        "rule sweep: dispatch failed"
                    );
                    (RuleActionResult::Failed, Some(e))
                }
            }
        };

        let entry = RuleActionLogEntry {
            id: Uuid::new_v4(),
            rule_id: row.rule_id,
            rule_name: row.rule_name.clone(),
            action: row.action,
            action_dest: row.action_dest.clone(),
            workflow_id: row.workflow_id,
            message_id: row.id,
            subject_snapshot: row.subject.clone(),
            sender_snapshot: row.from_first.clone(),
            result,
            error_message,
            ran_at: now,
        };
        let (tx, rx) = oneshot::channel();
        if db
            .writer
            .send(WriteCmd::InsertRuleActionLog { entry, ack: tx })
            .await
            .is_err()
        {
            tracing::warn!("rule sweep: writer channel closed");
            break;
        }
        let _ = rx.await;
    }

    tracing::info!(
        ok = counts.ok,
        skipped = counts.skipped,
        failed = counts.failed,
        "rule sweep: done"
    );
    counts
}

async fn dispatch_action(
    app: &tauri::AppHandle,
    db: &DbHandle,
    row: &ScheduledEnvelopeRow,
) -> Result<(), String> {
    match row.action {
        RuleAction::Archive => {
            crate::application::message_ops::archive(db, row.id).await
        }
        RuleAction::Delete => {
            crate::application::message_ops::delete(db, row.id).await
        }
        RuleAction::Move => {
            let dest = row
                .action_dest
                .clone()
                .ok_or_else(|| "Aktion 'move' ohne Zielordner".to_string())?;
            crate::application::message_ops::move_to(db, row.id, dest).await
        }
        RuleAction::RunWorkflow => {
            let workflow_id = row
                .workflow_id
                .ok_or_else(|| "RunWorkflow ohne workflow_id".to_string())?;
            // Sweeper-Pfad: keine UI-Interaktion möglich. Prompt-
            // Params brauchen einen `defaultTemplate`-Fallback;
            // Required-Prompts ohne Default lassen den Workflow mit
            // beschreibender Fehlermeldung im Audit-Log auflaufen.
            crate::application::workflows::apply_with_lifecycle(
                app,
                db,
                workflow_id,
                row.id,
                std::collections::HashMap::new(),
            )
            .await
            .map(|_| ())
        }
    }
}

/// Helper für die `apply_to_existing`-Backfill-Funktionalität — wird
/// vom Settings-UI nach dem Anlegen einer neuen Rule aufgerufen.
/// Liefert die Anzahl der getaggten Mails. Direkt-Ausführung der Action
/// (statt nur Tagging) gibt's hier nicht — das macht der Sweeper, weil
/// alle Skip-Bedingungen (flagged/answered) schon dort sauber abgebildet
/// sind. User klickt nach dem Backfill auf "Sweep jetzt", wenn er das
/// Ergebnis nicht abwarten will.
pub async fn apply_to_existing(
    db: &DbHandle,
    rule: &WorkflowRule,
) -> Result<u32, String> {
    let envelopes = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_unified_folder(&conn, "inbox", rule.account_id.as_ref(), 5000, 0)
            .map_err(|e| e.to_string())?
    };

    let mut affected = 0u32;
    for env in envelopes {
        // EnvelopeSummary → minimale Features. Folder ist immer "INBOX"
        // weil wir aus dem unified_inbox-View kommen — der Wert wird vom
        // folder-name-Filter der Rule trotzdem wieder geprüft, falls die
        // Rule bewusst nicht-INBOX scoped wurde (selten, aber legal).
        let from_email = env.from_first.to_ascii_lowercase();
        // Quick parse "Name <email>" — wir brauchen nur die @-Domain.
        let from_email = match from_email.find('<') {
            Some(open) => match from_email[open..].find('>') {
                Some(close) => from_email[open + 1..open + close].to_string(),
                None => from_email,
            },
            None => from_email,
        };
        let from_domain = from_email
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();
        let features = EnvelopeFeatures {
            from_email,
            from_domain,
            subject: &env.subject,
            folder_name: "INBOX",
        };
        if !rule_matches_envelope(rule, &features) {
            continue;
        }

        affected += 1;
        let scheduled_at = env.date + Duration::minutes(rule.delay_minutes as i64);
        let tag = ScheduledActionTag {
            scheduled_at,
            action: rule.action,
            action_dest: rule.action_dest.clone(),
            rule_id: Some(rule.id),
            rule_name: rule.name.clone(),
            workflow_id: rule.workflow_id,
            dry_run: rule.dry_run,
        };
        let (tx, rx) = oneshot::channel();
        let _ = db
            .writer
            .send(WriteCmd::TagEnvelopeScheduled {
                message_id: env.id,
                tag,
                ack: tx,
            })
            .await;
        let _ = rx.await;
    }

    if affected > 0 {
        let (tx, rx) = oneshot::channel();
        let _ = db
            .writer
            .send(WriteCmd::IncrementWorkflowRuleHit {
                rule_id: rule.id,
                ack: tx,
            })
            .await;
        let _ = rx.await;
    }

    Ok(affected)
}

#[allow(dead_code)]
pub fn debug_format_tag(tag: &ScheduledActionTag, now: DateTime<Utc>) -> String {
    let remaining = tag.scheduled_at - now;
    let days = remaining.num_days();
    format!(
        "{} in {}d ({})",
        tag.action.as_str(),
        days.max(0),
        if tag.dry_run { "dry_run" } else { "live" }
    )
}

// `WorkflowRuleId` is referenced only in the public surface above. The
// import stays valid even if unused — keep `dead_code` hush off.
#[allow(dead_code)]
fn _refer_rule_id(_id: WorkflowRuleId) {}
