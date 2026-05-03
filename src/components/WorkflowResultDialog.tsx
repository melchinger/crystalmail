import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { WorkflowRunResult } from "../types";

type Props = {
  result: WorkflowRunResult;
  workflowName?: string;
  onClose: () => void;
};

/**
 * Audit trail for a single workflow application. One line per step
 * with an ok/err indicator; click on a step expands its detail
 * (stdout/stderr for RunScript, paths for SaveAttachments, etc).
 *
 * The banner colour reflects `allOk`: green when everything worked,
 * amber when at least one step failed. The dialog deliberately
 * doesn't auto-close — when a workflow has a failing step the user
 * should read what happened, not have to dig through logs later.
 */
export function WorkflowResultDialog({
  result,
  workflowName,
  onClose,
}: Props) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState<Set<number>>(new Set());

  const toggle = (idx: number) => {
    const next = new Set(expanded);
    if (next.has(idx)) next.delete(idx);
    else next.add(idx);
    setExpanded(next);
  };

  return (
    <div
      className="fixed inset-0 z-[55] flex items-center justify-center px-4"
      style={{ background: "rgba(0,0,0,0.5)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose();
      }}
    >
      <div
        role="dialog"
        className="flex max-h-[85vh] w-full max-w-2xl flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <header
          className="flex items-center justify-between border-b px-4 py-3"
          style={{
            borderColor: "var(--border-soft)",
            background: result.allOk
              ? "rgba(34,197,94,0.1)"
              : "rgba(239,68,68,0.1)",
          }}
        >
          <div className="flex items-center gap-2">
            <span
              className="text-lg"
              aria-hidden
              style={{
                color: result.allOk ? "#22c55e" : "#ef4444",
              }}
            >
              {result.allOk ? "✓" : "⚠"}
            </span>
            <h2 className="text-sm font-semibold">
              {workflowName
                ? t("workflows.resultTitleNamed", { name: workflowName })
                : t("workflows.resultTitle")}
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            autoFocus
            className="text-xs"
            style={{ color: "var(--fg-muted)" }}
          >
            ✕
          </button>
        </header>

        <div className="flex flex-col divide-y overflow-y-auto">
          {result.steps.map((s) => {
            const isExpanded = expanded.has(s.stepIndex);
            return (
              <div
                key={s.stepIndex}
                className="flex flex-col px-4 py-2"
                style={{ borderColor: "var(--border-soft)" }}
              >
                <button
                  type="button"
                  onClick={() => s.detail && toggle(s.stepIndex)}
                  className="flex items-start gap-3 text-left"
                  disabled={!s.detail}
                  style={{
                    cursor: s.detail ? "pointer" : "default",
                  }}
                >
                  <span
                    className="mt-0.5 text-sm"
                    aria-hidden
                    style={{
                      color: s.ok ? "#22c55e" : "#ef4444",
                    }}
                  >
                    {s.ok ? "✓" : "✗"}
                  </span>
                  <div className="flex min-w-0 flex-1 flex-col">
                    <span
                      className="text-[10px] uppercase tracking-wider"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      #{s.stepIndex + 1} · {s.stepType}
                    </span>
                    <span
                      className="text-sm"
                      style={{
                        color: s.ok ? "var(--fg-base)" : "#ef4444",
                      }}
                    >
                      {s.message}
                    </span>
                  </div>
                  {s.detail && (
                    <span
                      className="text-[11px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      {isExpanded ? "▼" : "▶"}
                    </span>
                  )}
                </button>
                {isExpanded && s.detail && (
                  <pre
                    className="mt-2 overflow-x-auto whitespace-pre-wrap rounded-md border px-3 py-2 text-[11px]"
                    style={{
                      borderColor: "var(--border-soft)",
                      background: "var(--bg-base)",
                      color: "var(--fg-subtle)",
                    }}
                  >
                    {s.detail}
                  </pre>
                )}
              </div>
            );
          })}
        </div>

        <footer
          className="flex justify-end border-t px-4 py-2"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <button
            type="button"
            onClick={onClose}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
          >
            {t("workflows.resultClose")}
          </button>
        </footer>
      </div>
    </div>
  );
}
