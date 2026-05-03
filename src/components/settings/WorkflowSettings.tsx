import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type {
  AccountSummary,
  FolderSummary,
  ParamSource,
  RuleAction,
  RuleActionLogEntry,
  RuleMode,
  RulePredicate,
  ScriptParam,
  ScriptValueType,
  Workflow,
  WorkflowConfig,
  WorkflowDraft,
  WorkflowRule,
  WorkflowRuleDraft,
  WorkflowStep,
  WorkflowTrainingCandidate,
} from "../../types";
import { WorkflowTrainingDialog } from "./WorkflowTrainingDialog";
import {
  WORKFLOW_ATTACHMENT_EXTENSIONS,
  WORKFLOW_TEMPLATE_VARS,
} from "../../types";

/**
 * Top-level Workflows panel. Three regions:
 *
 *   1. Global config: script directory (picked via native dialog) and
 *      python interpreter command/path. Until `scriptDir` is set, the
 *      `RunScript` step type is effectively disabled — the editor
 *      makes that visible instead of silently failing on apply.
 *   2. Workflow list: one row per workflow with its name, hotkey, and
 *      recent stats. Edit opens the modal `WorkflowEditor`.
 *   3. Empty-state hint when no workflows exist yet — guides the user
 *      to pick a script and start binding parameters.
 *
 * All DB / filesystem interaction happens through the Tauri commands
 * `list_workflows`, `get/set_workflow_config`, `list_workflow_scripts`,
 * `analyze_python_script`, `add/update/delete_workflow`. The component
 * is stateful for the list + config, and delegates step editing to
 * `WorkflowEditor`.
 */
type Props = {
  accounts: AccountSummary[];
  /** Pass through to WorkflowTrainingDialog so the AI-required notice
   *  can offer a "→ KI-Einstellungen" jump. From within Settings the
   *  parent (`SettingsDialog`) wires this to `setActive("pi")`. */
  onOpenAiSettings?: () => void;
};

export function WorkflowSettings({ accounts, onOpenAiSettings }: Props) {
  const { t } = useTranslation();
  const [cfg, setCfg] = useState<WorkflowConfig | null>(null);
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<Workflow | "new" | null>(null);
  // Training-candidate count is a top-level concern because it spans
  // workflows — the user may have marked mails for multiple different
  // rules. Showing it here makes "I forgot I had training candidates"
  // impossible.
  const [trainingCount, setTrainingCount] = useState<number>(0);

  const refreshTraining = useCallback(async () => {
    try {
      const list = await invoke<WorkflowTrainingCandidate[]>(
        "list_workflow_training_candidates",
      );
      setTrainingCount(list.length);
    } catch {
      // Non-fatal — count just stays stale.
    }
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [cfgNext, list] = await Promise.all([
        invoke<WorkflowConfig>("get_workflow_config"),
        invoke<Workflow[]>("list_workflows"),
      ]);
      setCfg(cfgNext);
      setWorkflows(list);
      void refreshTraining();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [refreshTraining]);

  const clearTraining = async () => {
    if (!confirm(t("settings.workflows.training.confirmClear"))) return;
    try {
      await invoke("clear_workflow_training");
      // Broadcast so `App`-level listeners repopulate their
      // training-id set — the inbox TRAIN badges vanish instantly.
      window.dispatchEvent(new CustomEvent("cm:training:changed"));
      await refreshTraining();
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
    // Keep the top-level training-count banner in sync when the
    // set changes from elsewhere (Reader hotkey, training-accept in
    // a RulesBlock, bulk clear).
    const onChanged = () => void refreshTraining();
    window.addEventListener("cm:training:changed", onChanged);
    return () => {
      window.removeEventListener("cm:training:changed", onChanged);
    };
  }, [refresh, refreshTraining]);

  const saveCfg = async (next: WorkflowConfig) => {
    try {
      await invoke("set_workflow_config", { config: next });
      setCfg(next);
    } catch (e) {
      setError(String(e));
    }
  };

  const afterEditorSave = (_saved: Workflow) => {
    setEditing(null);
    void refresh();
  };

  const accountsList = accounts;

  const deleteWorkflow = async (id: string) => {
    if (!confirm(t("settings.workflows.confirmDelete"))) return;
    try {
      await invoke("delete_workflow", { workflowId: id });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-6">
      <header>
        <h2 className="text-base font-semibold">
          {t("settings.workflows.title")}
        </h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.workflows.hint")}
        </p>
      </header>

      {error && <ErrorBanner message={error} />}

      {cfg && (
        <ConfigSection
          cfg={cfg}
          onChange={(next) => {
            setCfg(next);
            void saveCfg(next);
          }}
        />
      )}

      {trainingCount > 0 && (
        <div
          className="flex items-center justify-between rounded-md border px-3 py-2 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "rgba(59,130,246,0.08)",
          }}
        >
          <span style={{ color: "var(--fg-base)" }}>
            🎓{" "}
            {t("settings.workflows.training.summary", {
              count: trainingCount,
            })}
          </span>
          <button
            type="button"
            onClick={() => void clearTraining()}
            className="rounded-md border px-2 py-0.5 text-[11px]"
            style={{
              borderColor: "var(--border-soft)",
              color: "var(--fg-muted)",
            }}
          >
            {t("settings.workflows.training.clearAll")}
          </button>
        </div>
      )}

      <section className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold">
            {t("settings.workflows.listTitle")}
          </h3>
          <button
            type="button"
            onClick={() => setEditing("new")}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--accent)",
            }}
          >
            + {t("settings.workflows.addNew")}
          </button>
        </div>

        {loading && (
          <div className="text-xs" style={{ color: "var(--fg-subtle)" }}>
            {t("common.loading")}
          </div>
        )}

        {!loading && workflows.length === 0 && (
          <div
            className="rounded-md border px-4 py-6 text-center text-sm"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
              color: "var(--fg-muted)",
            }}
          >
            {t("settings.workflows.emptyList")}
          </div>
        )}

        {!loading && workflows.length > 0 && (
          <ul
            className="flex flex-col overflow-hidden rounded-md border"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-base)",
            }}
          >
            {workflows.map((w, i) => (
              <li
                key={w.id}
                className={`flex items-center gap-3 px-3 py-2 text-sm ${
                  i === 0 ? "" : "border-t"
                }`}
                style={{ borderColor: "var(--border-soft)" }}
              >
                <div className="flex min-w-0 flex-1 flex-col">
                  <div className="flex items-center gap-2">
                    <span
                      className="truncate"
                      style={{
                        color: w.enabled ? "var(--fg-base)" : "var(--fg-muted)",
                        textDecoration: w.enabled ? "none" : "line-through",
                      }}
                    >
                      {w.name}
                    </span>
                    {w.hotkey && (
                      <kbd
                        className="rounded border px-1.5 py-0.5 text-[10px]"
                        style={{
                          borderColor: "var(--border-soft)",
                          color: "var(--fg-subtle)",
                        }}
                      >
                        {w.hotkey}
                      </kbd>
                    )}
                  </div>
                  <span
                    className="text-[11px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {t("settings.workflows.stepsCount", {
                      count: w.steps.length,
                    })}
                    {w.runCount > 0 && (
                      <> · {t("settings.workflows.runCount", { count: w.runCount })}</>
                    )}
                  </span>
                </div>
                <button
                  type="button"
                  onClick={() => setEditing(w)}
                  className="rounded border px-2 py-0.5 text-xs"
                  style={{
                    borderColor: "var(--border-soft)",
                    color: "var(--fg-base)",
                  }}
                >
                  {t("settings.workflows.edit")}
                </button>
                <button
                  type="button"
                  onClick={() => void deleteWorkflow(w.id)}
                  className="rounded border px-2 py-0.5 text-xs"
                  style={{
                    borderColor: "var(--border-soft)",
                    color: "#ef4444",
                  }}
                >
                  {t("settings.workflows.delete")}
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <RuleAuditLog />

      {editing && cfg && (
        <WorkflowEditor
          initial={editing === "new" ? null : editing}
          cfg={cfg}
          accounts={accountsList}
          onClose={() => setEditing(null)}
          onSaved={afterEditorSave}
          onOpenAiSettings={onOpenAiSettings}
        />
      )}
    </div>
  );
}

/**
 * Globaler Audit-Log-Block am Fuß der Workflow-Settings. Zeigt die
 * letzten 200 Einträge aus `workflow_rule_actions_log` — einer pro
 * Sweep-Versuch. Default eingeklappt, weil's Ergänzungs-Information
 * ist; wer's wissen will, klappt's auf.
 *
 * Bewusst kein per-Workflow-Filter: die Tabelle ist klein, der User
 * orientiert sich am Regel-Namen und scrollt. Wenn der Log mal hunderte
 * Einträge umfasst, lohnt's sich einen Filter nachzuziehen.
 */
function RuleAuditLog() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [entries, setEntries] = useState<RuleActionLogEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sweepRunning, setSweepRunning] = useState(false);
  const [sweepSummary, setSweepSummary] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await invoke<RuleActionLogEntry[]>(
        "list_rule_action_log",
        { limit: 200 },
      );
      setEntries(list);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  /** "Jetzt auf Posteingang anwenden": triggert einen Sweep-Run sofort,
   *  ohne auf den nächsten Sync zu warten. Nützlich nach einem Backfill
   *  oder zum Aufräumen, wenn der User nicht weitere Stunden warten will. */
  const runSweepNow = useCallback(async () => {
    setSweepRunning(true);
    setSweepSummary(null);
    setError(null);
    try {
      const r = await invoke<{
        ok: number;
        skipped: number;
        failed: number;
      }>("run_rule_sweep_now");
      setSweepSummary(
        t("settings.workflows.auditLog.applyDoneSummary", {
          ok: r.ok,
          skipped: r.skipped,
          failed: r.failed,
        }),
      );
      // Nach dem Sweep den Log-Tab automatisch öffnen + neu laden, damit
      // der User direkt sieht, was passiert ist.
      setOpen(true);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setSweepRunning(false);
    }
  }, [t, refresh]);

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  return (
    <section
      className="flex flex-col gap-2 rounded-lg border p-4"
      style={{
        borderColor: "var(--border-base)",
        background: "var(--bg-panel)",
      }}
    >
      <header className="flex items-center justify-between gap-2 flex-wrap">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="flex items-center gap-2 text-left"
        >
          <span className="text-sm font-semibold">
            {t("settings.workflows.auditLog.title")}
          </span>
          <span
            className="text-[11px]"
            style={{ color: "var(--fg-subtle)" }}
            aria-hidden
          >
            {open ? "▼" : "▶"}
          </span>
        </button>
        <div className="flex items-center gap-2">
          {/* "Jetzt anwenden" ist immer sichtbar (auch bei
              eingeklapptem Audit-Log), weil's der häufigere User-Wunsch
              ist als der Refresh-Button. Sweep läuft async im Backend
              — wir zeigen währenddessen den Spinner-Text und ploppen
              den Audit-Log automatisch auf wenn der Sweep fertig ist,
              damit der User die frischen Einträge sieht. */}
          <button
            type="button"
            onClick={() => void runSweepNow()}
            disabled={sweepRunning}
            className="rounded border px-2 py-0.5 text-xs"
            style={{
              borderColor: "var(--border-base)",
              background: sweepRunning ? "transparent" : "var(--accent)",
              color: sweepRunning ? "var(--fg-muted)" : "var(--bg-panel)",
              opacity: sweepRunning ? 0.7 : 1,
            }}
          >
            {sweepRunning
              ? t("settings.workflows.auditLog.applyRunning")
              : t("settings.workflows.auditLog.applyNow")}
          </button>
          {open && (
            <button
              type="button"
              onClick={() => void refresh()}
              disabled={loading}
              className="rounded border px-2 py-0.5 text-xs"
              style={{
                borderColor: "var(--border-soft)",
                color: "var(--fg-base)",
                opacity: loading ? 0.6 : 1,
              }}
            >
              {t("settings.workflows.auditLog.refresh")}
            </button>
          )}
        </div>
      </header>
      {sweepSummary && (
        <div
          className="rounded-md border px-3 py-2 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-muted)",
          }}
        >
          {sweepSummary}
        </div>
      )}
      {open && (
        <>
          <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t("settings.workflows.auditLog.subtitle")}
          </p>
          {error && <ErrorBanner message={error} />}
          {!error && entries.length === 0 && !loading && (
            <div
              className="rounded-md border-2 border-dashed px-4 py-6 text-center text-xs"
              style={{
                borderColor: "var(--border-soft)",
                color: "var(--fg-subtle)",
              }}
            >
              {t("settings.workflows.auditLog.empty")}
            </div>
          )}
          {entries.length > 0 && (
            <ul
              className="flex max-h-96 flex-col gap-0 overflow-y-auto rounded-md border"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-base)",
              }}
            >
              {entries.map((e, i) => (
                <li
                  key={e.id}
                  className={`flex flex-col gap-0.5 px-3 py-2 text-xs ${
                    i === 0 ? "" : "border-t"
                  }`}
                  style={{ borderColor: "var(--border-soft)" }}
                >
                  <div className="flex items-center gap-2 flex-wrap">
                    <span
                      className="rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wider"
                      style={auditResultBadgeStyle(e.result)}
                    >
                      {t(`settings.workflows.auditLog.result${capitalize(e.result)}`)}
                    </span>
                    <span
                      className="text-[11px] font-medium"
                      style={{ color: "var(--fg-base)" }}
                    >
                      {e.ruleName || "(Regel ohne Namen)"}
                    </span>
                    <span
                      className="text-[10px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      {auditActionLabel(e, t)}
                    </span>
                    <span
                      className="ml-auto text-[10px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      {formatAuditTimestamp(e.ranAt)}
                    </span>
                  </div>
                  <span style={{ color: "var(--fg-base)" }}>
                    {e.subjectSnapshot || "(kein Betreff)"}
                  </span>
                  <span
                    className="text-[11px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {e.senderSnapshot}
                    {e.errorMessage && (
                      <>
                        {" · "}
                        <span style={{ color: "#ef4444" }}>
                          {e.errorMessage}
                        </span>
                      </>
                    )}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </section>
  );
}

function auditResultBadgeStyle(result: RuleActionLogEntry["result"]): {
  background: string;
  color: string;
} {
  switch (result) {
    case "ok":
      return { background: "rgba(34,197,94,0.15)", color: "#22c55e" };
    case "skipped":
      return { background: "rgba(168,162,158,0.18)", color: "#a8a29e" };
    case "failed":
      return { background: "rgba(239,68,68,0.15)", color: "#ef4444" };
  }
}

function auditActionLabel(
  e: RuleActionLogEntry,
  t: (k: string, opts?: Record<string, unknown>) => string,
): string {
  switch (e.action) {
    case "archive":
      return t("settings.workflows.auditLog.actionArchive");
    case "delete":
      return t("settings.workflows.auditLog.actionDelete");
    case "move":
      return t("settings.workflows.auditLog.actionMove", {
        dest: e.actionDest ?? "?",
      });
    case "run_workflow":
      return t("settings.workflows.auditLog.actionRunWorkflow");
  }
}

function capitalize(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

function formatAuditTimestamp(iso: string): string {
  // Lokale Zeit, kompakt — der Audit-Log ist eher für "heute/gestern"-
  // Lookups gedacht als für historische Recherche. Falls das anders
  // wird, hier auf ein relatives Format ("vor 5 Min") schwenken.
  const d = new Date(iso);
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) {
    return d.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });
  }
  return d.toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function ErrorBanner({ message }: { message: string }) {
  return (
    <div
      className="rounded-md border px-3 py-2 text-xs"
      style={{
        borderColor: "#ef4444",
        background: "rgba(239,68,68,0.08)",
        color: "#ef4444",
      }}
    >
      {message}
    </div>
  );
}

// ─── config section ─────────────────────────────────────────────────

function ConfigSection({
  cfg,
  onChange,
}: {
  cfg: WorkflowConfig;
  onChange: (next: WorkflowConfig) => void;
}) {
  const { t } = useTranslation();
  const [pyDraft, setPyDraft] = useState(cfg.pythonBin);

  const pickDir = async () => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title: t("settings.workflows.scriptDirPick"),
      });
      if (typeof picked === "string" && picked.length > 0) {
        onChange({ ...cfg, scriptDir: picked });
      }
    } catch {
      // User cancelled or plugin denied — no-op.
    }
  };

  return (
    <section
      className="flex flex-col gap-3 rounded-md border p-3"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-base)",
      }}
    >
      <h3 className="text-sm font-semibold">
        {t("settings.workflows.configTitle")}
      </h3>

      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.scriptDir")}
        </label>
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={cfg.scriptDir}
            onChange={(e) => onChange({ ...cfg, scriptDir: e.target.value })}
            placeholder={t("settings.workflows.scriptDirPlaceholder")}
            className="flex-1 rounded-md border px-2 py-1 text-sm"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
            }}
          />
          <button
            type="button"
            onClick={() => void pickDir()}
            className="rounded-md border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
          >
            {t("settings.workflows.browse")}
          </button>
        </div>
        <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.scriptDirHint")}
        </p>
      </div>

      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.pythonBin")}
        </label>
        <input
          type="text"
          value={pyDraft}
          onChange={(e) => setPyDraft(e.target.value)}
          onBlur={() => {
            if (pyDraft !== cfg.pythonBin) {
              onChange({ ...cfg, pythonBin: pyDraft });
            }
          }}
          placeholder="py"
          className="rounded-md border px-2 py-1 text-sm"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-panel)",
            color: "var(--fg-base)",
          }}
        />
        <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.pythonBinHint")}
        </p>
      </div>
    </section>
  );
}

// ─── workflow editor (modal) ────────────────────────────────────────

function emptyDraft(): WorkflowDraft {
  return {
    name: "",
    hotkey: null,
    steps: [],
    enabled: true,
    archiveAfterSuccess: false,
  };
}

function WorkflowEditor({
  initial,
  cfg,
  accounts,
  onClose,
  onSaved,
  onOpenAiSettings,
}: {
  initial: Workflow | null;
  cfg: WorkflowConfig;
  accounts: AccountSummary[];
  onClose: () => void;
  onSaved: (w: Workflow) => void;
  onOpenAiSettings?: () => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<WorkflowDraft>(() =>
    initial
      ? {
          name: initial.name,
          hotkey: initial.hotkey,
          steps: initial.steps,
          enabled: initial.enabled,
          archiveAfterSuccess: initial.archiveAfterSuccess,
        }
      : emptyDraft(),
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const saved = initial
        ? await invoke<Workflow>("update_workflow", {
            workflowId: initial.id,
            draft,
          })
        : await invoke<Workflow>("add_workflow", { draft });
      onSaved(saved);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const addStep = (type: WorkflowStep["type"]) => {
    let fresh: WorkflowStep;
    switch (type) {
      case "saveAttachments":
        fresh = { type, targetDir: "", filter: null };
        break;
      case "saveBody":
        fresh = { type, path: "", format: "md" };
        break;
      case "runScript":
        fresh = { type, script: "", parameters: [] };
        break;
    }
    setDraft({ ...draft, steps: [...draft.steps, fresh] });
  };

  const updateStep = (i: number, next: WorkflowStep) => {
    const steps = [...draft.steps];
    steps[i] = next;
    setDraft({ ...draft, steps });
  };

  const removeStep = (i: number) => {
    const steps = draft.steps.filter((_, idx) => idx !== i);
    setDraft({ ...draft, steps });
  };

  const moveStep = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= draft.steps.length) return;
    const steps = [...draft.steps];
    [steps[i], steps[j]] = [steps[j], steps[i]];
    setDraft({ ...draft, steps });
  };

  return (
    <div
      className="fixed inset-0 z-[60] flex items-start justify-center overflow-y-auto px-4 py-[6vh]"
      style={{ background: "rgba(0,0,0,0.55)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        className="flex w-full max-w-3xl flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <header
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <h2 className="text-sm font-semibold">
            {initial
              ? t("settings.workflows.editorEdit")
              : t("settings.workflows.editorNew")}
          </h2>
          <button
            type="button"
            onClick={onClose}
            className="text-xs"
            style={{ color: "var(--fg-muted)" }}
          >
            ✕
          </button>
        </header>

        <div className="flex flex-col gap-4 overflow-y-auto px-4 py-4">
          {error && <ErrorBanner message={error} />}

          <div className="grid grid-cols-[2fr,1fr,auto] items-end gap-3">
            <div className="flex flex-col gap-1">
              <label
                className="text-xs"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("settings.workflows.name")}
              </label>
              <input
                type="text"
                value={draft.name}
                onChange={(e) =>
                  setDraft({ ...draft, name: e.target.value })
                }
                className="rounded-md border px-2 py-1 text-sm"
                style={{
                  borderColor: "var(--border-base)",
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                }}
              />
            </div>
            <div className="flex flex-col gap-1">
              <label
                className="text-xs"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("settings.workflows.hotkey")}
              </label>
              <input
                type="text"
                value={draft.hotkey ?? ""}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    hotkey: e.target.value.trim() || null,
                  })
                }
                placeholder="Ctrl+1"
                className="rounded-md border px-2 py-1 text-sm"
                style={{
                  borderColor: "var(--border-base)",
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                }}
              />
            </div>
            <label className="flex items-center gap-2 pb-1 text-xs">
              <input
                type="checkbox"
                checked={draft.enabled}
                onChange={(e) =>
                  setDraft({ ...draft, enabled: e.target.checked })
                }
              />
              <span>{t("settings.workflows.enabled")}</span>
            </label>
          </div>

          <label
            className="flex items-center gap-2 text-xs"
            style={{ color: "var(--fg-base)" }}
          >
            <input
              type="checkbox"
              checked={draft.archiveAfterSuccess}
              onChange={(e) =>
                setDraft({
                  ...draft,
                  archiveAfterSuccess: e.target.checked,
                })
              }
            />
            <span>{t("settings.workflows.archiveAfterSuccess")}</span>
            <span style={{ color: "var(--fg-subtle)" }}>
              — {t("settings.workflows.archiveAfterSuccessHint")}
            </span>
          </label>

          <div className="flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-semibold">
                {t("settings.workflows.steps")}
              </h3>
              <div className="flex gap-1">
                <AddStepButton onPick={addStep} />
              </div>
            </div>

            {draft.steps.length === 0 && (
              <div
                className="rounded-md border px-3 py-4 text-center text-xs"
                style={{
                  borderColor: "var(--border-soft)",
                  color: "var(--fg-muted)",
                }}
              >
                {t("settings.workflows.noSteps")}
              </div>
            )}

            {draft.steps.map((step, i) => (
              <StepEditor
                key={i}
                index={i}
                step={step}
                cfg={cfg}
                onChange={(next) => updateStep(i, next)}
                onRemove={() => removeStep(i)}
                onMoveUp={i > 0 ? () => moveStep(i, -1) : undefined}
                onMoveDown={
                  i < draft.steps.length - 1
                    ? () => moveStep(i, 1)
                    : undefined
                }
              />
            ))}
          </div>

          {/* Rules only attach to an already-persisted workflow — a
              rule row needs a stable workflow_id, and we don't get one
              until the workflow is saved at least once. The editor
              hides this section for brand-new drafts with a hint. */}
          {initial ? (
            <RulesBlock
              workflow={initial}
              accounts={accounts}
              onOpenAiSettings={onOpenAiSettings}
            />
          ) : (
            <div
              className="rounded-md border px-3 py-2 text-xs"
              style={{
                borderColor: "var(--border-soft)",
                color: "var(--fg-subtle)",
              }}
            >
              {t("settings.workflows.rulesAfterSave")}
            </div>
          )}
        </div>

        <footer
          className="flex items-center justify-end gap-2 border-t px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <button
            type="button"
            onClick={onClose}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-muted)",
            }}
          >
            {t("settings.workflows.cancel")}
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={saving || !draft.name.trim() || draft.steps.length === 0}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--accent)",
              color: "var(--bg-panel)",
              opacity:
                saving || !draft.name.trim() || draft.steps.length === 0
                  ? 0.6
                  : 1,
            }}
          >
            {saving
              ? t("settings.workflows.saving")
              : t("settings.workflows.save")}
          </button>
        </footer>
      </div>
    </div>
  );
}

function AddStepButton({
  onPick,
}: {
  onPick: (type: WorkflowStep["type"]) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);

  return (
    <div className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="rounded-md border px-2 py-1 text-xs"
        style={{
          borderColor: "var(--border-base)",
          color: "var(--accent)",
        }}
      >
        + {t("settings.workflows.addStep")}
      </button>
      {open && (
        <div
          className="absolute right-0 top-full z-10 mt-1 flex min-w-[220px] flex-col overflow-hidden rounded-md border shadow-lg"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-panel)",
          }}
        >
          {(
            [
              ["saveAttachments", "settings.workflows.stepTypes.saveAttachments"],
              ["saveBody", "settings.workflows.stepTypes.saveBody"],
              ["runScript", "settings.workflows.stepTypes.runScript"],
            ] as const
          ).map(([type, label]) => (
            <button
              key={type}
              type="button"
              onClick={() => {
                onPick(type);
                setOpen(false);
              }}
              className="px-3 py-1.5 text-left text-xs"
              style={{ color: "var(--fg-base)" }}
            >
              {t(label)}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// ─── step editor (dispatches on type) ───────────────────────────────

function StepEditor({
  index,
  step,
  cfg,
  onChange,
  onRemove,
  onMoveUp,
  onMoveDown,
}: {
  index: number;
  step: WorkflowStep;
  cfg: WorkflowConfig;
  onChange: (next: WorkflowStep) => void;
  onRemove: () => void;
  onMoveUp?: () => void;
  onMoveDown?: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div
      className="flex flex-col gap-2 rounded-md border p-3"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-base)",
      }}
    >
      <div className="flex items-center justify-between">
        <span
          className="text-xs font-semibold uppercase tracking-wider"
          style={{ color: "var(--fg-subtle)" }}
        >
          {t(`settings.workflows.stepTypes.${step.type}`)} · #{index + 1}
        </span>
        <div className="flex gap-1">
          <button
            type="button"
            onClick={onMoveUp}
            disabled={!onMoveUp}
            className="rounded px-1.5 py-0.5 text-xs"
            style={{
              color: onMoveUp ? "var(--fg-base)" : "var(--fg-subtle)",
              opacity: onMoveUp ? 1 : 0.4,
            }}
          >
            ↑
          </button>
          <button
            type="button"
            onClick={onMoveDown}
            disabled={!onMoveDown}
            className="rounded px-1.5 py-0.5 text-xs"
            style={{
              color: onMoveDown ? "var(--fg-base)" : "var(--fg-subtle)",
              opacity: onMoveDown ? 1 : 0.4,
            }}
          >
            ↓
          </button>
          <button
            type="button"
            onClick={onRemove}
            className="rounded px-1.5 py-0.5 text-xs"
            style={{ color: "#ef4444" }}
          >
            ✕
          </button>
        </div>
      </div>

      {step.type === "saveAttachments" && (
        <SaveAttachmentsEditor step={step} onChange={onChange} />
      )}
      {step.type === "saveBody" && (
        <SaveBodyEditor step={step} onChange={onChange} />
      )}
      {step.type === "runScript" && (
        <RunScriptEditor step={step} cfg={cfg} onChange={onChange} />
      )}
    </div>
  );
}

function SaveAttachmentsEditor({
  step,
  onChange,
}: {
  step: Extract<WorkflowStep, { type: "saveAttachments" }>;
  onChange: (next: WorkflowStep) => void;
}) {
  const { t } = useTranslation();
  const pickDir = async () => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
      });
      if (typeof picked === "string" && picked.length > 0) {
        onChange({ ...step, targetDir: picked });
      }
    } catch {
      /* cancelled */
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.step.targetDir")}
        </label>
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={step.targetDir}
            onChange={(e) =>
              onChange({ ...step, targetDir: e.target.value })
            }
            placeholder="C:\Inbox\Attachments"
            className="flex-1 rounded-md border px-2 py-1 text-sm"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
            }}
          />
          <button
            type="button"
            onClick={() => void pickDir()}
            className="rounded-md border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
          >
            …
          </button>
        </div>
      </div>
      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.step.filter")}
        </label>
        <input
          type="text"
          value={step.filter ?? ""}
          onChange={(e) =>
            onChange({
              ...step,
              filter: e.target.value.trim() || null,
            })
          }
          placeholder="*.csv"
          className="rounded-md border px-2 py-1 text-sm"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-panel)",
            color: "var(--fg-base)",
          }}
        />
      </div>
    </div>
  );
}

function SaveBodyEditor({
  step,
  onChange,
}: {
  step: Extract<WorkflowStep, { type: "saveBody" }>;
  onChange: (next: WorkflowStep) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.step.path")}
        </label>
        <input
          type="text"
          value={step.path}
          onChange={(e) => onChange({ ...step, path: e.target.value })}
          placeholder="C:\Inbox\$subject.md"
          className="rounded-md border px-2 py-1 text-sm"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-panel)",
            color: "var(--fg-base)",
          }}
        />
      </div>
      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.step.format")}
        </label>
        <div className="flex gap-3 text-xs">
          {(["md", "txt", "eml"] as const).map((f) => (
            <label key={f} className="flex items-center gap-1">
              <input
                type="radio"
                checked={step.format === f}
                onChange={() => onChange({ ...step, format: f })}
              />
              {t(`settings.workflows.step.formatOpt.${f}`)}
            </label>
          ))}
        </div>
      </div>
    </div>
  );
}

function RunScriptEditor({
  step,
  cfg,
  onChange,
}: {
  step: Extract<WorkflowStep, { type: "runScript" }>;
  cfg: WorkflowConfig;
  onChange: (next: WorkflowStep) => void;
}) {
  const { t } = useTranslation();
  const [scripts, setScripts] = useState<string[] | null>(null);
  const [scriptsError, setScriptsError] = useState<string | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [analyzeError, setAnalyzeError] = useState<string | null>(null);

  const configured = cfg.scriptDir.trim().length > 0;

  useEffect(() => {
    if (!configured) {
      setScripts(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const list = await invoke<string[]>("list_workflow_scripts");
        if (!cancelled) setScripts(list);
      } catch (e) {
        if (!cancelled) setScriptsError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [configured, cfg.scriptDir]);

  const analyze = async () => {
    if (!step.script) return;
    setAnalyzing(true);
    setAnalyzeError(null);
    try {
      const params = await invoke<ScriptParam[]>("analyze_python_script", {
        scriptName: step.script,
      });
      // Merge: keep user's existing source bindings for params that
      // still exist by `key`, so re-analyzing after a script edit
      // doesn't wipe the hand-configured bindings.
      const existing = new Map(step.parameters.map((p) => [p.key, p]));
      const merged: ScriptParam[] = params.map((fresh) => {
        const prior = existing.get(fresh.key);
        if (!prior) return fresh;
        return {
          ...fresh,
          source: prior.source,
          enabled: prior.enabled,
        };
      });
      onChange({ ...step, parameters: merged });
    } catch (e) {
      setAnalyzeError(String(e));
    } finally {
      setAnalyzing(false);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      {!configured && (
        <div
          className="rounded-md border px-3 py-2 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            color: "var(--fg-muted)",
          }}
        >
          {t("settings.workflows.step.scriptDirMissing")}
        </div>
      )}

      <div className="flex flex-col gap-1">
        <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.workflows.step.script")}
        </label>
        <div className="flex items-center gap-2">
          {scripts === null && configured && !scriptsError ? (
            <span
              className="text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              …
            </span>
          ) : scripts !== null && scripts.length > 0 ? (
            <select
              value={step.script}
              onChange={(e) =>
                onChange({ ...step, script: e.target.value })
              }
              className="flex-1 rounded-md border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
              }}
            >
              <option value="">
                {t("settings.workflows.step.pickScript")}
              </option>
              {scripts.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </select>
          ) : (
            <input
              type="text"
              value={step.script}
              onChange={(e) =>
                onChange({ ...step, script: e.target.value })
              }
              placeholder="import_csv.py"
              className="flex-1 rounded-md border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
              }}
            />
          )}
          <button
            type="button"
            onClick={() => void analyze()}
            disabled={!step.script || !configured || analyzing}
            className="rounded-md border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--accent)",
              opacity: !step.script || !configured || analyzing ? 0.5 : 1,
            }}
          >
            {analyzing
              ? t("settings.workflows.step.analyzing")
              : t("settings.workflows.step.analyze")}
          </button>
        </div>
        {scriptsError && (
          <p className="text-[11px]" style={{ color: "#ef4444" }}>
            {scriptsError}
          </p>
        )}
        {analyzeError && (
          <p className="text-[11px]" style={{ color: "#ef4444" }}>
            {analyzeError}
          </p>
        )}
        {scripts !== null && scripts.length === 0 && (
          <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t("settings.workflows.step.noScripts")}
          </p>
        )}
      </div>

      {step.parameters.length > 0 && (
        <div className="flex flex-col gap-2">
          <span
            className="text-xs font-semibold uppercase tracking-wider"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("settings.workflows.step.parameters")}
          </span>
          {step.parameters.map((p, i) => (
            <ParamRow
              key={p.key || i}
              param={p}
              onChange={(next) => {
                const params = [...step.parameters];
                params[i] = next;
                onChange({ ...step, parameters: params });
              }}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function ParamRow({
  param,
  onChange,
}: {
  param: ScriptParam;
  onChange: (next: ScriptParam) => void;
}) {
  const { t } = useTranslation();
  const setSource = (source: ParamSource) => onChange({ ...param, source });

  return (
    <div
      className="flex flex-col gap-2 rounded-md border p-2"
      style={{ borderColor: "var(--border-soft)" }}
    >
      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          checked={param.enabled}
          onChange={(e) => onChange({ ...param, enabled: e.target.checked })}
          title={t("settings.workflows.step.paramEnabled")}
        />
        <span className="text-sm font-semibold">{param.label}</span>
        <code
          className="rounded px-1 text-[11px]"
          style={{
            background: "var(--bg-hover)",
            color: "var(--fg-subtle)",
          }}
        >
          {param.cliName}
        </code>
        <span
          className="text-[10px] uppercase tracking-wider"
          style={{ color: "var(--fg-subtle)" }}
        >
          {param.kind}
        </span>
        {/* ValueType ist editierbar — der Argparse-Analyzer setzt einen
            sinnvollen Default, aber für hand-gepflegte Workflow-Skripte
            oder Korrekturen muss der User ihn umschalten können.
            Insbesondere "choice" ist die Eintrittskarte zum Choices-
            Editor weiter unten. */}
        <select
          value={param.valueType}
          onChange={(e) =>
            onChange({
              ...param,
              valueType: e.target.value as ScriptValueType,
            })
          }
          className="rounded border px-1 py-0 text-[10px] uppercase tracking-wider"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-subtle)",
          }}
          title={t("settings.workflows.step.valueType")}
        >
          <option value="string">string</option>
          <option value="number">number</option>
          <option value="boolean">boolean</option>
          <option value="choice">choice</option>
          <option value="path">path</option>
        </select>
        {param.required && (
          <span
            className="text-[10px] font-bold"
            style={{ color: "#ef4444" }}
          >
            *
          </span>
        )}
      </div>

      {param.helpText && (
        <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
          {param.helpText}
        </p>
      )}

      {param.valueType === "choice" && (
        <div className="flex flex-col gap-1">
          <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
            {t("settings.workflows.step.choices")}
          </label>
          <ChoicesChipInput
            values={param.choices}
            onChange={(next) => onChange({ ...param, choices: next })}
          />
          <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t("settings.workflows.step.choicesHint")}
          </p>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
        <label className="flex items-center gap-1">
          <input
            type="radio"
            checked={param.source.kind === "fixed"}
            onChange={() =>
              setSource({
                kind: "fixed",
                value:
                  param.source.kind === "fixed"
                    ? param.source.value
                    : param.defaultValue ?? "",
              })
            }
          />
          {t("settings.workflows.step.sourceFixed")}
        </label>
        <label className="flex items-center gap-1">
          <input
            type="radio"
            checked={param.source.kind === "template"}
            onChange={() =>
              setSource({
                kind: "template",
                var:
                  param.source.kind === "template"
                    ? param.source.var
                    : guessTemplateVar(param),
              })
            }
          />
          {t("settings.workflows.step.sourceTemplate")}
        </label>
        <label className="flex items-center gap-1">
          <input
            type="radio"
            checked={param.source.kind === "firstAttachment"}
            onChange={() =>
              setSource({
                kind: "firstAttachment",
                extension:
                  param.source.kind === "firstAttachment"
                    ? param.source.extension
                    : guessAttachmentExtension(param),
              })
            }
          />
          {t("settings.workflows.step.sourceFirstAttachment")}
        </label>
        <label className="flex items-center gap-1">
          <input
            type="radio"
            checked={param.source.kind === "prompt"}
            onChange={() =>
              setSource({
                kind: "prompt",
                defaultTemplate:
                  param.source.kind === "prompt"
                    ? param.source.defaultTemplate
                    : null,
              })
            }
          />
          {t("settings.workflows.step.sourcePrompt")}
        </label>
      </div>

      {param.source.kind === "fixed" && param.kind !== "flag" && (
        <div className="flex flex-col gap-1">
          {param.valueType === "choice" && param.choices.length > 0 ? (
            <select
              value={param.source.value}
              onChange={(e) =>
                setSource({ kind: "fixed", value: e.target.value })
              }
              className="rounded-md border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
              }}
            >
              <option value="">—</option>
              {param.choices.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          ) : (
            <input
              type={
                param.valueType === "number"
                  ? "number"
                  : "text"
              }
              value={param.source.value}
              onChange={(e) =>
                setSource({ kind: "fixed", value: e.target.value })
              }
              placeholder={param.defaultValue ?? ""}
              className="rounded-md border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
              }}
            />
          )}
        </div>
      )}

      {param.source.kind === "fixed" && param.kind === "flag" && (
        <label className="flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            checked={["1", "true", "yes", "on"].includes(
              param.source.value.trim().toLowerCase(),
            )}
            onChange={(e) =>
              setSource({
                kind: "fixed",
                value: e.target.checked ? "true" : "false",
              })
            }
          />
          {t("settings.workflows.step.flagOn")}
        </label>
      )}

      {param.source.kind === "template" && (
        <div className="flex flex-col gap-1">
          <select
            value={param.source.var}
            onChange={(e) =>
              setSource({ kind: "template", var: e.target.value })
            }
            className="rounded-md border px-2 py-1 text-sm"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
            }}
          >
            {WORKFLOW_TEMPLATE_VARS.map((v) => (
              <option key={v} value={v}>
                ${v}
              </option>
            ))}
          </select>
          <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t(`settings.workflows.vars.${param.source.var}`, {
              defaultValue: "",
            })}
          </p>
        </div>
      )}

      {param.source.kind === "firstAttachment" && (
        <div className="flex flex-col gap-1">
          {/* Datalist-driven input: the dropdown suggests the common
              set, but the user can type any other suffix (say "docx"
              or "ics") if their workflow needs it. Keeps the menu
              tight without locking anything out. */}
          <input
            list="workflow-attachment-ext-list"
            type="text"
            value={param.source.extension}
            onChange={(e) =>
              setSource({
                kind: "firstAttachment",
                extension: e.target.value
                  .trim()
                  .replace(/^\.+/, "")
                  .toLowerCase(),
              })
            }
            placeholder="csv"
            className="rounded-md border px-2 py-1 text-sm"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
            }}
          />
          <datalist id="workflow-attachment-ext-list">
            {WORKFLOW_ATTACHMENT_EXTENSIONS.map((ext) => (
              <option key={ext} value={ext} />
            ))}
          </datalist>
          <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t("settings.workflows.step.firstAttachmentHint")}
          </p>
        </div>
      )}

      {param.source.kind === "prompt" && (
        <div className="flex flex-col gap-1">
          {/* Default-Template optional. Wenn leer, wird beim Apply
              zwingend der Pre-Apply-Dialog gezeigt; ein Wert wird im
              Dialog vorgeblendet (kann Literal oder $var sein). Bei
              Auto-Trigger ohne Default schlägt der Run mit Fehler auf
              — der Workflow-Run-Result-Dialog macht das transparent. */}
          <input
            type="text"
            value={param.source.defaultTemplate ?? ""}
            onChange={(e) =>
              setSource({
                kind: "prompt",
                defaultTemplate: e.target.value || null,
              })
            }
            placeholder={t("settings.workflows.step.promptDefaultPlaceholder")}
            className="rounded-md border px-2 py-1 text-sm"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
            }}
          />
          <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t("settings.workflows.step.promptHint")}
          </p>
        </div>
      )}
    </div>
  );
}

/**
 * Best-effort heuristic picking a template var when the user flips a
 * parameter's source to "Template". Matches the variable naming
 * conventions we expose — the user can always change it afterwards.
 */
function guessTemplateVar(p: ScriptParam): string {
  const hint = `${p.key} ${p.cliName}`.toLowerCase();
  if (hint.includes("attach") || hint.includes("dir")) return "attachments_dir";
  if (hint.includes("body") || hint.includes("markdown") || hint.includes("md"))
    return "body_md";
  if (hint.includes("subject")) return "subject";
  if (hint.includes("from") || hint.includes("sender")) return "from";
  if (hint.includes("date")) return "date";
  return WORKFLOW_TEMPLATE_VARS[0];
}

/**
 * Same idea for the FirstAttachment source: if the param name hints
 * at a file type we know how to match, preselect that extension. Any
 * miss just defaults to `csv` — the user tweaks it in the input.
 */
function guessAttachmentExtension(p: ScriptParam): string {
  const hint = `${p.key} ${p.cliName}`.toLowerCase();
  for (const ext of WORKFLOW_ATTACHMENT_EXTENSIONS) {
    if (hint.includes(ext)) return ext;
  }
  return "csv";
}

// ─── auto-trigger rules ─────────────────────────────────────────────

function RulesBlock({
  workflow,
  accounts,
  onOpenAiSettings,
}: {
  workflow: Workflow;
  accounts: AccountSummary[];
  onOpenAiSettings?: () => void;
}) {
  const { t } = useTranslation();
  const workflowId = workflow.id;
  const [rules, setRules] = useState<WorkflowRule[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Editing state carries three shapes:
  //   - null        → panel collapsed
  //   - "new"       → blank editor
  //   - Rule        → edit existing rule
  //   - { draft }   → new editor prefilled from training proposal
  const [editing, setEditing] = useState<
    WorkflowRule | "new" | { draft: WorkflowRuleDraft } | null
  >(null);
  const [trainingOpen, setTrainingOpen] = useState(false);
  const [trainingCandidates, setTrainingCandidates] = useState<
    WorkflowTrainingCandidate[]
  >([]);
  // Latched when the editor is opened from the training flow
  // (either Accept-direct or Edit-then-Save). On the next successful
  // rule save we'll clear the training candidates — the user just
  // consumed them, no reason to keep them around.
  const [pendingTrainingClear, setPendingTrainingClear] = useState(false);

  const consumeTrainingCandidates = useCallback(async () => {
    try {
      await invoke("clear_workflow_training");
      window.dispatchEvent(new CustomEvent("cm:training:changed"));
    } catch {
      // Non-fatal — user can still manually clear from the
      // "Alle entfernen" button in the settings header.
    }
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await invoke<WorkflowRule[]>(
        "list_workflow_rules_for_workflow",
        { workflowId },
      );
      setRules(list);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [workflowId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const toggleEnabled = async (rule: WorkflowRule, enabled: boolean) => {
    // Optimistic flip so the checkbox reacts instantly.
    setRules((rs) =>
      rs.map((r) => (r.id === rule.id ? { ...r, enabled } : r)),
    );
    try {
      await invoke("set_workflow_rule_enabled", {
        ruleId: rule.id,
        enabled,
      });
    } catch (e) {
      setError(String(e));
      setRules((rs) =>
        rs.map((r) =>
          r.id === rule.id ? { ...r, enabled: !enabled } : r,
        ),
      );
    }
  };

  const onDelete = async (rule: WorkflowRule) => {
    if (!confirm(t("settings.workflows.confirmDeleteRule"))) return;
    try {
      await invoke("delete_workflow_rule", { ruleId: rule.id });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold">
          {t("settings.workflows.rulesTitle")}
        </h3>
        <div className="flex gap-1">
          <button
            type="button"
            onClick={async () => {
              try {
                const list = await invoke<WorkflowTrainingCandidate[]>(
                  "list_workflow_training_candidates",
                );
                setTrainingCandidates(list);
                setTrainingOpen(true);
              } catch (e) {
                setError(String(e));
              }
            }}
            className="rounded-md border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
            title={t("settings.workflows.training.buttonTooltip")}
          >
            🎓 {t("settings.workflows.training.button")}
          </button>
          <button
            type="button"
            onClick={() => setEditing("new")}
            className="rounded-md border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--accent)",
            }}
          >
            + {t("settings.workflows.addRule")}
          </button>
        </div>
      </div>

      <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
        {t("settings.workflows.rulesHint")}
      </p>

      {error && <ErrorBanner message={error} />}

      {loading && (
        <div className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("common.loading")}
        </div>
      )}

      {!loading && rules.length === 0 && (
        <div
          className="rounded-md border px-3 py-2 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            color: "var(--fg-muted)",
          }}
        >
          {t("settings.workflows.noRules")}
        </div>
      )}

      {!loading && rules.length > 0 && (
        <ul
          className="flex flex-col overflow-hidden rounded-md border"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-base)",
          }}
        >
          {rules.map((r, i) => (
            <li
              key={r.id}
              className={`flex items-start gap-3 px-3 py-2 text-xs ${
                i === 0 ? "" : "border-t"
              }`}
              style={{ borderColor: "var(--border-soft)" }}
            >
              <input
                type="checkbox"
                checked={r.enabled}
                onChange={(e) => void toggleEnabled(r, e.target.checked)}
                className="mt-0.5 h-3.5 w-3.5 cursor-pointer"
              />
              <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                <div className="flex items-center gap-2 flex-wrap">
                  {/* Action-Badge — RunWorkflow nutzt weiterhin den
                      Mode-Tone (auto = rot, confirm = blau). Direkt-
                      Aktionen kriegen einen Aktion-spezifischen Tone
                      damit man im Listing auf einen Blick sieht, was
                      die Regel macht. */}
                  <span
                    className="rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wider"
                    style={ruleActionBadgeStyle(r)}
                  >
                    {ruleActionLabel(r, t)}
                  </span>
                  {r.name && (
                    <span
                      className="text-[11px] font-medium"
                      style={{ color: "var(--fg-base)" }}
                    >
                      {r.name}
                    </span>
                  )}
                  {r.dryRun && (
                    <span
                      className="rounded px-1.5 py-0.5 text-[10px]"
                      style={{
                        background: "rgba(168,85,247,0.15)",
                        color: "#a855f7",
                      }}
                      title={t("settings.workflows.rule.dryRunHint")}
                    >
                      {t("settings.workflows.rule.dryRun")}
                    </span>
                  )}
                  {r.delayMinutes > 0 && (
                    <span
                      className="text-[10px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      · {formatDelayShort(r.delayMinutes)}
                    </span>
                  )}
                  {r.accountId && (
                    <span
                      className="text-[10px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      ·{" "}
                      {accounts.find((a) => a.id === r.accountId)
                        ?.displayName ?? "?"}
                    </span>
                  )}
                  {r.folderName && (
                    <span
                      className="text-[10px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      · {r.folderName}
                    </span>
                  )}
                  {r.hitCount > 0 && (
                    <span
                      className="text-[10px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      ·{" "}
                      {t("settings.workflows.rule.hits", {
                        count: r.hitCount,
                      })}
                    </span>
                  )}
                </div>
                <span style={{ color: "var(--fg-base)" }}>
                  {r.predicates
                    .map((p) => formatPredicateInline(p, t))
                    .join(" · ")}
                </span>
              </div>
              <button
                type="button"
                onClick={() => setEditing(r)}
                className="rounded border px-2 py-0.5 text-[11px]"
                style={{
                  borderColor: "var(--border-soft)",
                  color: "var(--fg-base)",
                }}
              >
                {t("settings.workflows.edit")}
              </button>
              <button
                type="button"
                onClick={() => void onDelete(r)}
                className="rounded border px-2 py-0.5 text-[11px]"
                style={{
                  borderColor: "var(--border-soft)",
                  color: "#ef4444",
                }}
              >
                {t("settings.workflows.delete")}
              </button>
            </li>
          ))}
        </ul>
      )}

      {editing && (
        <RuleEditor
          workflowId={workflowId}
          initial={
            editing === "new" || "draft" in editing ? null : editing
          }
          presetDraft={
            editing !== "new" && editing !== null && "draft" in editing
              ? editing.draft
              : null
          }
          accounts={accounts}
          onClose={() => {
            setEditing(null);
            setPendingTrainingClear(false);
          }}
          onSaved={() => {
            setEditing(null);
            // If the editor was opened from a pi-training "edit"
            // path, the user just consumed the current candidate
            // set by saving a rule from it. Clear so the next run
            // starts fresh.
            if (pendingTrainingClear) {
              setPendingTrainingClear(false);
              void consumeTrainingCandidates();
            }
            void refresh();
          }}
        />
      )}

      {trainingOpen && (
        <WorkflowTrainingDialog
          workflow={workflow}
          candidates={trainingCandidates}
          accounts={accounts}
          onOpenAiSettings={
            onOpenAiSettings
              ? () => {
                  // Close the training dialog first so the user
                  // doesn't bounce into a broken modal stack.
                  setTrainingOpen(false);
                  onOpenAiSettings();
                }
              : undefined
          }
          onClose={() => setTrainingOpen(false)}
          onRuleCreated={() => {
            setTrainingOpen(false);
            // Direct Accept path — clear candidates and refresh the
            // list. No need to set `pendingTrainingClear` since
            // we're not going through the RuleEditor.
            void consumeTrainingCandidates();
            void refresh();
          }}
          onEditDraft={(draft) => {
            setTrainingOpen(false);
            // Remember to clear after the RuleEditor saves — the
            // consumer flow continues there.
            setPendingTrainingClear(true);
            setEditing({ draft });
          }}
        />
      )}
    </div>
  );
}

/** Badge-Tone pro Action-Variante. RunWorkflow erbt den Mode-Tone
 *  (auto = rot, confirm = blau, vertraute Schmerzlinderung).
 *  Direkt-Aktionen haben jeweils eigene Töne damit das Listing auf
 *  einen Blick spricht. */
function ruleActionBadgeStyle(r: WorkflowRule): {
  background: string;
  color: string;
} {
  if (r.action === "run_workflow") {
    return r.mode === "auto"
      ? { background: "rgba(239,68,68,0.15)", color: "#ef4444" }
      : { background: "rgba(59,130,246,0.15)", color: "#3b82f6" };
  }
  switch (r.action) {
    case "archive":
      return { background: "rgba(34,197,94,0.15)", color: "#22c55e" };
    case "delete":
      return { background: "rgba(239,68,68,0.15)", color: "#ef4444" };
    case "move":
      return { background: "rgba(245,158,11,0.15)", color: "#f59e0b" };
  }
}

function ruleActionLabel(
  r: WorkflowRule,
  t: (k: string, opts?: Record<string, unknown>) => string,
): string {
  switch (r.action) {
    case "run_workflow":
      return t(`settings.workflows.mode.${r.mode}`);
    case "archive":
      return t("settings.workflows.rule.actionArchive");
    case "delete":
      return t("settings.workflows.rule.actionDelete");
    case "move":
      return r.actionDest
        ? t("settings.workflows.auditLog.actionMove", { dest: r.actionDest })
        : t("settings.workflows.rule.actionMove");
  }
}

function formatPredicateInline(
  p: RulePredicate,
  t: (k: string, opts?: Record<string, unknown>) => string,
): string {
  switch (p.kind) {
    case "fromEmail":
      return `${t("settings.workflows.predicate.fromEmail")} = ${p.value}`;
    case "fromDomain":
      return `${t("settings.workflows.predicate.fromDomain")} = ${p.value}`;
    case "fromDomainIn":
      // Summary for the list view: truncate overly long lists so
      // the row stays readable; full list still visible in editor.
      if (p.values.length <= 3) {
        return `${t("settings.workflows.predicate.fromDomainIn")} ∈ {${p.values.join(", ")}}`;
      }
      return `${t("settings.workflows.predicate.fromDomainIn")} ∈ {${p.values.slice(0, 2).join(", ")}, +${p.values.length - 2}}`;
    case "subjectContains":
      return `${t("settings.workflows.predicate.subjectContains")} "${p.value}"`;
    case "hasAttachmentExtension":
      return `${t("settings.workflows.predicate.hasAttachmentExtension")} .${p.extension}`;
  }
}

function RuleEditor({
  workflowId,
  initial,
  presetDraft,
  accounts,
  onClose,
  onSaved,
}: {
  workflowId: string;
  initial: WorkflowRule | null;
  /** Pre-filled draft when the editor is opened from the training
   *  flow ("edit pi's proposal before saving"). Takes precedence
   *  over the empty-new-rule path when `initial` is null. */
  presetDraft?: WorkflowRuleDraft | null;
  accounts: AccountSummary[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<WorkflowRuleDraft>(() => {
    if (initial) {
      return {
        name: initial.name,
        workflowId,
        accountId: initial.accountId,
        folderName: initial.folderName,
        predicates: initial.predicates,
        mode: initial.mode,
        action: initial.action,
        actionDest: initial.actionDest,
        delayMinutes: initial.delayMinutes,
        dryRun: initial.dryRun,
        enabled: initial.enabled,
      };
    }
    if (presetDraft) {
      // pi proposal handed in — use it verbatim, workflowId enforced
      // to the one this editor is scoped to in case pi got creative.
      return { ...presetDraft, workflowId };
    }
    return {
      // Defaults für eine neue Rule, die im Workflow-Editor angelegt
      // wird: weiterhin Workflow-gebunden, sofortige Ausführung, kein
      // Trockenmodus — entspricht dem v1-Verhalten dieses Editors.
      // Die Direkt-Aktionen (archive/delete/move) kriegen einen eigenen
      // Editor in einer späteren UI-Iteration; hier bleibt's bei
      // run_workflow.
      name: "",
      workflowId,
      accountId: null,
      folderName: null,
      predicates: [{ kind: "fromEmail", value: "" }],
      mode: "confirm",
      action: "run_workflow",
      actionDest: null,
      delayMinutes: 0,
      dryRun: false,
      enabled: true,
    };
  });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Folder suggestions for the current account. Populated on demand
  // so we don't thrash the IMAP folder cache when the editor opens
  // without an account scope. Empty array once loaded ⇒ no datalist
  // suggestions but the free-text input still works.
  const [folderSuggestions, setFolderSuggestions] = useState<string[]>([]);
  // Mehrstufiger Save-Flow für neue Regeln: nach erfolgreichem Save
  // bietet der Editor an, die Regel direkt auf bestehende Inbox-Mails
  // anzuwenden (Backfill). User kann ablehnen oder bestätigen; bei
  // Bestätigung läuft `apply_workflow_rule_to_existing` und wir zeigen
  // das Ergebnis als Toast-artiger Hinweis. Update-Pfad überspringt
  // diesen Schritt — wer eine Regel editiert, will nicht zwingend
  // alle Treffer rückwirkend retaggen.
  type Step =
    | { kind: "edit" }
    | { kind: "backfillPrompt"; ruleId: string }
    | { kind: "backfillRunning" }
    | { kind: "backfillDone"; affected: number };
  const [step, setStep] = useState<Step>({ kind: "edit" });

  useEffect(() => {
    if (!draft.accountId) {
      setFolderSuggestions([]);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const folders = await invoke<FolderSummary[]>(
          "list_account_folders",
          { accountId: draft.accountId },
        );
        if (!cancelled) {
          setFolderSuggestions(folders.map((f) => f.name));
        }
      } catch {
        if (!cancelled) setFolderSuggestions([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [draft.accountId]);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      if (initial) {
        await invoke("update_workflow_rule", {
          ruleId: initial.id,
          draft,
        });
        onSaved();
      } else {
        const created = await invoke<WorkflowRule>("add_workflow_rule", {
          draft,
        });
        // Editor bleibt offen, schwenkt auf Backfill-Frage. onSaved()
        // ruft refresh() im Parent — wir verzögern das auf den Close
        // nach Schritt 3, damit die Liste erst aktualisiert wird wenn
        // der Backfill-Flow abgeschlossen ist (sonst flackern doppelte
        // Refreshes durch den Parent).
        setStep({ kind: "backfillPrompt", ruleId: created.id });
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  /** Backfill ausführen — taggt bestehende passende Mails mit dem
   *  ScheduledActionTag der gerade gespeicherten Regel. Tatsächliche
   *  Action passiert beim nächsten Sweep-Tick. */
  const runBackfill = async (ruleId: string) => {
    setStep({ kind: "backfillRunning" });
    setError(null);
    try {
      const affected = await invoke<number>(
        "apply_workflow_rule_to_existing",
        { ruleId },
      );
      setStep({ kind: "backfillDone", affected });
    } catch (e) {
      setError(String(e));
      // Trotzdem zur Done-Stufe schalten, damit der User eine "Schließen"-
      // Action hat — die Regel selbst ist ja gespeichert.
      setStep({ kind: "backfillDone", affected: 0 });
    }
  };

  const finishAndClose = () => {
    setStep({ kind: "edit" });
    onSaved();
  };

  const addPredicate = () => {
    setDraft({
      ...draft,
      predicates: [...draft.predicates, { kind: "fromEmail", value: "" }],
    });
  };

  const updatePredicate = (i: number, next: RulePredicate) => {
    const predicates = [...draft.predicates];
    predicates[i] = next;
    setDraft({ ...draft, predicates });
  };

  const removePredicate = (i: number) => {
    const predicates = draft.predicates.filter((_, idx) => idx !== i);
    setDraft({ ...draft, predicates });
  };

  return (
    <div
      className="fixed inset-0 z-[62] flex items-start justify-center overflow-y-auto px-4 py-[12vh]"
      style={{ background: "rgba(0,0,0,0.55)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        className="flex w-full max-w-2xl flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <header
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <h2 className="text-sm font-semibold">
            {step.kind === "edit"
              ? initial
                ? t("settings.workflows.rule.editorEdit")
                : t("settings.workflows.rule.editorNew")
              : t("settings.workflows.backfill.title")}
          </h2>
          <button
            type="button"
            onClick={
              step.kind === "backfillRunning"
                ? undefined
                : step.kind === "edit"
                  ? onClose
                  : finishAndClose
            }
            disabled={step.kind === "backfillRunning"}
            className="text-xs"
            style={{
              color: "var(--fg-muted)",
              opacity: step.kind === "backfillRunning" ? 0.4 : 1,
            }}
          >
            ✕
          </button>
        </header>

        {step.kind === "backfillPrompt" && (
          <BackfillPrompt
            ruleId={step.ruleId}
            onSkip={finishAndClose}
            onApply={runBackfill}
          />
        )}
        {step.kind === "backfillRunning" && (
          <div
            className="px-4 py-8 text-center text-sm"
            style={{ color: "var(--fg-muted)" }}
          >
            {t("settings.workflows.backfill.running")}
          </div>
        )}
        {step.kind === "backfillDone" && (
          <BackfillDone
            affected={step.affected}
            error={error}
            onClose={finishAndClose}
          />
        )}

        {step.kind === "edit" && (
        <div className="flex flex-col gap-4 px-4 py-4">
          {error && <ErrorBanner message={error} />}

          {/* Name — auf eigener Zeile, prominent. Wird im Marker-
              Tooltip an betroffenen Mails angezeigt, also lohnt's
              sich, hier einen sprechenden Namen einzutippen. */}
          <div className="flex flex-col gap-1">
            <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
              {t("settings.workflows.rule.name")}
            </label>
            <input
              type="text"
              value={draft.name}
              onChange={(e) => setDraft({ ...draft, name: e.target.value })}
              placeholder={t("settings.workflows.rule.namePlaceholder")}
              className="rounded-md border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
            />
            <span className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
              {t("settings.workflows.rule.nameHint")}
            </span>
          </div>

          <div className="grid grid-cols-[1fr,1fr] gap-3">
            <div className="flex flex-col gap-1">
              <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
                {t("settings.workflows.rule.account")}
              </label>
              <select
                value={draft.accountId ?? ""}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    accountId: e.target.value || null,
                  })
                }
                className="rounded-md border px-2 py-1 text-sm"
                style={{
                  borderColor: "var(--border-base)",
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                }}
              >
                <option value="">
                  {t("settings.workflows.rule.anyAccount")}
                </option>
                {accounts.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.displayName}
                  </option>
                ))}
              </select>
            </div>

            <div className="flex flex-col gap-1">
              <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
                {t("settings.workflows.rule.folder")}
              </label>
              {/* Free-text input with a datalist of the account's known
                  folders, so the user can pick "INBOX" quickly or
                  type a custom path. Case-sensitive match on the IMAP
                  side so we don't normalise here. */}
              <input
                list={`wf-rule-folder-list-${workflowId}`}
                type="text"
                value={draft.folderName ?? ""}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    folderName: e.target.value.trim() || null,
                  })
                }
                placeholder={t("settings.workflows.rule.anyFolder")}
                className="rounded-md border px-2 py-1 text-sm"
                style={{
                  borderColor: "var(--border-base)",
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                }}
              />
              <datalist id={`wf-rule-folder-list-${workflowId}`}>
                {folderSuggestions.map((name) => (
                  <option key={name} value={name} />
                ))}
              </datalist>
            </div>
          </div>

          {/* Action-Achse + Mode (nur für RunWorkflow relevant). Move-
              Aktion blendet zusätzlich ein Zielordner-Feld ein. */}
          <div className="grid grid-cols-[1fr,1fr] gap-3">
            <div className="flex flex-col gap-1">
              <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
                {t("settings.workflows.rule.action")}
              </label>
              <select
                value={draft.action}
                onChange={(e) => {
                  const next = e.target.value as RuleAction;
                  setDraft({
                    ...draft,
                    action: next,
                    // actionDest nur für `move` relevant — beim
                    // Wechsel auf andere Action säubern, sonst trägt
                    // der Save-Pfad einen Zombie-Wert mit.
                    actionDest: next === "move" ? draft.actionDest : null,
                    // RunWorkflow braucht zwingend eine Workflow-
                    // Bindung; die ist im aktuellen Editor an den
                    // umgebenden Workflow gepinnt. Direkt-Aktionen
                    // setzen workflowId auf null beim Save (siehe
                    // Backend `resolve_action_fields`).
                  });
                }}
                className="rounded-md border px-2 py-1 text-sm"
                style={{
                  borderColor: "var(--border-base)",
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                }}
              >
                <option value="run_workflow">
                  {t("settings.workflows.rule.actionRunWorkflow")}
                </option>
                <option value="archive">
                  {t("settings.workflows.rule.actionArchive")}
                </option>
                <option value="delete">
                  {t("settings.workflows.rule.actionDelete")}
                </option>
                <option value="move">
                  {t("settings.workflows.rule.actionMove")}
                </option>
              </select>
            </div>

            {draft.action === "run_workflow" ? (
              <div className="flex flex-col gap-1">
                <label
                  className="text-xs"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("settings.workflows.rule.mode")}
                </label>
                <select
                  value={draft.mode}
                  onChange={(e) =>
                    setDraft({ ...draft, mode: e.target.value as RuleMode })
                  }
                  className="rounded-md border px-2 py-1 text-sm"
                  style={{
                    borderColor: "var(--border-base)",
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                  }}
                >
                  <option value="confirm">
                    {t("settings.workflows.mode.confirm")}
                  </option>
                  <option value="auto">
                    {t("settings.workflows.mode.auto")}
                  </option>
                </select>
              </div>
            ) : draft.action === "move" ? (
              <div className="flex flex-col gap-1">
                <label
                  className="text-xs"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("settings.workflows.rule.actionDest")}
                </label>
                {/* Gleicher datalist-Pool wie beim Folder-Filter
                    oben, aber separate ID damit Browser die beiden
                    Felder nicht verwechseln. */}
                <input
                  list={`wf-rule-dest-list-${workflowId}`}
                  type="text"
                  value={draft.actionDest ?? ""}
                  onChange={(e) =>
                    setDraft({
                      ...draft,
                      actionDest: e.target.value.trim() || null,
                    })
                  }
                  placeholder={t(
                    "settings.workflows.rule.actionDestPlaceholder",
                  )}
                  className="rounded-md border px-2 py-1 text-sm"
                  style={{
                    borderColor: "var(--border-base)",
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                  }}
                />
                <datalist id={`wf-rule-dest-list-${workflowId}`}>
                  {folderSuggestions.map((name) => (
                    <option key={name} value={name} />
                  ))}
                </datalist>
              </div>
            ) : (
              // Archive/Delete: nichts neben dem Action-Picker.
              // Ordner kommt aus den Account-Settings (archive_folder /
              // trash_folder). Der Sweeper resolved den.
              <div />
            )}
          </div>

          {/* Delay + Dry-Run + Enabled-Switch in einer Zeile. */}
          <div className="grid grid-cols-[220px,1fr] gap-3">
            <div className="flex flex-col gap-1">
              <label className="text-xs" style={{ color: "var(--fg-subtle)" }}>
                {t("settings.workflows.rule.delay")}
              </label>
              <DelayInput
                minutes={draft.delayMinutes}
                onChange={(n) => setDraft({ ...draft, delayMinutes: n })}
              />
            </div>
            <div className="flex flex-col gap-2 self-end pb-1">
              <label className="flex items-center gap-2 text-xs">
                <input
                  type="checkbox"
                  checked={draft.dryRun}
                  onChange={(e) =>
                    setDraft({ ...draft, dryRun: e.target.checked })
                  }
                />
                <span>{t("settings.workflows.rule.dryRun")}</span>
              </label>
              <span
                className="text-[11px]"
                style={{ color: "var(--fg-subtle)" }}
              >
                {draft.dryRun
                  ? t("settings.workflows.rule.dryRunHint")
                  : t("settings.workflows.rule.delayHint")}
              </span>
            </div>
          </div>

          <div className="flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <span
                className="text-xs font-semibold uppercase tracking-wider"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("settings.workflows.rule.predicates")}
              </span>
              <button
                type="button"
                onClick={addPredicate}
                className="rounded-md border px-2 py-0.5 text-xs"
                style={{
                  borderColor: "var(--border-base)",
                  color: "var(--accent)",
                }}
              >
                + {t("settings.workflows.rule.addPredicate")}
              </button>
            </div>

            <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
              {t("settings.workflows.rule.andHint")}
            </p>

            {draft.predicates.map((p, i) => (
              <PredicateRow
                key={i}
                predicate={p}
                onChange={(next) => updatePredicate(i, next)}
                onRemove={
                  draft.predicates.length > 1
                    ? () => removePredicate(i)
                    : undefined
                }
              />
            ))}
          </div>

          <label className="flex items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={draft.enabled}
              onChange={(e) =>
                setDraft({ ...draft, enabled: e.target.checked })
              }
            />
            <span>{t("settings.workflows.enabled")}</span>
          </label>
        </div>
        )}

        {step.kind === "edit" && (
        <footer
          className="flex items-center justify-end gap-2 border-t px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <button
            type="button"
            onClick={onClose}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-muted)",
            }}
          >
            {t("settings.workflows.cancel")}
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={saving || draft.predicates.length === 0}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--accent)",
              color: "var(--bg-panel)",
              opacity: saving || draft.predicates.length === 0 ? 0.6 : 1,
            }}
          >
            {saving
              ? t("settings.workflows.saving")
              : t("settings.workflows.save")}
          </button>
        </footer>
        )}
      </div>
    </div>
  );
}

/**
 * Inner section eines RuleEditor-Modals — Schritt 2 nach erfolgreichem
 * Save: User entscheidet, ob die frische Regel auf bestehende Mails
 * angewendet werden soll. „Anwenden" heißt: Tags setzen, der Sweeper
 * räumt nach Ablauf der Frist (oder bei dry_run gar nicht — das macht
 * ihn risikoarm).
 */
function BackfillPrompt({
  ruleId,
  onSkip,
  onApply,
}: {
  ruleId: string;
  onSkip: () => void;
  onApply: (ruleId: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <div className="flex flex-col gap-3 px-4 py-4">
        <div
          className="text-xs"
          style={{ color: "var(--fg-muted)" }}
        >
          {t("settings.workflows.backfill.saved")}
        </div>
        <p className="text-sm">
          {t("settings.workflows.backfill.explain")}
        </p>
      </div>
      <footer
        className="flex items-center justify-end gap-2 border-t px-4 py-3"
        style={{ borderColor: "var(--border-soft)" }}
      >
        <button
          type="button"
          onClick={onSkip}
          className="rounded-md border px-3 py-1 text-xs"
          style={{
            borderColor: "var(--border-base)",
            color: "var(--fg-muted)",
          }}
        >
          {t("settings.workflows.backfill.skipBtn")}
        </button>
        <button
          type="button"
          onClick={() => onApply(ruleId)}
          className="rounded-md border px-3 py-1 text-xs"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--accent)",
            color: "var(--bg-panel)",
          }}
        >
          {t("settings.workflows.backfill.applyBtn")}
        </button>
      </footer>
    </>
  );
}

function BackfillDone({
  affected,
  error,
  onClose,
}: {
  affected: number;
  error: string | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const message =
    affected === 0
      ? t("settings.workflows.backfill.doneNone")
      : affected === 1
        ? t("settings.workflows.backfill.doneOne")
        : t("settings.workflows.backfill.doneMany", { count: affected });
  return (
    <>
      <div className="flex flex-col gap-3 px-4 py-4">
        {error && <ErrorBanner message={error} />}
        <p className="text-sm">{message}</p>
      </div>
      <footer
        className="flex items-center justify-end gap-2 border-t px-4 py-3"
        style={{ borderColor: "var(--border-soft)" }}
      >
        <button
          type="button"
          onClick={onClose}
          className="rounded-md border px-3 py-1 text-xs"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--accent)",
            color: "var(--bg-panel)",
          }}
        >
          {t("settings.workflows.training.close")}
        </button>
      </footer>
    </>
  );
}

/** Verzögerung als Number-Input + Einheits-Dropdown. Speichert immer
 *  in Minuten, zeigt aber je nach Größe in Min/Std/Tage damit der User
 *  intuitiv „10 Min" oder „7 Tage" eingeben kann. Beim Wechsel der
 *  Einheit wird der Anzeigewert nicht umgerechnet — der User schreibt
 *  den nächsten Wert in der neuen Einheit. */
function DelayInput({
  minutes,
  onChange,
}: {
  minutes: number;
  onChange: (m: number) => void;
}) {
  // Einheit aus dem aktuellen Wert herleiten — bei 0 Default Min damit
  // wir nicht aus Tagen springen müssen wenn der User die Regel auf
  // sofort stellt.
  const initialUnit: "min" | "h" | "d" =
    minutes === 0
      ? "min"
      : minutes % 1440 === 0
        ? "d"
        : minutes % 60 === 0
          ? "h"
          : "min";
  const [unit, setUnit] = useState<"min" | "h" | "d">(initialUnit);
  const factor = unit === "min" ? 1 : unit === "h" ? 60 : 1440;
  const display = factor === 1 ? minutes : Math.round(minutes / factor);
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-2">
      <input
        type="number"
        min={0}
        // Großzügiges max — 365 Tage (525600 Min) reicht für jeden
        // realistischen Use-Case und verhindert Tippfehler-Lawinen.
        max={525_600}
        value={display}
        onChange={(e) => {
          const n = Math.max(0, Math.floor(Number(e.target.value) || 0));
          onChange(n * factor);
        }}
        className="w-20 rounded-md border px-2 py-1 text-sm"
        style={{
          borderColor: "var(--border-base)",
          background: "var(--bg-base)",
          color: "var(--fg-base)",
        }}
      />
      <select
        value={unit}
        onChange={(e) => {
          const next = e.target.value as "min" | "h" | "d";
          // Beim Einheits-Wechsel den absoluten Minutenwert beibehalten
          // — User bekommt sofort die korrekt umgerechnete Zahl im Feld
          // (z.B. „7 Tage" → switch auf „Std" → „168").
          setUnit(next);
        }}
        className="rounded-md border px-2 py-1 text-sm"
        style={{
          borderColor: "var(--border-base)",
          background: "var(--bg-base)",
          color: "var(--fg-base)",
        }}
      >
        <option value="min">{t("settings.workflows.rule.delayUnitMinutes")}</option>
        <option value="h">{t("settings.workflows.rule.delayUnitHours")}</option>
        <option value="d">{t("settings.workflows.rule.delayUnitDays")}</option>
      </select>
    </div>
  );
}

/** Kompakte Anzeige für die Listing-Spalte. „70m" / „4h" / „7d" /
 *  „90m" — wir wählen die größte ganzzahlige Einheit. */
function formatDelayShort(minutes: number): string {
  if (minutes === 0) return "0";
  if (minutes % 1440 === 0) return `${minutes / 1440}d`;
  if (minutes % 60 === 0) return `${minutes / 60}h`;
  return `${minutes}m`;
}

function PredicateRow({
  predicate,
  onChange,
  onRemove,
}: {
  predicate: RulePredicate;
  onChange: (next: RulePredicate) => void;
  onRemove?: () => void;
}) {
  const { t } = useTranslation();

  const changeKind = (kind: RulePredicate["kind"]) => {
    // Preserve as much of the prior value as the new shape can carry
    // — switching fromDomain → fromDomainIn keeps the single domain
    // as the first list entry; the reverse takes the first entry and
    // drops the rest. String ↔ list transitions use best-effort.
    switch (kind) {
      case "fromEmail":
      case "fromDomain":
      case "subjectContains": {
        const prior =
          "value" in predicate
            ? predicate.value
            : predicate.kind === "fromDomainIn"
              ? predicate.values[0] ?? ""
              : "";
        onChange({ kind, value: prior });
        break;
      }
      case "fromDomainIn": {
        const prior =
          predicate.kind === "fromDomainIn"
            ? predicate.values
            : "value" in predicate && predicate.value
              ? [predicate.value]
              : [];
        onChange({ kind: "fromDomainIn", values: prior });
        break;
      }
      case "hasAttachmentExtension": {
        const prior =
          predicate.kind === "hasAttachmentExtension"
            ? predicate.extension
            : "csv";
        onChange({ kind: "hasAttachmentExtension", extension: prior });
        break;
      }
    }
  };

  return (
    <div
      className="flex items-center gap-2 rounded-md border p-2"
      style={{ borderColor: "var(--border-soft)" }}
    >
      <select
        value={predicate.kind}
        onChange={(e) => changeKind(e.target.value as RulePredicate["kind"])}
        className="rounded-md border px-2 py-1 text-xs"
        style={{
          borderColor: "var(--border-base)",
          background: "var(--bg-panel)",
          color: "var(--fg-base)",
        }}
      >
        <option value="fromEmail">
          {t("settings.workflows.predicate.fromEmail")}
        </option>
        <option value="fromDomain">
          {t("settings.workflows.predicate.fromDomain")}
        </option>
        <option value="fromDomainIn">
          {t("settings.workflows.predicate.fromDomainIn")}
        </option>
        <option value="subjectContains">
          {t("settings.workflows.predicate.subjectContains")}
        </option>
        <option value="hasAttachmentExtension">
          {t("settings.workflows.predicate.hasAttachmentExtension")}
        </option>
      </select>

      {predicate.kind === "hasAttachmentExtension" ? (
        <input
          list="workflow-attachment-ext-list"
          type="text"
          value={predicate.extension}
          onChange={(e) =>
            onChange({
              kind: "hasAttachmentExtension",
              extension: e.target.value
                .trim()
                .replace(/^\.+/, "")
                .toLowerCase(),
            })
          }
          placeholder="csv"
          className="flex-1 rounded-md border px-2 py-1 text-xs"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-panel)",
            color: "var(--fg-base)",
          }}
        />
      ) : predicate.kind === "fromDomainIn" ? (
        <DomainChipInput
          values={predicate.values}
          onChange={(values) =>
            onChange({ kind: "fromDomainIn", values })
          }
        />
      ) : (
        <input
          type="text"
          value={predicate.value}
          onChange={(e) =>
            onChange({ ...predicate, value: e.target.value })
          }
          placeholder={placeholderForKind(predicate.kind)}
          className="flex-1 rounded-md border px-2 py-1 text-xs"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-panel)",
            color: "var(--fg-base)",
          }}
        />
      )}

      {onRemove && (
        <button
          type="button"
          onClick={onRemove}
          className="rounded px-1.5 py-0.5 text-xs"
          style={{ color: "#ef4444" }}
        >
          ✕
        </button>
      )}
    </div>
  );
}

function placeholderForKind(kind: RulePredicate["kind"]): string {
  switch (kind) {
    case "fromEmail":
      return "sender@example.com";
    case "fromDomain":
      return "example.com";
    case "fromDomainIn":
      return "example.com, other.com";
    case "subjectContains":
      return "Rechnung";
    case "hasAttachmentExtension":
      return "csv";
  }
}

/**
 * Chip-based multi-value input for the `fromDomainIn` predicate.
 * Adds on Enter or comma, removes via the × chip, and also accepts
 * paste of a comma-separated string (you can copy a list of domains
 * out of a spreadsheet and dump it in). Normalises each domain to
 * lower-case, no leading `@` or `.`, no whitespace.
 */
function DomainChipInput({
  values,
  onChange,
}: {
  values: string[];
  onChange: (next: string[]) => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");

  const normalise = (s: string): string =>
    s
      .trim()
      .toLowerCase()
      .replace(/^@+/, "")
      .replace(/^\.+/, "");

  const addMany = (raw: string) => {
    const candidates = raw
      .split(/[,\s]+/)
      .map(normalise)
      .filter((s) => s.length > 0);
    if (candidates.length === 0) return;
    // Dedupe against existing list — a rule with `example.com` twice
    // is equivalent to once, but reading it is annoying.
    const seen = new Set(values);
    const next = [...values];
    for (const c of candidates) {
      if (!seen.has(c)) {
        seen.add(c);
        next.push(c);
      }
    }
    onChange(next);
    setDraft("");
  };

  const removeAt = (idx: number) => {
    onChange(values.filter((_, i) => i !== idx));
  };

  return (
    <div
      className="flex flex-1 flex-wrap items-center gap-1 rounded-md border px-2 py-1"
      style={{
        borderColor: "var(--border-base)",
        background: "var(--bg-panel)",
      }}
      onClick={(e) => {
        // Click on the chip wrapper focuses the input — standard
        // chip-field UX so the user doesn't have to hit the tiny
        // input area exactly.
        const input = (e.currentTarget.querySelector(
          "input[data-chip-input]",
        ) as HTMLInputElement | null);
        input?.focus();
      }}
    >
      {values.map((v, i) => (
        <span
          key={`${v}-${i}`}
          className="flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px]"
          style={{
            background: "var(--bg-hover)",
            color: "var(--fg-base)",
          }}
        >
          {v}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              removeAt(i);
            }}
            className="text-[11px]"
            style={{ color: "var(--fg-muted)" }}
            aria-label={t("settings.workflows.predicate.removeDomain", {
              domain: v,
            })}
          >
            ×
          </button>
        </span>
      ))}
      <input
        data-chip-input
        type="text"
        value={draft}
        onChange={(e) => {
          const next = e.target.value;
          // Comma-auto-commit: if the user types (or pastes) a value
          // ending in comma, flush it as a chip.
          if (next.includes(",")) {
            addMany(next);
          } else {
            setDraft(next);
          }
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            addMany(draft);
          } else if (
            e.key === "Backspace" &&
            draft.length === 0 &&
            values.length > 0
          ) {
            // Empty field + backspace = pop last chip. Standard
            // chip-UX nicety.
            removeAt(values.length - 1);
          }
        }}
        onBlur={() => {
          if (draft.trim().length > 0) addMany(draft);
        }}
        placeholder={
          values.length === 0
            ? placeholderForKind("fromDomainIn")
            : ""
        }
        className="min-w-[120px] flex-1 bg-transparent text-xs outline-none"
        style={{ color: "var(--fg-base)" }}
      />
    </div>
  );
}

/**
 * Chip-Input für die `choices`-Liste eines Choice-Parameters.
 * Bewusst KEINE Normalisierung (im Gegensatz zu DomainChipInput): die
 * Werte landen 1:1 als CLI-Argumente an das Skript, da würde Lower-Case
 * z.B. „CONCIDE_DEV" → „concide_dev" verstümmeln.
 *
 * Reihenfolge bleibt erhalten — die zeigt sich später im Pre-Apply-
 * Dialog als Dropdown-Reihenfolge und im Fixed-Source-Picker. Drag-
 * and-drop-Reorder lassen wir vorerst — wer's wirklich braucht,
 * kann's aushebeln indem er eine bestehende Liste komplett neu
 * tippt.
 */
function ChoicesChipInput({
  values,
  onChange,
}: {
  values: string[];
  onChange: (next: string[]) => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState("");

  const commitDraft = () => {
    // Mehrere Werte auf einmal: Komma oder Newline als Separator.
    // Whitespace innerhalb eines Eintrags bleibt erhalten — Anhang-
    // namen oder Choice-Labels mit Leerzeichen sind legal.
    const candidates = draft
      .split(/[,\n]/)
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    if (candidates.length === 0) return;
    const seen = new Set(values);
    const next = [...values];
    for (const c of candidates) {
      if (!seen.has(c)) {
        seen.add(c);
        next.push(c);
      }
    }
    onChange(next);
    setDraft("");
  };

  const removeAt = (idx: number) => {
    onChange(values.filter((_, i) => i !== idx));
  };

  return (
    <div
      className="flex flex-wrap items-center gap-1 rounded-md border px-2 py-1"
      style={{
        borderColor: "var(--border-base)",
        background: "var(--bg-panel)",
      }}
      onClick={(e) => {
        const input = e.currentTarget.querySelector(
          "input[data-chip-input]",
        ) as HTMLInputElement | null;
        input?.focus();
      }}
    >
      {values.map((v, i) => (
        <span
          key={`${v}-${i}`}
          className="flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px]"
          style={{
            background: "var(--bg-hover)",
            color: "var(--fg-base)",
          }}
        >
          {v}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              removeAt(i);
            }}
            className="text-[11px]"
            style={{ color: "var(--fg-muted)" }}
            aria-label={t("settings.workflows.step.removeChoice", {
              choice: v,
            })}
          >
            ×
          </button>
        </span>
      ))}
      <input
        data-chip-input
        type="text"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === ",") {
            e.preventDefault();
            commitDraft();
          } else if (
            e.key === "Backspace" &&
            draft.length === 0 &&
            values.length > 0
          ) {
            // Leeres Feld + Backspace → letzte Choice rauspicken; klassische
            // Chip-Input-UX (Slack, GitHub).
            e.preventDefault();
            removeAt(values.length - 1);
          }
        }}
        onBlur={() => {
          if (draft.trim().length > 0) commitDraft();
        }}
        placeholder={
          values.length === 0
            ? t("settings.workflows.step.choicesPlaceholder")
            : ""
        }
        className="min-w-[120px] flex-1 bg-transparent text-xs outline-none"
        style={{ color: "var(--fg-base)" }}
      />
    </div>
  );
}
