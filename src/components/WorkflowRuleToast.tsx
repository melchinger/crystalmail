import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { RuleMatchEvent, WorkflowRunResult } from "../types";
import { WorkflowResultDialog } from "./WorkflowResultDialog";

/**
 * Global subscriber for confirm-mode workflow-rule matches. The Rust
 * matcher emits `workflow-rule-match` events from its post-store_body
 * hook; we queue them up and surface them as a stacked toast in the
 * bottom-right corner. Each toast has three buttons:
 *
 *   * **Anwenden** — call `apply_workflow_rule` for that rule+message
 *     pair. The result dialog pops over the toast so the user can
 *     confirm the workflow actually did its thing.
 *   * **Verwerfen** — just drop the toast. The match stays in the
 *     backend's rule hit counter only if/when the user applies later
 *     (intentional: we don't count "Dismiss").
 *   * **×** — close without firing. Same semantics as Verwerfen.
 *
 * De-dupe: same (ruleId, messageId) pair only shows once even if the
 * matcher fires it multiple times (possible on a re-prefetch).
 */
export function WorkflowRuleToastStack() {
  const { t } = useTranslation();
  const [queue, setQueue] = useState<RuleMatchEvent[]>([]);
  const [applyingKey, setApplyingKey] = useState<string | null>(null);
  const [lastResult, setLastResult] = useState<{
    result: WorkflowRunResult;
    name: string;
  } | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      const off = await listen<RuleMatchEvent>(
        "workflow-rule-match",
        (e) => {
          const incoming = e.payload;
          setQueue((q) => {
            // De-dupe by (ruleId, messageId) — the matcher can fire
            // the same match twice if prefetch re-hits the mail.
            const key = `${incoming.ruleId}::${incoming.messageId}`;
            if (
              q.some((x) => `${x.ruleId}::${x.messageId}` === key)
            ) {
              return q;
            }
            return [...q, incoming];
          });
        },
      );
      unlisten = off;
    })();
    return () => {
      unlisten?.();
    };
  }, []);

  const drop = (key: string) => {
    setQueue((q) => q.filter((x) => `${x.ruleId}::${x.messageId}` !== key));
  };

  const apply = async (ev: RuleMatchEvent) => {
    const key = `${ev.ruleId}::${ev.messageId}`;
    if (applyingKey) return;
    setApplyingKey(key);
    try {
      const result = await invoke<WorkflowRunResult>("apply_workflow_rule", {
        ruleId: ev.ruleId,
        messageId: ev.messageId,
      });
      setLastResult({ result, name: ev.workflowName });
      drop(key);
    } catch (e) {
      // Surface the failure through the same result dialog so the
      // user can read a stdout/stderr if Rust sent one, rather than
      // a bare toast "Fehler".
      setLastResult({
        result: {
          workflowId: ev.workflowId,
          messageId: ev.messageId,
          allOk: false,
          steps: [
            {
              stepIndex: 0,
              stepType: "runScript",
              ok: false,
              message: String(e),
              detail: null,
            },
          ],
        },
        name: ev.workflowName,
      });
      drop(key);
    } finally {
      setApplyingKey(null);
    }
  };

  if (queue.length === 0 && !lastResult) return null;

  return (
    <>
      <div
        className="fixed bottom-4 right-4 z-[45] flex flex-col gap-2"
        aria-live="polite"
      >
        {queue.map((ev) => {
          const key = `${ev.ruleId}::${ev.messageId}`;
          const isBusy = applyingKey === key;
          return (
            <div
              key={key}
              className="flex w-[360px] flex-col gap-2 rounded-lg border p-3 text-xs shadow-xl"
              style={{
                background: "var(--bg-panel)",
                borderColor: "var(--border-base)",
                color: "var(--fg-base)",
              }}
            >
              <div className="flex items-start justify-between gap-2">
                <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span
                    className="text-[10px] uppercase tracking-wider"
                    style={{ color: "var(--accent)" }}
                  >
                    {t("workflows.toastHeadline", {
                      name: ev.workflowName,
                    })}
                  </span>
                  <span
                    className="truncate"
                    style={{ color: "var(--fg-base)" }}
                  >
                    {ev.subject || t("workflows.toastNoSubject")}
                  </span>
                  <span
                    className="truncate text-[11px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {ev.fromEmail}
                  </span>
                </div>
                <button
                  type="button"
                  onClick={() => drop(key)}
                  disabled={isBusy}
                  className="text-xs"
                  style={{ color: "var(--fg-muted)" }}
                  aria-label={t("workflows.toastClose")}
                >
                  ✕
                </button>
              </div>
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => drop(key)}
                  disabled={isBusy}
                  className="rounded-md border px-2 py-0.5 text-xs"
                  style={{
                    borderColor: "var(--border-soft)",
                    color: "var(--fg-muted)",
                  }}
                >
                  {t("workflows.toastDismiss")}
                </button>
                <button
                  type="button"
                  onClick={() => void apply(ev)}
                  disabled={isBusy}
                  className="rounded-md border px-2 py-0.5 text-xs"
                  style={{
                    borderColor: "var(--border-base)",
                    background: "var(--accent)",
                    color: "var(--bg-panel)",
                    opacity: isBusy ? 0.6 : 1,
                  }}
                >
                  {isBusy
                    ? t("workflows.toastApplying")
                    : t("workflows.toastApply")}
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {lastResult && (
        <WorkflowResultDialog
          result={lastResult.result}
          workflowName={lastResult.name}
          onClose={() => setLastResult(null)}
        />
      )}
    </>
  );
}
