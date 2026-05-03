// Tauri commands for the workflow engine. CRUD on the workflows table,
// config get/set (script dir + interpreter), and `apply_workflow` —
// the entry point the frontend calls after the user picks a workflow
// for a focused message.
//
// The config is persisted to `workflow_config.json` under the app data
// dir, same pattern as `pi_config.json`. Unreadable / missing file =
// fall back to defaults, which is non-functional for `RunScript` until
// the user opens Settings and configures a script directory.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::application::{rule_scheduler, workflow_analyzer, workflow_training, workflows};
use crate::domain::message::MessageId;
use crate::domain::workflow::{
    RuleAction, RuleActionLogEntry, RulePredicate, ScriptParam, Workflow, WorkflowDraft,
    WorkflowId, WorkflowRule, WorkflowRuleDraft, WorkflowRuleId,
};
use crate::infrastructure::db::WriteCmd;
use crate::infrastructure::queries;
use crate::llm::pi_rpc::PiRpc;
use crate::state::{AppState, PiConfig, WorkflowConfig};

const WORKFLOW_CONFIG_FILE: &str = "workflow_config.json";

fn config_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join(WORKFLOW_CONFIG_FILE))
}

pub fn load_persisted(app: &AppHandle) -> Option<WorkflowConfig> {
    let path = config_path(app)?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<WorkflowConfig>(&bytes).ok()
}

fn save_persisted(app: &AppHandle, cfg: &WorkflowConfig) -> Result<(), String> {
    let path = config_path(app).ok_or_else(|| "app_data_dir nicht verfügbar".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write: {e}"))
}

#[tauri::command]
pub async fn get_workflow_config(app: AppHandle) -> WorkflowConfig {
    let state = app.state::<AppState>();
    let guard = state.workflow_config.lock().unwrap();
    guard.clone()
}

#[tauri::command]
pub async fn set_workflow_config(
    app: AppHandle,
    config: WorkflowConfig,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    {
        let mut guard = state.workflow_config.lock().unwrap();
        *guard = config.clone();
    }
    if let Err(e) = save_persisted(&app, &config) {
        tracing::warn!(error = %e, "persisting workflow_config failed");
    }
    Ok(())
}

#[tauri::command]
pub async fn list_workflows(app: AppHandle) -> Result<Vec<Workflow>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_workflows(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_workflow(
    app: AppHandle,
    draft: WorkflowDraft,
) -> Result<Workflow, String> {
    validate_draft(&draft)?;
    let workflow = Workflow {
        id: WorkflowId(Uuid::new_v4()),
        name: draft.name.trim().to_string(),
        hotkey: draft.hotkey.filter(|s| !s.trim().is_empty()),
        steps: draft.steps,
        enabled: draft.enabled,
        archive_after_success: draft.archive_after_success,
        created_at: Utc::now(),
        run_count: 0,
        last_run_at: None,
    };

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::InsertWorkflow {
            workflow: workflow.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db insert: {e}"))?;
    Ok(workflow)
}

#[tauri::command]
pub async fn update_workflow(
    app: AppHandle,
    workflow_id: WorkflowId,
    draft: WorkflowDraft,
) -> Result<Workflow, String> {
    validate_draft(&draft)?;
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    // Pull the existing row so we keep the server-owned stats
    // (run_count / last_run_at) across the edit. Missing row ⇒ the
    // frontend is trying to save a workflow that was deleted in
    // another window; fail loudly so the UI refetches.
    let existing = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_workflow(&conn, &workflow_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Workflow existiert nicht mehr.".to_string())?
    };
    let workflow = Workflow {
        id: existing.id,
        name: draft.name.trim().to_string(),
        hotkey: draft.hotkey.filter(|s| !s.trim().is_empty()),
        steps: draft.steps,
        enabled: draft.enabled,
        archive_after_success: draft.archive_after_success,
        created_at: existing.created_at,
        run_count: existing.run_count,
        last_run_at: existing.last_run_at,
    };

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpdateWorkflow {
            workflow: workflow.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update: {e}"))?;
    Ok(workflow)
}

#[tauri::command]
pub async fn delete_workflow(
    app: AppHandle,
    workflow_id: WorkflowId,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteWorkflow {
            workflow_id,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete: {e}"))
}

/// `prompt_values` ist optional — Workflows ohne Prompt-Params können
/// `null` schicken. Tauri-Argumente erlauben kein `#[serde(default)]`
/// inline, deshalb hier als `Option<…>` statt Default-Map.
#[tauri::command]
pub async fn apply_workflow(
    app: AppHandle,
    workflow_id: WorkflowId,
    message_id: MessageId,
    prompt_values: Option<std::collections::HashMap<String, String>>,
) -> Result<workflows::WorkflowRunResult, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    // Lifecycle wrapper handles workflow load, apply, optional
    // archive-on-success, and run bookkeeping. Same code path as the
    // auto-trigger in `workflow_rules::spawn_auto_apply`.
    workflows::apply_with_lifecycle(
        &app,
        db,
        workflow_id,
        message_id,
        prompt_values.unwrap_or_default(),
    )
    .await
}

fn validate_draft(d: &WorkflowDraft) -> Result<(), String> {
    if d.name.trim().is_empty() {
        return Err("Workflow-Name fehlt.".into());
    }
    if d.steps.is_empty() {
        return Err("Workflow braucht mindestens einen Schritt.".into());
    }
    Ok(())
}

/// List `.py` filenames directly below the configured
/// `WorkflowConfig::script_dir`. Subdirectories are ignored in Stage 1
/// (same scope as the executor's allow-list). Returns an empty list
/// when the directory isn't configured yet, so the frontend can render
/// a "configure script directory first" state without a special error.
#[tauri::command]
pub async fn list_workflow_scripts(app: AppHandle) -> Result<Vec<String>, String> {
    let state = app.state::<AppState>();
    let dir = {
        let guard = state.workflow_config.lock().unwrap();
        guard.script_dir.clone()
    };
    if dir.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out: Vec<String> = Vec::new();
    let read_dir = std::fs::read_dir(&dir)
        .map_err(|e| format!("Script-Verzeichnis nicht lesbar ({dir}): {e}"))?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.to_ascii_lowercase().ends_with(".py") {
            out.push(name.to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// Analyse a Python script for its argparse parameter surface. The
/// `script_name` argument is a *filename* (same shape the executor
/// accepts) — path components are rejected here as well, so the
/// analyzer can't be tricked into reading outside the configured dir.
#[tauri::command]
pub async fn analyze_python_script(
    app: AppHandle,
    script_name: String,
) -> Result<Vec<ScriptParam>, String> {
    let state = app.state::<AppState>();
    let dir = {
        let guard = state.workflow_config.lock().unwrap();
        guard.script_dir.clone()
    };
    if dir.trim().is_empty() {
        return Err(
            "Kein Workflow-Script-Verzeichnis konfiguriert (Einstellungen → Workflows)."
                .into(),
        );
    }
    if script_name.contains('/')
        || script_name.contains('\\')
        || Path::new(&script_name).is_absolute()
    {
        return Err(format!(
            "Script-Name darf keine Pfadbestandteile enthalten: {script_name}"
        ));
    }

    let root = PathBuf::from(&dir);
    let full = root.join(&script_name);

    // Canonicalise both sides so symlink escapes don't smuggle an
    // outside-of-root script into the analyzer.
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("Script-Verzeichnis nicht lesbar ({}): {e}", root.display()))?;
    let canon_full = full
        .canonicalize()
        .map_err(|e| format!("Script nicht gefunden ({}): {e}", full.display()))?;
    if !canon_full.starts_with(&canon_root) {
        return Err("Script liegt außerhalb des erlaubten Verzeichnisses.".into());
    }

    workflow_analyzer::analyze(&canon_full)
}

// ─── workflow rules ──────────────────────────────────────────────────

#[tauri::command]
pub async fn list_workflow_rules(app: AppHandle) -> Result<Vec<WorkflowRule>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_workflow_rules(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_workflow_rules_for_workflow(
    app: AppHandle,
    workflow_id: WorkflowId,
) -> Result<Vec<WorkflowRule>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_workflow_rules_for_workflow(&conn, &workflow_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_workflow_rule(
    app: AppHandle,
    draft: WorkflowRuleDraft,
) -> Result<WorkflowRule, String> {
    validate_rule_draft(&draft)?;
    let resolved = resolve_action_fields(&draft)?;

    let rule = WorkflowRule {
        id: WorkflowRuleId(Uuid::new_v4()),
        name: draft.name.trim().to_string(),
        workflow_id: resolved.workflow_id,
        account_id: draft.account_id,
        folder_name: draft
            .folder_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        predicates: draft.predicates,
        mode: draft.mode,
        action: draft.action,
        action_dest: resolved.action_dest,
        delay_minutes: draft.delay_minutes,
        dry_run: draft.dry_run,
        enabled: draft.enabled,
        created_at: Utc::now(),
        hit_count: 0,
        last_hit_at: None,
    };

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::InsertWorkflowRule {
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
pub async fn update_workflow_rule(
    app: AppHandle,
    rule_id: WorkflowRuleId,
    draft: WorkflowRuleDraft,
) -> Result<WorkflowRule, String> {
    validate_rule_draft(&draft)?;
    let resolved = resolve_action_fields(&draft)?;
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let existing = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_workflow_rule(&conn, &rule_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Regel existiert nicht mehr.".to_string())?
    };

    // Keep server-owned stats (hit_count, last_hit_at, created_at) —
    // the draft only carries editable user fields.
    let rule = WorkflowRule {
        id: existing.id,
        name: draft.name.trim().to_string(),
        workflow_id: resolved.workflow_id,
        account_id: draft.account_id,
        folder_name: draft
            .folder_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        predicates: draft.predicates,
        mode: draft.mode,
        action: draft.action,
        action_dest: resolved.action_dest,
        delay_minutes: draft.delay_minutes,
        dry_run: draft.dry_run,
        enabled: draft.enabled,
        created_at: existing.created_at,
        hit_count: existing.hit_count,
        last_hit_at: existing.last_hit_at,
    };

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpdateWorkflowRule {
            rule: rule.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update: {e}"))?;
    Ok(rule)
}

/// Action-spezifische Felder konsistent zurechtlegen:
///   * `RunWorkflow` braucht `workflow_id` Pflicht und ignoriert `action_dest`.
///   * `Move` braucht `action_dest`, `workflow_id` ist `None`.
///   * `Archive`/`Delete` ignorieren beide → beide auf `None`.
struct ResolvedActionFields {
    workflow_id: Option<WorkflowId>,
    action_dest: Option<String>,
}

fn resolve_action_fields(draft: &WorkflowRuleDraft) -> Result<ResolvedActionFields, String> {
    match draft.action {
        RuleAction::RunWorkflow => {
            let wid = draft
                .workflow_id
                .ok_or_else(|| "Action 'run_workflow' braucht eine Workflow-Auswahl.".to_string())?;
            Ok(ResolvedActionFields {
                workflow_id: Some(wid),
                action_dest: None,
            })
        }
        RuleAction::Archive | RuleAction::Delete => Ok(ResolvedActionFields {
            workflow_id: None,
            action_dest: None,
        }),
        RuleAction::Move => {
            let dest = draft
                .action_dest
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Action 'move' braucht einen Zielordner.".to_string())?;
            Ok(ResolvedActionFields {
                workflow_id: None,
                action_dest: Some(dest.to_string()),
            })
        }
    }
}

#[tauri::command]
pub async fn delete_workflow_rule(
    app: AppHandle,
    rule_id: WorkflowRuleId,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteWorkflowRule { rule_id, ack: tx })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete: {e}"))
}

#[tauri::command]
pub async fn set_workflow_rule_enabled(
    app: AppHandle,
    rule_id: WorkflowRuleId,
    enabled: bool,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::SetWorkflowRuleEnabled {
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

/// Apply a confirm-mode rule that the user approved via the toast.
/// Thin wrapper: run the workflow (same lifecycle as manual apply),
/// then bump the rule's hit counter on success. Keeps `apply_workflow`
/// itself unaware of rules.
#[tauri::command]
pub async fn apply_workflow_rule(
    app: AppHandle,
    rule_id: WorkflowRuleId,
    message_id: MessageId,
) -> Result<workflows::WorkflowRunResult, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let rule = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_workflow_rule(&conn, &rule_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Regel nicht gefunden.".to_string())?
    };

    // `apply_workflow_rule` ist der Confirm-Modus-Pfad — das Frontend
    // ruft den nur, wenn die Rule auf einen Workflow zeigt. Direkt-
    // Aktionen (Archive/Delete/Move) tagen das Envelope und der Sweeper
    // führt aus, ohne UI-Bestätigung. Wenn der Confirm-Pfad doch mal
    // mit einer Direkt-Action-Rule reinkommt: hart Fehler werfen, weil
    // das ein Frontend-Bug wäre.
    let workflow_id = rule.workflow_id.ok_or_else(|| {
        "Diese Regel hat keine Workflow-Bindung — Direkt-Aktionen werden vom Sweeper ausgeführt."
            .to_string()
    })?;
    let result =
        // Confirm-Mode-Pfad: User hat im Toast bestätigt. Prompt-
        // Param-Werte wären jetzt Pflicht — den Confirm-Toast
        // erweitern wir später um einen Pre-Apply-Dialog. Für jetzt
        // läuft's mit leerer Map (Defaults greifen, Required-Prompts
        // ohne Default werfen einen sauberen Fehler).
        workflows::apply_with_lifecycle(
            &app,
            db,
            workflow_id,
            message_id,
            std::collections::HashMap::new(),
        )
        .await?;

    // Count the hit. Mirrors the auto branch in
    // `workflow_rules::spawn_auto_apply`.
    let (tx, rx) = oneshot::channel();
    let _ = db
        .writer
        .send(WriteCmd::IncrementWorkflowRuleHit { rule_id, ack: tx })
        .await;
    let _ = rx.await;

    Ok(result)
}

// ─── training candidates ─────────────────────────────────────────────

#[tauri::command]
pub async fn list_workflow_training_candidates(
    app: AppHandle,
) -> Result<Vec<queries::WorkflowTrainingCandidate>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_workflow_training_candidates(&conn).map_err(|e| e.to_string())
}

/// Compact list endpoint used by the frontend to build a
/// `Set<messageId>` for badge rendering — cheaper than JOIN-ing into
/// every envelope query.
#[tauri::command]
pub async fn list_workflow_training_ids(
    app: AppHandle,
) -> Result<Vec<MessageId>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_workflow_training_ids(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn is_workflow_training_candidate(
    app: AppHandle,
    message_id: MessageId,
) -> Result<bool, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::is_workflow_training_candidate(&conn, &message_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_workflow_training(
    app: AppHandle,
    message_ids: Vec<MessageId>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::AddWorkflowTraining {
            message_ids,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db insert: {e}"))
}

#[tauri::command]
pub async fn remove_workflow_training(
    app: AppHandle,
    message_ids: Vec<MessageId>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::RemoveWorkflowTraining {
            message_ids,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete: {e}"))
}

/// Result of a successful pi training run, shipped to the frontend.
/// Feature list included so the dialog can show "pi saw these 4
/// mails" without another round-trip.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainingResult {
    pub proposal: workflow_training::RuleProposal,
    pub features: Vec<workflow_training::TrainingFeatures>,
    pub raw_response: String,
}

/// Sentinel returned when the user cancelled. Kept identical in
/// spelling to the spam path so a generic frontend matcher can
/// recognise either signal.
pub const WORKFLOW_TRAINING_CANCELLED: &str = "cancelled_by_user";

/// Timeout cap (in seconds) for one pi training call — generous on
/// a local small model, still fails the UI cleanly if pi is stuck.
const WORKFLOW_TRAINING_PI_TIMEOUT_SECS: u64 = 120;

/// pi-driven rule learner. Takes the workflow to attach the rule to
/// plus the current training candidate set, runs pi once, and parses
/// the response into a `RuleProposal`. The frontend then previews
/// the proposal and either calls `add_workflow_rule` with it or
/// opens the rule editor pre-filled.
#[tauri::command]
pub async fn suggest_workflow_rule(
    app: AppHandle,
    workflow_id: WorkflowId,
) -> Result<TrainingResult, String> {
    let state = app.state::<AppState>();
    if !crate::commands::pi::ai_enabled(state.inner()) {
        return Err(crate::commands::pi::AI_DISABLED_ERR.to_string());
    }
    let db = state.db.get().ok_or("database not ready")?;

    let workflow = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_workflow(&conn, &workflow_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Workflow nicht gefunden.".to_string())?
    };

    let candidates = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_workflow_training_candidates(&conn)
            .map_err(|e| e.to_string())?
    };
    if candidates.is_empty() {
        return Err(
            "Keine Trainings-Mails markiert — im Reader auf die Kappe klicken."
                .into(),
        );
    }
    let ids: Vec<MessageId> =
        candidates.iter().map(|c| c.message_id).collect();

    let features =
        workflow_training::collect_training_features(db, &ids).await?;
    if features.is_empty() {
        return Err(
            "Zu den markierten Mails wurden keine Envelopes geladen.".into(),
        );
    }

    let prompt = workflow_training::build_prompt(&features, &workflow.name);

    let accounts = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_accounts(&conn).map_err(|e| e.to_string())?
    };

    let raw_response =
        run_workflow_training_pi_oneshot(&app, state.inner(), prompt).await?;

    let proposal =
        workflow_training::parse_pi_response(&raw_response, &accounts)?;

    Ok(TrainingResult {
        proposal,
        features,
        raw_response,
    })
}

#[tauri::command]
pub async fn cancel_workflow_training(app: AppHandle) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let state = app.state::<AppState>();
    state
        .workflow_training_cancel_requested
        .store(true, Ordering::Relaxed);
    let guard = state.active_workflow_training_pi.lock().await;
    if let Some(rpc) = guard.as_ref() {
        rpc.kill().await;
    }
    Ok(())
}

/// Dedicated pi one-shot for workflow training. Same shape as the
/// spam-analysis helper: tools off, thinking off, kill_on_drop. See
/// `commands::spam::run_spam_pi_oneshot` for the rationale — we copy
/// rather than share so the two paths can evolve their prompts /
/// timeouts independently.
async fn run_workflow_training_pi_oneshot(
    app: &AppHandle,
    state: &AppState,
    prompt: String,
) -> Result<String, String> {
    use std::sync::atomic::Ordering;

    let base_cfg = {
        let guard = state.pi_config.lock().unwrap();
        guard.clone()
    };
    // Piggyback on the user's spam-provider override — "cloud pi for
    // AI tasks, local pi for chat" is the typical setup and workflow
    // training is another AI task with the same tradeoff profile.
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
        session_file: "pi_workflow_training_session.jsonl".into(),
        prompt_prefix: String::new(),
        ..base_cfg
    };

    state
        .workflow_training_cancel_requested
        .store(false, Ordering::Relaxed);

    let rpc = PiRpc::spawn(app.clone(), &cfg).await?;
    *state.active_workflow_training_pi.lock().await = Some(rpc.clone());

    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(WORKFLOW_TRAINING_PI_TIMEOUT_SECS),
        // Sentinel key for our rule-learner schema — without this
        // parametrisation the detector would hunt for `rules` (the
        // spam schema) and never fire on a predicates-shaped answer,
        // leaving the dialog hanging until the 120s timeout.
        rpc.prompt_collect_until_key(prompt, "predicates"),
    )
    .await;

    let response_or_partial: Result<String, String> = match timeout_result {
        Ok(result) => result,
        Err(_) => {
            let partial = rpc.collected_snapshot().await;
            if partial.trim().is_empty() {
                Err(format!(
                    "pi-Training dauerte länger als {WORKFLOW_TRAINING_PI_TIMEOUT_SECS}s \
                     ohne eine einzige Ausgabe. Kleineres Modell oder Cloud-Provider probieren."
                ))
            } else {
                tracing::warn!(
                    "pi-Training timeout with partial buffer ({} bytes) — returning for parsing.",
                    partial.len()
                );
                Ok(partial)
            }
        }
    };

    *state.active_workflow_training_pi.lock().await = None;

    if state
        .workflow_training_cancel_requested
        .load(Ordering::Relaxed)
    {
        return Err(WORKFLOW_TRAINING_CANCELLED.into());
    }

    response_or_partial.map_err(|e| format!("pi-Aufruf fehlgeschlagen: {e}"))
}

#[tauri::command]
pub async fn clear_workflow_training(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::ClearWorkflowTraining { ack: tx })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete: {e}"))
}

fn validate_rule_draft(d: &WorkflowRuleDraft) -> Result<(), String> {
    if d.predicates.is_empty() {
        return Err("Regel braucht mindestens ein Kriterium.".into());
    }
    // Empty-value predicates are meaningless (would either match
    // everything or nothing depending on the predicate type). Reject
    // early so the matcher doesn't have to defend against them.
    for p in &d.predicates {
        match p {
            RulePredicate::FromEmail { value }
            | RulePredicate::FromDomain { value }
            | RulePredicate::SubjectContains { value } => {
                if value.trim().is_empty() {
                    return Err("Regel-Wert darf nicht leer sein.".into());
                }
            }
            RulePredicate::FromDomainIn { values } => {
                if values.iter().all(|v| v.trim().is_empty()) {
                    return Err(
                        "Mindestens eine Domain mit Wert angeben.".into(),
                    );
                }
            }
            RulePredicate::HasAttachmentExtension { extension } => {
                if extension.trim().is_empty() {
                    return Err(
                        "Anhang-Typ darf nicht leer sein.".into(),
                    );
                }
            }
        }
    }
    if d.name.trim().is_empty() {
        return Err("Regel-Name darf nicht leer sein.".into());
    }
    Ok(())
}

// ─── Scheduling-spezifische Commands ────────────────────────────────────

/// Pro-Rule-Trockenmodus-Toggle. Eigenes Command, damit das Settings-UI
/// einen einfachen Switch ohne kompletten Draft-Update bauen kann.
#[tauri::command]
pub async fn set_workflow_rule_dry_run(
    app: AppHandle,
    rule_id: WorkflowRuleId,
    dry_run: bool,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::SetWorkflowRuleDryRun {
            rule_id,
            dry_run,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update: {e}"))
}

/// Backfill-Sweep: Inbox-Mails durchziehen und passend taggen. Frontend
/// zeigt nach `add_workflow_rule` einen Dialog: "247 Mails passen — jetzt
/// markieren?". Der Sweeper erledigt die Action später (Direkt-Aktionen
/// laufen beim nächsten Sync-Sweep, mit allen Skip-Schutzschichten).
#[tauri::command]
pub async fn apply_workflow_rule_to_existing(
    app: AppHandle,
    rule_id: WorkflowRuleId,
) -> Result<u32, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let rule = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_workflow_rule(&conn, &rule_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Regel nicht gefunden.".to_string())?
    };
    rule_scheduler::apply_to_existing(db, &rule).await
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSweepReport {
    pub ok: u32,
    pub skipped: u32,
    pub failed: u32,
}

/// Manueller Sweep-Trigger für Tests/Debug. Im Normalbetrieb läuft der
/// Sweep nach jedem Sync automatisch — der User braucht das nur, wenn
/// er ein "jetzt sofort"-Bedürfnis hat.
#[tauri::command]
pub async fn run_rule_sweep_now(app: AppHandle) -> Result<RuleSweepReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let counts = rule_scheduler::sweep_once(&app, db).await;
    Ok(RuleSweepReport {
        ok: counts.ok,
        skipped: counts.skipped,
        failed: counts.failed,
    })
}

#[tauri::command]
pub async fn list_rule_action_log(
    app: AppHandle,
    rule_id: Option<WorkflowRuleId>,
    limit: Option<u32>,
) -> Result<Vec<RuleActionLogEntry>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_rule_action_log(&conn, rule_id.as_ref(), limit.unwrap_or(200))
        .map_err(|e| e.to_string())
}
