use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex, OnceCell};

use crate::domain::account::AccountId;
use crate::domain::message::MessageId;
use crate::infrastructure::db::DbHandle;

/// Pi (agent CLI) configuration — fields mirror mila so the RPC port works
/// unchanged. A config change triggers respawn via `pi_rpc::fingerprint`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiConfig {
    pub bin_path: String,
    pub provider: String,
    pub model: String,
    pub session_dir: String,
    pub session_file: String,
    pub tools: String,
    pub thinking: String,
    pub extra_args: Vec<String>,
    pub show_thinking: bool,
    /// User-defined instructions prepended to every prompt. Good place to
    /// put persona ("Antworte knapp, keine Floskeln."), tone, or policy
    /// hints that should apply to every conversation. Empty = no prefix.
    #[serde(default)]
    pub prompt_prefix: String,
    /// Override provider used **only** for the one-shot spam-analysis
    /// flow (not the interactive chat). Empty string = reuse the main
    /// `provider`. The typical setup: local gemma for chat (private
    /// mail content) + a cloud model (anthropic/openai) for spam
    /// analysis where the content isn't sensitive and JSON generation
    /// quality matters more.
    #[serde(default)]
    pub spam_provider: String,
    /// Override model for spam analysis. Empty string = reuse `model`.
    #[serde(default)]
    pub spam_model: String,
    /// Master AI kill-switch. When `false`, every entry point that would
    /// spawn or talk to a pi process refuses with the `"ai_disabled"`
    /// sentinel error string — the frontend pattern-matches that to
    /// show a friendly notice instead of a stack trace. Background
    /// pattern-matchers (workflow / spam rules) keep running because
    /// they only execute pre-compiled regexes; they don't call pi.
    /// Default `true` so existing setups don't suddenly lose AI on an
    /// app update; users opt out explicitly via the settings switch.
    #[serde(default = "default_ai_enabled")]
    pub enabled: bool,
}

fn default_ai_enabled() -> bool {
    true
}

impl Default for PiConfig {
    fn default() -> Self {
        Self {
            bin_path: "pi".to_string(),
            provider: "ollama".to_string(),
            model: "gemma3".to_string(),
            session_dir: String::new(),
            session_file: String::new(),
            // Mail AI features read-only by default: never let pi write/bash
            // against the user's filesystem during normal mail operations.
            tools: "read,grep,find,ls".to_string(),
            thinking: "off".to_string(),
            extra_args: Vec::new(),
            show_thinking: false,
            prompt_prefix: String::new(),
            spam_provider: String::new(),
            spam_model: String::new(),
            enabled: true,
        }
    }
}

/// Workflow-engine configuration. Holds the two trust-boundary settings
/// that gate what the executor is allowed to touch:
///
///   * `script_dir` — the only directory from which `RunScript` steps
///     may pick a Python file. Chosen once by the user via a native
///     folder-pick dialog; absolute path, no expansion at use time.
///   * `python_bin` — the interpreter used for every `.py` step. On
///     Windows we default to `py` (the official launcher); users can
///     override with an absolute path to a venv python.
///
/// Both fields empty = the user hasn't configured workflows yet; the
/// executor refuses `RunScript` until they do.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowConfig {
    #[serde(default)]
    pub script_dir: String,
    #[serde(default)]
    pub python_bin: String,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        // On Windows the Python launcher `py` is the right default: it
        // resolves to whatever interpreter the user has registered and
        // survives 3.x minor-version bumps without config changes. On
        // non-Windows hosts we fall back to `python3` which is the
        // de-facto standard on Linux/macOS. Users can override either
        // by setting an absolute path in Settings.
        #[cfg(target_os = "windows")]
        let default_py = "py".to_string();
        #[cfg(not(target_os = "windows"))]
        let default_py = "python3".to_string();
        Self {
            script_dir: String::new(),
            python_bin: default_py,
        }
    }
}

/// Phase-2 calendar IMAP sync configuration. When `enabled` is `false`
/// (the default), the calendar lives only in SQLite — Phase-1 behavior.
/// When the user opts in via Settings and picks an `account_id`, the
/// "Sync"-Action in the Calendar view starts publishing/reading the
/// configured `folder_path` on that account per ADR-0011.
///
/// `folder_path` is the raw IMAP path the server expects. The default
/// uses the conventional `/` delimiter; servers using `.` (Cyrus-style)
/// require the user to set the override (e.g. `INBOX.TimeProtocol.Calendar`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub account_id: Option<AccountId>,
    #[serde(default = "default_folder_path")]
    pub folder_path: String,
    /// Interval (seconds) for the background periodic-sync task. Floor
    /// is 60 s — anything shorter is mostly self-DOSing the IMAP server
    /// without changing user-perceived latency. Default 300 s (5 min).
    /// Set this to 0 to disable periodic sync entirely (manual + IDLE
    /// + sync-on-mutation still run).
    #[serde(default = "default_auto_sync_interval")]
    pub auto_sync_interval_seconds: u64,
    /// When true, the IMAP-IDLE actor opens a long-lived session against
    /// the calendar folder and triggers a sync whenever the server
    /// notifies of a change. Cheaper than aggressive polling for
    /// typical home-server / Dovecot deployments. Default true.
    #[serde(default = "default_idle_enabled")]
    pub idle_enabled: bool,
    /// When true, every mutation (create/update/cancel/import) fires a
    /// fire-and-forget background publish so the user doesn't have to
    /// click Sync after every edit. Default true. Errors land in the
    /// dev console; the next periodic / IDLE-triggered sync catches
    /// up automatically.
    #[serde(default = "default_sync_on_mutation")]
    pub sync_on_mutation: bool,
    /// When true, every successful sync runs a compaction pass that
    /// moves superseded ICS messages to `<folder>/Archive`. Default
    /// true — keeps active folders bounded for typical multi-year
    /// usage. ADR-0011 §6 makes compaction OPTIONAL and the Archive
    /// folder OPTIONAL; readers that ignore Archive still reconstruct
    /// state correctly.
    #[serde(default = "default_compaction_enabled")]
    pub compaction_enabled: bool,
}

fn default_folder_path() -> String {
    "INBOX/TimeProtocol/Calendar".to_string()
}

fn default_auto_sync_interval() -> u64 {
    300
}

fn default_idle_enabled() -> bool {
    true
}

fn default_sync_on_mutation() -> bool {
    true
}

fn default_compaction_enabled() -> bool {
    true
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            account_id: None,
            folder_path: default_folder_path(),
            auto_sync_interval_seconds: default_auto_sync_interval(),
            idle_enabled: default_idle_enabled(),
            sync_on_mutation: default_sync_on_mutation(),
            compaction_enabled: default_compaction_enabled(),
        }
    }
}

pub struct AppState {
    pub pi_config: std::sync::Mutex<PiConfig>,
    /// Persistent pi RPC process; spawned on first AI request.
    pub pi_rpc: Mutex<Option<Arc<crate::llm::pi_rpc::PiRpc>>>,
    /// Opened asynchronously in the Tauri setup hook — hence `OnceCell`.
    /// All DB commands go through this handle.
    pub db: OnceCell<DbHandle>,
    /// Account IDs whose background-prefetch worker is currently running.
    /// Prevents the same account from spawning two parallel IMAP sessions
    /// when the worker is triggered both from sync and from the frontend.
    pub prefetch_running: std::sync::Mutex<HashSet<AccountId>>,
    /// Cancel-tokens for in-flight `open_message` body fetches, keyed by
    /// message id. Archive/delete/move fire the matching token before
    /// starting their own IMAP op so the fetch drops its session early
    /// — saves bandwidth and lets the next operation reach the server
    /// on the same connection slot without queuing.
    pub pending_fetch_cancels: std::sync::Mutex<HashMap<MessageId, oneshot::Sender<()>>>,
    /// Handle to the currently-running one-shot spam-analysis pi
    /// subprocess (if any). Populated by `suggest_spam_rules` for the
    /// duration of the call; `cancel_spam_analysis` kills it, which
    /// unblocks the waiting call via EOF on stdout.
    pub active_spam_pi: Mutex<Option<Arc<crate::llm::pi_rpc::PiRpc>>>,
    /// Latch set by `cancel_spam_analysis` so the caller can distinguish
    /// "user aborted" from "pi crashed". Reset at the start of each
    /// suggest-call.
    pub spam_cancel_requested: AtomicBool,
    /// Workflow engine configuration (script dir + interpreter). Hydrated
    /// from disk at startup via `commands::workflows::load_persisted`.
    pub workflow_config: std::sync::Mutex<WorkflowConfig>,
    /// Phase-2 calendar IMAP sync configuration. Hydrated at startup via
    /// `timeprotocol::commands::load_persisted_calendar_config`.
    pub calendar_config: std::sync::Mutex<CalendarConfig>,
    /// Single-flight gate for all calendar sync trigger sources (manual
    /// button, periodic timer, IDLE actor, sync-on-mutation). Holding
    /// this lock guarantees only one sync runs at a time, so concurrent
    /// triggers don't race against each other publishing or importing
    /// the same UIDs. Tokio Mutex (async-aware) so a periodic-tick
    /// trigger waiting for an in-flight manual sync doesn't block the
    /// runtime.
    pub calendar_sync_lock: Mutex<()>,
    /// Optional command channel to the live IMAP-IDLE calendar actor.
    /// Set when an actor task is currently running (config enabled +
    /// idle_enabled + account selected). `cal_set_config` sends a
    /// `Reload` message through here so the actor restarts against the
    /// new account/folder; `None` after shutdown.
    pub calendar_actor_tx: Mutex<
        Option<tokio::sync::mpsc::Sender<crate::application::calendar_actor::ActorCmd>>,
    >,
    /// Twin of `active_spam_pi` for the workflow-rule trainer.
    /// Separate so a spam-analysis run in flight doesn't block the
    /// user from training a workflow rule (they're independent
    /// user-initiated operations) — and vice versa.
    pub active_workflow_training_pi: Mutex<Option<Arc<crate::llm::pi_rpc::PiRpc>>>,
    pub workflow_training_cancel_requested: AtomicBool,
    /// Per-Konto-Background-Sync-Actors: jeder Account mit `sync_mode`
    /// idle/polling/idle_and_polling läuft hier mit einem Channel-Handle,
    /// über das die Tauri-Commands Lifecycle-Events schicken (Updated /
    /// Shutdown). Beim App-Start werden Actors für alle Accounts
    /// gespawnt, beim Account-Add ein neuer dazu, beim Account-Update
    /// kriegt der bestehende Actor `Updated(Account)`, beim Delete
    /// `Shutdown`. Siehe `application::actor`.
    pub actor_handles: Mutex<HashMap<AccountId, crate::application::actor::ActorHandle>>,
    /// Puffer für externe Draft-Imports (CLI-Aufruf), die ankommen,
    /// bevor das Frontend seinen Compose-Listener gemountet hat.
    /// Wird vom Argv-Pfad in `main.rs` befüllt und vom Frontend einmal
    /// per `consume_pending_import_drafts` geleert. Live-Imports
    /// (App läuft schon) gehen direkt als Tauri-Event raus und
    /// landen *nicht* hier — der Puffer ist nur für die
    /// Cold-Start-Race da.
    pub pending_import_drafts: std::sync::Mutex<Vec<crate::application::draft_import::PreparedImportDraft>>,
    /// Read-only third-party iCal subscriptions overlay. Lazily filled
    /// in the Tauri setup hook (we need `AppHandle::path()` to know
    /// where to put the on-disk cache, which isn't available in
    /// `AppState::default`). Once present, holds the persisted list +
    /// the in-memory parsed-event cache. Lookups from
    /// `cal_list_in_range` merge with this overlay on top of SQLite.
    pub subscription_store: tokio::sync::OnceCell<
        std::sync::Arc<crate::timeprotocol::subscriptions::SubscriptionStore>,
    >,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            pi_config: std::sync::Mutex::new(PiConfig::default()),
            pi_rpc: Mutex::new(None),
            db: OnceCell::new(),
            prefetch_running: std::sync::Mutex::new(HashSet::new()),
            pending_fetch_cancels: std::sync::Mutex::new(HashMap::new()),
            active_spam_pi: Mutex::new(None),
            spam_cancel_requested: AtomicBool::new(false),
            workflow_config: std::sync::Mutex::new(WorkflowConfig::default()),
            calendar_config: std::sync::Mutex::new(CalendarConfig::default()),
            calendar_sync_lock: Mutex::new(()),
            calendar_actor_tx: Mutex::new(None),
            active_workflow_training_pi: Mutex::new(None),
            workflow_training_cancel_requested: AtomicBool::new(false),
            actor_handles: Mutex::new(HashMap::new()),
            pending_import_drafts: std::sync::Mutex::new(Vec::new()),
            subscription_store: tokio::sync::OnceCell::new(),
        }
    }
}
