import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type {
  ApplyRuleResult,
  RuleDraft,
  SuggestResult,
} from "../types";
import { isAiDisabledError, useAiEnabled } from "../utils/aiState";
import { AiRequiredNotice } from "./AiRequiredNotice";

type Props = {
  /** IDs of the `$Junk`-flagged inbox candidates — the corpus pi analyses. */
  candidateIds: string[];
  onClose: () => void;
  /** Called after successful rule application — parent should refresh
   *  the inbox so moved mails disappear. */
  onApplied: () => void;
  /** Optional jump-to-AI-settings affordance. Shown inside the
   *  AiRequiredNotice when the master kill-switch is off — caller
   *  closes this dialog and routes to the pi tab. */
  onOpenAiSettings?: () => void;
};

type Phase =
  | { kind: "analyzing" }
  | { kind: "suggested"; result: SuggestResult }
  | { kind: "noSuggestions"; result: SuggestResult }
  | { kind: "cancelled" }
  | { kind: "needsAi" }
  | { kind: "error"; message: string };

const CANCEL_SENTINEL = "cancelled_by_user";

/**
 * Analyses the user's spam-candidate corpus via pi and offers the
 * resulting rule drafts for one-click application. Dialog stays open
 * after an apply so the user can chain multiple rules from the same
 * batch — rare but useful if pi proposes two orthogonal patterns.
 */
export function LearnSpamRuleDialog({
  candidateIds,
  onClose,
  onApplied,
  onOpenAiSettings,
}: Props) {
  const { t } = useTranslation();
  const [aiEnabled] = useAiEnabled();
  // Phase starts as `analyzing` only when AI is on; otherwise we
  // short-circuit to `needsAi` so we never kick off `suggest_spam_rules`
  // and ride into a guaranteed-error path. Initial-state dependence on
  // a hook value is fine here — useAiEnabled has its own optimistic
  // default (`true`) that we trust on first paint.
  const [phase, setPhase] = useState<Phase>(
    aiEnabled ? { kind: "analyzing" } : { kind: "needsAi" },
  );
  /** Which draft rows the user has already applied in this dialog instance. */
  const [appliedIndices, setAppliedIndices] = useState<Set<number>>(new Set());
  const [applyingIdx, setApplyingIdx] = useState<number | null>(null);
  const [applyResults, setApplyResults] = useState<
    Record<number, ApplyRuleResult>
  >({});
  const [showRaw, setShowRaw] = useState(false);

  // Snapshot the ID list at mount time. The dialog's entire lifecycle
  // runs against this fixed set — parent re-renders that reshape the
  // prop (e.g. `inbox.filter().map()` creates a fresh array reference
  // on every App.tsx tick) must NOT re-trigger `suggest_spam_rules`.
  // One click → exactly one pi analyse call.
  const idsRef = useRef(candidateIds);
  /**
   * React 18 StrictMode runs mount effects twice in development. We
   * deduplicate via the *promise* rather than by skipping the second
   * effect body outright: the invoke is fired exactly once and cached
   * in this ref; both effect runs subscribe .then/.catch to the same
   * promise, so the UI updates reliably regardless of whether the
   * first mount's local `cancelled` flag got flipped.
   */
  const invokePromiseRef = useRef<Promise<SuggestResult> | null>(null);

  useEffect(() => {
    // Don't fire pi when AI is off — `phase` was already initialised
    // to `needsAi`, and the AiRequiredNotice handles the rest.
    if (!aiEnabled) return;
    if (invokePromiseRef.current === null) {
      invokePromiseRef.current = invoke<SuggestResult>("suggest_spam_rules", {
        messageIds: idsRef.current,
      });
    }
    let cancelled = false;
    invokePromiseRef.current
      .then((result) => {
        if (cancelled) return;
        if (result.drafts.length === 0) {
          setPhase({ kind: "noSuggestions", result });
        } else {
          setPhase({ kind: "suggested", result });
        }
      })
      .catch((e) => {
        if (cancelled) return;
        const msg = String(e);
        // Backend uses this sentinel string when the user hit the
        // cancel button — distinguish from real failures so we can
        // show a friendlier state.
        if (msg.includes(CANCEL_SENTINEL)) {
          setPhase({ kind: "cancelled" });
        } else if (isAiDisabledError(e)) {
          // AI got toggled off between dialog open and pi response —
          // race window is small but possible.
          setPhase({ kind: "needsAi" });
        } else {
          setPhase({ kind: "error", message: msg });
        }
      });
    return () => {
      cancelled = true;
      // Deliberately NOT calling `cancel_spam_analysis` here. In
      // StrictMode dev-mode this cleanup fires between the two mount
      // runs and would kill the pi subprocess before the second run's
      // .then/.catch listener ever sees a result. Cancellation is the
      // business of `onCancel` / `handleClose` below — explicit user
      // action only.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [aiEnabled]);

  /** Bound to the in-dialog "Abbrechen"-button during the analyzing phase. */
  const onCancel = () => {
    void invoke("cancel_spam_analysis").catch(() => {});
  };

  /**
   * Close-intent handler: the user wants the dialog gone. If pi is
   * still chewing we signal cancel to the backend so the subprocess
   * doesn't burn CPU in the background. Wrapped so the overlay click,
   * the "✕" button, and the "Fertig"-footer-button all share one path.
   */
  const handleClose = () => {
    if (phase.kind === "analyzing") {
      void invoke("cancel_spam_analysis").catch(() => {});
    }
    onClose();
  };

  const applyDraft = async (idx: number, draft: RuleDraft) => {
    setApplyingIdx(idx);
    try {
      const res = await invoke<ApplyRuleResult>("apply_spam_rule", { draft });
      setApplyResults((prev) => ({ ...prev, [idx]: res }));
      setAppliedIndices((prev) => new Set(prev).add(idx));
      onApplied();
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    } finally {
      setApplyingIdx(null);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center px-4"
      style={{ background: "rgba(0,0,0,0.45)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) handleClose();
      }}
    >
      <div
        role="dialog"
        aria-label={t("learnSpam.title")}
        className="flex max-h-[92vh] w-full max-w-2xl flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <header
          className="flex items-center justify-between border-b px-5 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <div>
            <h2 className="text-base font-semibold">{t("learnSpam.title")}</h2>
            <p
              className="mt-0.5 text-[11px]"
              style={{ color: "var(--fg-muted)" }}
            >
              {t("learnSpam.subtitle", { count: candidateIds.length })}
            </p>
          </div>
          <button
            type="button"
            onClick={handleClose}
            aria-label={t("learnSpam.close")}
            className="rounded-md px-2 py-1 text-sm"
            style={{ color: "var(--fg-muted)" }}
          >
            ✕
          </button>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          {/* === RESULT FIRST — the whole point of the dialog. Whatever
               pi produced or why it couldn't, that's the first thing
               the user should see when they scroll open this panel. === */}

          {phase.kind === "needsAi" && (
            <AiRequiredNotice
              onOpenAiSettings={
                onOpenAiSettings
                  ? () => {
                      onClose();
                      onOpenAiSettings();
                    }
                  : undefined
              }
            />
          )}

          {phase.kind === "analyzing" && (
            <div
              className="flex items-center gap-3 rounded-md border px-4 py-3 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
              }}
            >
              <span
                aria-hidden
                className="inline-block"
                style={{ animation: "cm-spin 1.2s linear infinite" }}
              >
                ↻
              </span>
              <span
                className="flex-1"
                style={{ color: "var(--fg-muted)" }}
              >
                {t("learnSpam.analyzing")}
              </span>
              <button
                type="button"
                onClick={onCancel}
                className="rounded-md border px-2 py-1 text-[11px]"
                style={{
                  borderColor: "var(--border-base)",
                  color: "var(--fg-muted)",
                }}
              >
                {t("learnSpam.cancel")}
              </button>
            </div>
          )}

          {phase.kind === "cancelled" && (
            <div
              className="rounded-md px-4 py-3 text-sm"
              style={{
                background: "rgba(148,163,184,0.10)",
                border: "1px solid var(--border-soft)",
                color: "var(--fg-muted)",
              }}
            >
              {t("learnSpam.cancelled")}
            </div>
          )}

          {phase.kind === "error" && (
            <div
              className="rounded-md px-3 py-2 text-xs"
              style={{
                background: "rgba(248,113,113,0.12)",
                color: "#ef4444",
                border: "1px solid rgba(248,113,113,0.25)",
              }}
            >
              {phase.message}
            </div>
          )}

          {phase.kind === "suggested" && (
            <>
              <div
                className="mb-2 text-xs uppercase tracking-wider"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("learnSpam.proposals", {
                  count: phase.result.drafts.length,
                })}
              </div>
              <ul className="flex flex-col gap-2">
                {phase.result.drafts.map((draft, idx) => {
                  const applied = appliedIndices.has(idx);
                  const loading = applyingIdx === idx;
                  const result = applyResults[idx];
                  return (
                    <li
                      key={idx}
                      className="rounded-md border p-3"
                      style={{
                        borderColor: applied
                          ? "rgba(16,185,129,0.4)"
                          : "var(--border-soft)",
                        background: "var(--bg-base)",
                      }}
                    >
                      <ProposalCard
                        draft={draft}
                        applied={applied}
                        loading={loading}
                        result={result}
                        onApply={() => void applyDraft(idx, draft)}
                      />
                    </li>
                  );
                })}
              </ul>
              <RawResponseToggle
                showRaw={showRaw}
                onToggle={() => setShowRaw((v) => !v)}
                raw={phase.result.rawResponse}
              />
            </>
          )}

          {phase.kind === "noSuggestions" && (
            <>
              <div
                className="rounded-md px-4 py-3 text-sm"
                style={{
                  background: "rgba(234,179,8,0.10)",
                  border: "1px solid rgba(234,179,8,0.3)",
                  color: "var(--fg-base)",
                }}
              >
                <div className="font-medium">
                  {t("learnSpam.noSuggestionsTitle")}
                </div>
                <div
                  className="mt-1 text-[11px]"
                  style={{ color: "var(--fg-muted)" }}
                >
                  {t("learnSpam.noSuggestions")}
                </div>
              </div>
              {/* When pi didn't yield a usable rule, auto-expand the raw
                  response. It's the only useful artefact in this state
                  and hiding it behind a toggle makes the dialog feel
                  empty. */}
              <div className="mt-5">
                <div
                  className="mb-1 text-xs uppercase tracking-wider"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("learnSpam.rawResponseHeading")}
                </div>
                <pre
                  className="max-h-48 overflow-y-auto whitespace-pre-wrap rounded-md border p-2 font-mono text-[11px]"
                  style={{
                    borderColor: "var(--border-soft)",
                    background: "var(--bg-base)",
                    color: "var(--fg-muted)",
                  }}
                >
                  {phase.result.rawResponse.trim() ||
                    t("learnSpam.rawResponseEmpty")}
                </pre>
              </div>
            </>
          )}

          {/* === CONTEXT BELOW — the mail corpus pi was analysing, shown
               as reference / collapsible detail so the user understands
               *what* the proposal is a reaction to. === */}
          {(phase.kind === "analyzing" ||
            phase.kind === "suggested" ||
            phase.kind === "noSuggestions") && (
            <details className="mt-5">
              <summary
                className="cursor-pointer text-xs uppercase tracking-wider select-none"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("learnSpam.corpus")}{" "}
                <span style={{ color: "var(--fg-muted)" }}>
                  (
                  {phase.kind === "analyzing"
                    ? candidateIds.length
                    : phase.result.features.length}
                  )
                </span>
              </summary>
              <div className="mt-2">
                <FeaturesSection
                  features={
                    phase.kind === "analyzing" ? null : phase.result.features
                  }
                />
              </div>
            </details>
          )}
        </div>

        <footer
          className="flex items-center justify-end gap-2 border-t px-5 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <button
            type="button"
            onClick={handleClose}
            className="rounded-md px-4 py-1.5 text-sm"
            style={{ color: "var(--fg-muted)" }}
          >
            {t("learnSpam.done")}
          </button>
        </footer>
      </div>
    </div>
  );
}

function FeaturesSection({
  features,
}: {
  features: SuggestResult["features"] | null;
}) {
  const { t } = useTranslation();
  // No inner heading — the parent (<details summary>) already labels
  // this block. Keeps the layout tight when collapsed/expanded.
  return (
    <section>
      {features === null ? (
        <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
          …
        </p>
      ) : (
        <ul
          className="flex flex-col overflow-hidden rounded-md border text-[12px]"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
          }}
        >
          {features.map((f, i) => (
            <li
              key={f.messageId}
              className={i === 0 ? "" : "border-t"}
              style={{ borderColor: "var(--border-soft)" }}
            >
              <div className="px-3 py-2">
                <div className="flex items-baseline gap-2">
                  <span
                    className="truncate font-medium"
                    style={{ color: "var(--fg-base)" }}
                  >
                    {f.subject || t("spam.noSubject")}
                  </span>
                </div>
                <div
                  className="mt-0.5 truncate text-[11px]"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {f.fromEmail}
                </div>
                {f.relevantHeaders.length > 0 && (
                  <ul
                    className="mt-1 flex flex-col gap-0.5 font-mono text-[10px]"
                    style={{ color: "var(--fg-muted)" }}
                  >
                    {f.relevantHeaders.map((h, j) => (
                      <li key={j} className="truncate">
                        {h}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function ProposalCard({
  draft,
  applied,
  loading,
  result,
  onApply,
}: {
  draft: RuleDraft;
  applied: boolean;
  loading: boolean;
  result: ApplyRuleResult | undefined;
  onApply: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-baseline gap-2">
        <span
          className="rounded bg-black/5 px-1.5 py-0.5 text-[10px] font-semibold uppercase"
          style={{ color: "var(--fg-muted)" }}
        >
          {t(`spam.patternType.${camel(draft.patternType)}`)}
        </span>
        <span className="truncate font-mono text-[12px]">{draft.pattern}</span>
        {draft.confidence != null && (
          <span
            className="ml-auto text-[10px]"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("spam.confidence", {
              percent: Math.round(draft.confidence * 100),
            })}
          </span>
        )}
      </div>
      {draft.reason && (
        <p className="text-[11px]" style={{ color: "var(--fg-muted)" }}>
          {draft.reason}
        </p>
      )}
      <div className="flex items-center gap-2">
        {applied ? (
          <span
            className="rounded-md px-3 py-1 text-xs"
            style={{
              background: "rgba(16,185,129,0.15)",
              color: "#10b981",
              border: "1px solid rgba(16,185,129,0.3)",
            }}
          >
            {result
              ? `${t("learnSpam.applied", {
                  moved: result.moved,
                  matched: result.matched,
                })}${
                  result.alreadyInSpam > 0
                    ? " " +
                      t("spam.appliedAlreadyInSpam", {
                        count: result.alreadyInSpam,
                      })
                    : ""
                }`
              : t("learnSpam.appliedGeneric")}
          </span>
        ) : (
          <button
            type="button"
            onClick={onApply}
            disabled={loading}
            className="rounded-md px-3 py-1.5 text-xs font-medium disabled:opacity-50"
            style={{ background: "var(--accent)", color: "white" }}
          >
            {loading ? t("spam.applying") : t("learnSpam.applyProposal")}
          </button>
        )}
      </div>
    </div>
  );
}

function RawResponseToggle({
  showRaw,
  onToggle,
  raw,
}: {
  showRaw: boolean;
  onToggle: () => void;
  raw: string;
}) {
  const { t } = useTranslation();
  return (
    <div className="mt-5">
      <button
        type="button"
        onClick={onToggle}
        className="text-[11px] underline"
        style={{ color: "var(--fg-subtle)" }}
      >
        {showRaw ? t("learnSpam.hideRaw") : t("learnSpam.showRaw")}
      </button>
      {showRaw && (
        <pre
          className="mt-2 max-h-48 overflow-y-auto whitespace-pre-wrap rounded-md border p-2 font-mono text-[10px]"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-muted)",
          }}
        >
          {raw}
        </pre>
      )}
    </div>
  );
}

function camel(s: string): string {
  return s.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
}
