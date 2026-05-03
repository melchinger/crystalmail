import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type {
  AccountSummary,
  RulePredicate,
  Workflow,
  WorkflowRuleDraft,
  WorkflowTrainingCandidate,
  WorkflowTrainingResult,
} from "../../types";
import { WORKFLOW_TRAINING_CANCELLED } from "../../types";
import { isAiDisabledError, useAiEnabled } from "../../utils/aiState";
import { AiRequiredNotice } from "../AiRequiredNotice";

type Phase =
  | { kind: "prompt" } // awaiting user to press "Start"
  | { kind: "running" } // pi in flight
  | { kind: "result"; result: WorkflowTrainingResult }
  | { kind: "error"; message: string };

type Props = {
  workflow: Workflow;
  candidates: WorkflowTrainingCandidate[];
  accounts: AccountSummary[];
  onClose: () => void;
  /** Called after the user accepts a proposal — caller refreshes its
   *  rule list and closes the dialog. */
  onRuleCreated: () => void;
  /** Called when the user chose "Edit before saving" — the parent
   *  takes the draft and opens the normal RuleEditor prefilled. */
  onEditDraft: (draft: WorkflowRuleDraft) => void;
  /** Optional jump-to-AI-settings affordance. Shown inside the
   *  AiRequiredNotice when the master kill-switch is off. Caller
   *  closes this dialog and routes to the pi tab — see
   *  `WorkflowSettings`. */
  onOpenAiSettings?: () => void;
};

/**
 * Three-phase dialog for the pi rule learner:
 *
 *   prompt  — show candidate count + list, let user hit "Training
 *             starten" or close.
 *   running — streaming pi, cancel button active.
 *   result  — show the proposal (predicates + scopes + mode +
 *             reason). Accept creates the rule directly; Edit
 *             hands the draft back up to the RuleEditor.
 *
 * The dialog doesn't touch the training-candidate list — it stays
 * intact so the user can train the same set against multiple
 * workflows if they want. Clearing is a manual action in the
 * Settings overview row.
 */
export function WorkflowTrainingDialog({
  workflow,
  candidates,
  accounts,
  onClose,
  onRuleCreated,
  onEditDraft,
  onOpenAiSettings,
}: Props) {
  const { t } = useTranslation();
  const [phase, setPhase] = useState<Phase>({ kind: "prompt" });
  const [saving, setSaving] = useState(false);
  const [aiEnabled] = useAiEnabled();
  // Track whether a cancel is already in-flight so the button can
  // reflect "Cancelling…" instead of re-firing the tauri call.
  const cancellingRef = useRef(false);

  const start = async () => {
    if (!aiEnabled) {
      // Defensive — the start button is disabled in this state.
      return;
    }
    setPhase({ kind: "running" });
    cancellingRef.current = false;
    try {
      const result = await invoke<WorkflowTrainingResult>(
        "suggest_workflow_rule",
        { workflowId: workflow.id },
      );
      setPhase({ kind: "result", result });
    } catch (e) {
      const msg = String(e);
      if (msg.includes(WORKFLOW_TRAINING_CANCELLED)) {
        // Cancel is a benign path — don't show error, just back to
        // the prompt so the user can try again or close.
        setPhase({ kind: "prompt" });
      } else if (isAiDisabledError(e)) {
        // AI got toggled off mid-run (rare race). Bounce back to
        // the prompt phase — the AiRequiredNotice will render
        // because aiEnabled is false now.
        setPhase({ kind: "prompt" });
      } else {
        setPhase({ kind: "error", message: msg });
      }
    }
  };

  const cancel = async () => {
    if (cancellingRef.current) return;
    cancellingRef.current = true;
    try {
      await invoke("cancel_workflow_training");
    } catch {
      // Ignore — the outer call's own error path handles the visible
      // state transition.
    }
  };

  const accept = async (result: WorkflowTrainingResult) => {
    setSaving(true);
    try {
      const draft = proposalToDraft(workflow.id, result);
      await invoke("add_workflow_rule", { draft });
      onRuleCreated();
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    } finally {
      setSaving(false);
    }
  };

  const editDraft = (result: WorkflowTrainingResult) => {
    onEditDraft(proposalToDraft(workflow.id, result));
  };

  useEffect(() => {
    // Dialog auto-closes nothing on unmount — the async training may
    // still be running if the user closes. Fire a best-effort cancel
    // so a dangling pi child doesn't keep burning cycles.
    return () => {
      if (phase.kind === "running") void cancel();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      className="fixed inset-0 z-[63] flex items-start justify-center overflow-y-auto px-4 py-[10vh]"
      style={{ background: "rgba(0,0,0,0.55)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && phase.kind !== "running") onClose();
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
            {t("settings.workflows.training.title", {
              name: workflow.name,
            })}
          </h2>
          <button
            type="button"
            onClick={onClose}
            disabled={phase.kind === "running"}
            className="text-xs"
            style={{ color: "var(--fg-muted)" }}
          >
            ✕
          </button>
        </header>

        <div className="flex flex-col gap-4 px-4 py-4">
          {phase.kind === "prompt" && !aiEnabled && (
            <AiRequiredNotice onOpenAiSettings={onOpenAiSettings} />
          )}
          {phase.kind === "prompt" && aiEnabled && (
            <PromptPhase candidates={candidates} />
          )}
          {phase.kind === "running" && (
            <RunningPhase onCancel={() => void cancel()} />
          )}
          {phase.kind === "result" && (
            <ResultPhase
              result={phase.result}
              accounts={accounts}
              onAccept={() => void accept(phase.result)}
              onEdit={() => editDraft(phase.result)}
              onRetry={() => void start()}
              saving={saving}
            />
          )}
          {phase.kind === "error" && (
            <div
              className="rounded-md border px-3 py-2 text-xs"
              style={{
                borderColor: "#ef4444",
                background: "rgba(239,68,68,0.08)",
                color: "#ef4444",
              }}
            >
              {phase.message}
            </div>
          )}
        </div>

        {(phase.kind === "prompt" || phase.kind === "error") && (
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
              {t("settings.workflows.training.close")}
            </button>
            <button
              type="button"
              onClick={() => void start()}
              disabled={candidates.length === 0 || !aiEnabled}
              className="rounded-md border px-3 py-1 text-xs"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--accent)",
                color: "var(--bg-panel)",
                opacity:
                  candidates.length === 0 || !aiEnabled ? 0.5 : 1,
              }}
            >
              {phase.kind === "error"
                ? t("settings.workflows.training.retry")
                : t("settings.workflows.training.start")}
            </button>
          </footer>
        )}
      </div>
    </div>
  );
}

function PromptPhase({
  candidates,
}: {
  candidates: WorkflowTrainingCandidate[];
}) {
  const { t } = useTranslation();
  if (candidates.length === 0) {
    return (
      <div
        className="rounded-md border px-3 py-3 text-xs"
        style={{
          borderColor: "var(--border-soft)",
          color: "var(--fg-muted)",
        }}
      >
        {t("settings.workflows.training.noCandidates")}
      </div>
    );
  }
  return (
    <>
      <p className="text-xs" style={{ color: "var(--fg-subtle)" }}>
        {t("settings.workflows.training.prompt", {
          count: candidates.length,
        })}
      </p>
      <ul
        className="flex max-h-[40vh] flex-col overflow-y-auto rounded-md border"
        style={{
          borderColor: "var(--border-soft)",
          background: "var(--bg-base)",
        }}
      >
        {candidates.map((c, i) => (
          <li
            key={c.messageId}
            className={`flex flex-col px-3 py-1.5 text-xs ${
              i === 0 ? "" : "border-t"
            }`}
            style={{ borderColor: "var(--border-soft)" }}
          >
            <span style={{ color: "var(--fg-base)" }}>
              {c.subject || "(ohne Betreff)"}
            </span>
            <span
              className="text-[11px]"
              style={{ color: "var(--fg-subtle)" }}
            >
              {c.fromEmail} · {c.folderName}
            </span>
          </li>
        ))}
      </ul>
    </>
  );
}

function RunningPhase({ onCancel }: { onCancel: () => void }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col items-center gap-3 py-6">
      <div
        className="h-6 w-6 animate-pulse rounded-full"
        style={{ background: "var(--accent)" }}
      />
      <p className="text-sm" style={{ color: "var(--fg-base)" }}>
        {t("settings.workflows.training.running")}
      </p>
      <button
        type="button"
        onClick={onCancel}
        className="rounded-md border px-3 py-1 text-xs"
        style={{
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        {t("settings.workflows.training.cancel")}
      </button>
    </div>
  );
}

function ResultPhase({
  result,
  accounts,
  onAccept,
  onEdit,
  onRetry,
  saving,
}: {
  result: WorkflowTrainingResult;
  accounts: AccountSummary[];
  onAccept: () => void;
  onEdit: () => void;
  onRetry: () => void;
  saving: boolean;
}) {
  const { t } = useTranslation();
  const proposal = result.proposal;
  const empty = proposal.predicates.length === 0;

  const accountName = proposal.accountId
    ? accounts.find((a) => a.id === proposal.accountId)?.displayName ??
      proposal.accountId
    : null;

  return (
    <>
      {proposal.reason && (
        <div
          className="rounded-md border px-3 py-2 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-base)",
          }}
        >
          <strong style={{ color: "var(--fg-subtle)" }}>
            {t("settings.workflows.training.piReason")}:
          </strong>{" "}
          {proposal.reason}
        </div>
      )}

      {empty ? (
        <div
          className="rounded-md border px-3 py-3 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            color: "var(--fg-muted)",
          }}
        >
          {t("settings.workflows.training.noProposal")}
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          <span
            className="text-[10px] uppercase tracking-wider"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("settings.workflows.training.proposedRule")}
          </span>
          <div
            className="flex flex-col gap-1 rounded-md border p-3 text-xs"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
            }}
          >
            <div className="flex flex-wrap items-center gap-2">
              {/* Action-Badge — pi darf statt RunWorkflow auch eine
                  Direkt-Aktion vorschlagen. Color-Tone matcht das
                  Listing in den Settings (grün=archive, rot=delete,
                  orange=move, blau/rot=mode-abhängig bei run_workflow). */}
              <span
                className="rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wider"
                style={proposalActionBadgeStyle(proposal)}
              >
                {proposalActionLabel(proposal, t)}
              </span>
              {proposal.dryRun && (
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
              {proposal.delayMinutes > 0 && (
                <span
                  className="text-[10px]"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  · {formatDelayProposal(proposal.delayMinutes)}
                </span>
              )}
              {accountName && (
                <span style={{ color: "var(--fg-subtle)" }}>
                  · {accountName}
                </span>
              )}
              {proposal.folderName && (
                <span style={{ color: "var(--fg-subtle)" }}>
                  · {proposal.folderName}
                </span>
              )}
            </div>
            <ul className="flex flex-col gap-0.5">
              {proposal.predicates.map((p, i) => (
                <li
                  key={i}
                  className="text-[12px]"
                  style={{ color: "var(--fg-base)" }}
                >
                  • {renderPredicate(p)}
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}

      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onRetry}
          disabled={saving}
          className="rounded-md border px-3 py-1 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            color: "var(--fg-muted)",
          }}
        >
          {t("settings.workflows.training.retry")}
        </button>
        {!empty && (
          <>
            <button
              type="button"
              onClick={onEdit}
              disabled={saving}
              className="rounded-md border px-3 py-1 text-xs"
              style={{
                borderColor: "var(--border-base)",
                color: "var(--fg-base)",
              }}
            >
              {t("settings.workflows.training.edit")}
            </button>
            <button
              type="button"
              onClick={onAccept}
              disabled={saving}
              className="rounded-md border px-3 py-1 text-xs"
              style={{
                borderColor: "var(--border-base)",
                background: "var(--accent)",
                color: "var(--bg-panel)",
                opacity: saving ? 0.6 : 1,
              }}
            >
              {saving
                ? t("settings.workflows.training.saving")
                : t("settings.workflows.training.accept")}
            </button>
          </>
        )}
      </div>
    </>
  );
}

/** Anzeigeformat für pi-vorgeschlagene Delay-Werte. Identisch zur
 *  Listing-Funktion in WorkflowSettings, aber dupliziert um keine
 *  Cross-File-Imports zu erzwingen — die Funktion ist trivial. */
function formatDelayProposal(minutes: number): string {
  if (minutes === 0) return "0";
  if (minutes % 1440 === 0) return `${minutes / 1440}d`;
  if (minutes % 60 === 0) return `${minutes / 60}h`;
  return `${minutes}m`;
}

function renderPredicate(p: RulePredicate): string {
  switch (p.kind) {
    case "fromEmail":
      return `Absender = ${p.value}`;
    case "fromDomain":
      return `Domain = ${p.value}`;
    case "fromDomainIn":
      return `Domain ∈ {${p.values.join(", ")}}`;
    case "subjectContains":
      return `Betreff enthält "${p.value}"`;
    case "hasAttachmentExtension":
      return `Anhang .${p.extension}`;
  }
}

/** Badge-Tone passend zur vorgeschlagenen Action — gleiche Farbpalette
 *  wie im Workflow-Settings-Listing, damit der User die selben Tones
 *  überall wiedererkennt. */
function proposalActionBadgeStyle(p: {
  action: import("../../types").RuleAction;
  mode: import("../../types").RuleMode;
}): { background: string; color: string } {
  if (p.action === "run_workflow") {
    return p.mode === "auto"
      ? { background: "rgba(239,68,68,0.15)", color: "#ef4444" }
      : { background: "rgba(59,130,246,0.15)", color: "#3b82f6" };
  }
  switch (p.action) {
    case "archive":
      return { background: "rgba(34,197,94,0.15)", color: "#22c55e" };
    case "delete":
      return { background: "rgba(239,68,68,0.15)", color: "#ef4444" };
    case "move":
      return { background: "rgba(245,158,11,0.15)", color: "#f59e0b" };
  }
}

function proposalActionLabel(
  p: {
    action: import("../../types").RuleAction;
    actionDest: string | null;
    mode: import("../../types").RuleMode;
  },
  t: (k: string, opts?: Record<string, unknown>) => string,
): string {
  switch (p.action) {
    case "run_workflow":
      return t(`settings.workflows.mode.${p.mode}`);
    case "archive":
      return t("settings.workflows.rule.actionArchive");
    case "delete":
      return t("settings.workflows.rule.actionDelete");
    case "move":
      return p.actionDest
        ? t("settings.workflows.auditLog.actionMove", { dest: p.actionDest })
        : t("settings.workflows.rule.actionMove");
  }
}

function proposalToDraft(
  workflowId: string,
  result: WorkflowTrainingResult,
): WorkflowRuleDraft {
  // pi darf eine Direkt-Aktion vorschlagen (archive/delete/move) — in
  // dem Fall ist die Workflow-Bindung irrelevant, das Backend setzt
  // workflow_id auf null beim Save (siehe `resolve_action_fields`).
  // Wir geben workflowId trotzdem mit, weil das Frontend-Modell ihn
  // für RunWorkflow-Rules erwartet. Bei Direkt-Aktionen wird er
  // ignoriert.
  return {
    name: "",
    workflowId,
    accountId: result.proposal.accountId,
    folderName: result.proposal.folderName,
    predicates: result.proposal.predicates,
    mode: result.proposal.mode,
    action: result.proposal.action,
    actionDest: result.proposal.actionDest,
    delayMinutes: result.proposal.delayMinutes,
    dryRun: result.proposal.dryRun,
    enabled: true,
  };
}
