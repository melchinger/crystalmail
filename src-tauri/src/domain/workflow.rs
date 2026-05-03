// User-defined workflows: an ordered list of typed actions applied to
// one message. Stage 1 is manual ("apply this workflow to the focused
// message"); Stage 2 adds rule-based auto-trigger on top of the same
// domain model.
//
// The `Step` enum is deliberately a closed tagged union. New action
// types require a Rust-side implementation in `application::workflows`
// — this keeps the trust boundary visible in the type system instead
// of hiding it behind a generic "run arbitrary thing" step.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowId(pub Uuid);

/// One step in a workflow. Tagged JSON on the wire/DB:
/// `{"type":"saveAttachments","targetDir":"…","filter":"*.csv"}`.
///
/// Template variables usable in string fields (and as the `template`
/// source for `ScriptParam`):
///   * `subject`, `from`, `date` — envelope metadata
///   * `attachments_dir` — path to a freshly-created temp dir
///     containing *all* attachments of the message (populated lazily
///     by the executor when any step references it)
///   * `csv` — path to the *first* `.csv` attachment, if present
///   * `body_md` — path to a markdown file containing the mail body
///     (populated when a `SaveBody` step ran earlier in the chain)
///
/// In plain string fields (like `SaveAttachments.target_dir`), vars
/// are referenced as `$name`. In `ScriptParam::Template`, just the
/// name.
#[derive(Debug, Clone, Serialize, Deserialize)]
// `rename_all` alone only renames the *variant names* (SaveAttachments
// → saveAttachments). We need `rename_all_fields` too so the per-variant
// struct fields (`target_dir`, `cli_name`, …) round-trip as
// `targetDir` / `cliName` — otherwise deserialisation from the
// camelCase-producing frontend fails with `missing field target_dir`.
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Step {
    /// Write every (non-inline) attachment into `target_dir`. The dir
    /// is created on demand. Optional glob filter (`*.csv`, `*.pdf`)
    /// restricts which filenames are written out.
    SaveAttachments {
        target_dir: String,
        #[serde(default)]
        filter: Option<String>,
    },
    /// Write the message body to `path`. `format = md|txt|eml` picks
    /// the serialization (md = subject + headers + plain body).
    SaveBody {
        path: String,
        #[serde(default = "default_body_format")]
        format: BodyFormat,
    },
    /// Invoke a Python script from the configured workflow-scripts dir.
    /// `script` is the *filename* (no path); resolution is done by the
    /// executor against `WorkflowConfig::script_dir`.
    ///
    /// `parameters` is a structured argv-builder modelled after
    /// launchpad: each entry is one argparse parameter the script
    /// accepts, with its kind (positional/option/flag), and a source
    /// that decides what the concrete value is at run time (a fixed
    /// string the user typed into Settings, or a named template var
    /// like `csv`). The executor assembles argv from this in order.
    RunScript {
        script: String,
        #[serde(default)]
        parameters: Vec<ScriptParam>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterKind {
    Positional,
    Option,
    Flag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Number,
    Boolean,
    Choice,
    Path,
}

/// Where the concrete value of a parameter comes from at run time.
/// Tagged union: `{"kind":"fixed","value":"…"}`,
/// `{"kind":"template","var":"subject"}`, or
/// `{"kind":"firstAttachment","extension":"csv"}` on the wire.
///
/// We deliberately don't model "prompt user at run time" here —
/// workflows in crystalmail are applied to a focused message, not
/// run interactively. If a user picks a workflow via the hotkey, we
/// run it; no extra modal for argument entry.
///
/// `FirstAttachment` is a cousin of the `csv` template var, generalised
/// to "give me the first non-inline attachment whose filename ends in
/// `.<extension>`". Covers the common "here's a PDF / ZIP / XML — hand
/// it to the script" case without cluttering the template-var list
/// with one var per file type.
#[derive(Debug, Clone, Serialize, Deserialize)]
// Same story as `Step`: `rename_all_fields` keeps `var` / `value` /
// `extension` round-tripping as camelCase on the wire.
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ParamSource {
    Fixed { value: String },
    Template { var: String },
    /// Extension without the leading dot. Lower-cased matching, so
    /// `CSV` vs `csv` on the filesystem both hit. Compound suffixes
    /// like `tar.gz` work because the matcher does a literal
    /// ends-with check on the whole suffix.
    FirstAttachment { extension: String },
    /// User wird beim Workflow-Apply gefragt. Frontend zeigt vor dem
    /// eigentlichen Lauf einen Dialog mit Eingabefeldern für jeden
    /// Prompt-Param; die Werte werden als Map an `apply_workflow`
    /// gereicht. `defaultTemplate` (optional) wird bei Anzeige
    /// vorgeblendet — kann ein literaler String oder ein Template-
    /// Ausdruck wie `$subject` sein. Der User darf den Wert ändern
    /// oder leer lassen (bei nicht-required Params).
    ///
    /// Bei Auto-Trigger-Rules (`mode = auto`) ohne Default kann die
    /// Resolution scheitern — der Workflow läuft dann mit Fehlermeldung
    /// auf, ohne UI-Popup. Wer Auto-Mode mit Prompt-Params will, sollte
    /// zwingend ein `defaultTemplate` setzen.
    Prompt {
        #[serde(default)]
        default_template: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptParam {
    /// Stable internal identifier (usually the argparse dest name).
    pub key: String,
    /// CLI surface form — `--output-dir`, `-v`, or `input` for a
    /// positional. Exactly what argparse reads.
    pub cli_name: String,
    pub kind: ParameterKind,
    /// Human-readable label for the editor UI.
    pub label: String,
    pub value_type: ValueType,
    /// Allowed choices (present for `value_type = choice`).
    #[serde(default)]
    pub choices: Vec<String>,
    /// Help text lifted from argparse's `help=` — shown as tooltip.
    #[serde(default)]
    pub help_text: Option<String>,
    /// Whether the script declares this as required. Executor fails
    /// the step when a required param produces an empty value.
    #[serde(default)]
    pub required: bool,
    /// Default value picked up from argparse. Preserved so re-analysis
    /// can show what upstream changed, and so the editor can suggest
    /// it as a fixed value.
    #[serde(default)]
    pub default_value: Option<String>,
    /// What gets handed in as argv at run time.
    pub source: ParamSource,
    /// Render order — low-to-high. argparse order by default.
    #[serde(default)]
    pub order: u32,
    /// Whether this parameter contributes to the argv at all. Users
    /// switch off optional params they don't want to pass without
    /// deleting them (preserves defaults for later re-enable).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyFormat {
    Md,
    Txt,
    Eml,
}

fn default_body_format() -> BodyFormat {
    BodyFormat::Md
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: WorkflowId,
    pub name: String,
    /// Optional key binding. Normalised in the same form as the
    /// hotkey registry (see `settings/hotkeys.ts`). `None` = not bound.
    pub hotkey: Option<String>,
    pub steps: Vec<Step>,
    pub enabled: bool,
    /// Move the message to the account's archive folder after the
    /// workflow runs end-to-end successfully (`all_ok = true`).
    /// Failed runs never archive — the user needs to inspect the
    /// result dialog first.
    #[serde(default)]
    pub archive_after_success: bool,
    pub created_at: DateTime<Utc>,
    pub run_count: u64,
    pub last_run_at: Option<DateTime<Utc>>,
}

/// Draft used by the `add_workflow` / `update_workflow` command path.
/// Same shape as `Workflow` minus the server-owned fields (id, stats).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDraft {
    pub name: String,
    #[serde(default)]
    pub hotkey: Option<String>,
    pub steps: Vec<Step>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub archive_after_success: bool,
}

fn default_true() -> bool {
    true
}

// ─── auto-trigger rules ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRuleId(pub Uuid);

/// One predicate inside a `WorkflowRule`. Closed tagged enum — same
/// motivation as `Step`: the matcher fails to compile when a new
/// variant lands without a handler.
///
/// Match semantics across predicates within a rule: AND. Match across
/// rules on one workflow: OR. (Any rule firing triggers the workflow.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RulePredicate {
    /// Full sender address, case-insensitive exact match.
    FromEmail { value: String },
    /// Domain portion of sender after `@`, case-insensitive exact.
    FromDomain { value: String },
    /// Sender domain is one of a list. Case-insensitive. Matches if
    /// the envelope's from-domain equals any entry — common shape
    /// for "a handful of trusted vendors" rules, where writing one
    /// rule per domain would be busywork.
    FromDomainIn { values: Vec<String> },
    /// Substring match in subject, case-insensitive.
    SubjectContains { value: String },
    /// At least one non-inline attachment whose filename ends in
    /// `.<extension>` (case-insensitive). Compound suffixes like
    /// `tar.gz` work through the same literal-suffix check the
    /// `FirstAttachment` param source uses.
    HasAttachmentExtension { extension: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleMode {
    /// Run the workflow immediately in the background. Result lands
    /// in a per-app log — no modal pop-up because the user didn't
    /// trigger this manually.
    Auto,
    /// Surface a confirmation toast; the user picks whether to run.
    /// Safer default for workflows that write to the filesystem.
    Confirm,
}

/// Was bei einem Match passieren soll. Vier Varianten, die die zwei
/// Welten abdecken, die wir in v2 zusammengefasst haben:
///
///   * `RunWorkflow` — die alte Workflow-Rules-Welt: Rule zeigt auf
///     einen `Workflow` (Mehrschritt-Pipeline), der bei Match läuft.
///     `workflow_id` MUSS auf der Rule gesetzt sein.
///
///   * `Archive` / `Delete` / `Move` — die alte Lifetime-Rules-Welt:
///     direkte Aktionen, ohne dass der User vorher einen leeren
///     "Archiv"-Workflow anlegen muss. `Move` braucht `action_dest`
///     als Zielordner-Namen; `Archive` und `Delete` greifen auf die
///     Account-Konfiguration zurück (`archive_folder` / `trash_folder`).
///
/// Permanent-Löschung ist bewusst NICHT als Action vorgesehen — zu
/// unwiderruflich für Pattern-Match-Automatik. Wer permanent löschen
/// will, leert den Trash manuell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    RunWorkflow,
    Archive,
    Delete,
    Move,
}

impl RuleAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleAction::RunWorkflow => "run_workflow",
            RuleAction::Archive => "archive",
            RuleAction::Delete => "delete",
            RuleAction::Move => "move",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "run_workflow" => Some(RuleAction::RunWorkflow),
            "archive" => Some(RuleAction::Archive),
            "delete" => Some(RuleAction::Delete),
            "move" => Some(RuleAction::Move),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRule {
    pub id: WorkflowRuleId,
    /// Anzeige-Name. Kann leer sein für ältere Rows aus der Zeit vor v2 —
    /// in dem Fall fällt das UI auf "Workflow-Name" zurück (für
    /// `RunWorkflow`-Actions) bzw. einen generierten Default. Pflichtfeld
    /// für neue Rules ab v2 (Validierung im CRUD-Pfad).
    #[serde(default)]
    pub name: String,
    /// Bei `action = RunWorkflow` PFLICHT (sonst weiß der Sweeper nicht
    /// welcher Workflow zu laufen hat). Bei den drei Direkt-Actions
    /// (Archive/Delete/Move) ignoriert — Setter machen das `None`.
    pub workflow_id: Option<WorkflowId>,
    /// `None` = rule applies to mail on any account. `Some(_)` narrows
    /// to one — useful for "private-mailbox only" scoping.
    pub account_id: Option<super::account::AccountId>,
    /// Optional IMAP folder path the rule is constrained to. `None`
    /// = every folder on the in-scope accounts. Stored verbatim —
    /// `INBOX`, `INBOX.Steuer` etc. Case-sensitive per RFC 3501.
    #[serde(default)]
    pub folder_name: Option<String>,
    pub predicates: Vec<RulePredicate>,
    pub mode: RuleMode,
    pub action: RuleAction,
    /// Nur für `Move` relevant — Zielordner. `None` für die anderen
    /// Actions; bei `Move` wird ein leerer Wert beim Save abgewiesen.
    #[serde(default)]
    pub action_dest: Option<String>,
    /// 0 = sofort beim Match handeln (alte Workflow-Rules-Semantik).
    /// >0 = `mail.date + delay_minutes` ist das Fälligkeitsdatum, bis
    /// dahin trägt die Mail nur ein Snapshot-Tag, der Sweeper räumt
    /// später ab. Minuten als Einheit reicht für „in 10 Min weg" (kurz)
    /// bis „in 30 Tagen weg" (43200 Min) ohne zwei Felder.
    #[serde(default)]
    pub delay_minutes: u32,
    /// `true` = Tagging passiert, aber der Sweeper überspringt die
    /// Ausführung. UX-Schutz beim ersten Anlegen einer Regel — User sieht
    /// im Marker, was passieren würde, ohne dass Mails verschwinden.
    #[serde(default)]
    pub dry_run: bool,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub hit_count: u64,
    pub last_hit_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRuleDraft {
    /// Pflicht für neue Rules (v2). Lassen wir leer durch, validiert das
    /// CRUD-Command und schickt einen Fehler.
    #[serde(default)]
    pub name: String,
    /// Pflicht wenn `action == RunWorkflow`. Bei den Direkt-Actions
    /// (Archive/Delete/Move) wird der Wert beim Insert auf `None` gezwungen.
    #[serde(default)]
    pub workflow_id: Option<WorkflowId>,
    #[serde(default)]
    pub account_id: Option<super::account::AccountId>,
    #[serde(default)]
    pub folder_name: Option<String>,
    pub predicates: Vec<RulePredicate>,
    pub mode: RuleMode,
    #[serde(default = "default_action")]
    pub action: RuleAction,
    #[serde(default)]
    pub action_dest: Option<String>,
    #[serde(default)]
    pub delay_minutes: u32,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_action() -> RuleAction {
    RuleAction::RunWorkflow
}

/// Snapshot eines geplanten Action-Tags auf einer Envelope-Row. Wird
/// vom Frontend gelesen, um den Auto-Rule-Marker + Hover-Tooltip zu
/// rendern. Snapshot-Semantik: einmal getaggte Mail behält ihre
/// ursprüngliche Action-Intention, auch wenn die Regel danach geändert
/// oder gelöscht wird.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledActionTag {
    /// ISO-8601 UTC. Wenn ≤ now → Sweeper packt sie beim nächsten Tick.
    pub scheduled_at: DateTime<Utc>,
    pub action: RuleAction,
    pub action_dest: Option<String>,
    pub rule_id: Option<WorkflowRuleId>,
    pub rule_name: String,
    /// Nur für `RunWorkflow` relevant — welcher Workflow gefeuert wird.
    pub workflow_id: Option<WorkflowId>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleActionResult {
    Ok,
    Skipped,
    Failed,
}

impl RuleActionResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleActionResult::Ok => "ok",
            RuleActionResult::Skipped => "skipped",
            RuleActionResult::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleActionLogEntry {
    pub id: Uuid,
    pub rule_id: Option<WorkflowRuleId>,
    pub rule_name: String,
    pub action: RuleAction,
    pub action_dest: Option<String>,
    pub workflow_id: Option<WorkflowId>,
    pub message_id: super::message::MessageId,
    pub subject_snapshot: String,
    pub sender_snapshot: String,
    pub result: RuleActionResult,
    pub error_message: Option<String>,
    pub ran_at: DateTime<Utc>,
}
