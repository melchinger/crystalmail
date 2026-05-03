import { useTranslation } from "react-i18next";

type Props = {
  /** Optional jump-to-settings handler. When omitted, the button isn't
   *  rendered — useful when there's no good way to navigate (e.g. the
   *  notice appears in a context where settings can't be opened
   *  cleanly). */
  onOpenAiSettings?: () => void;
};

/**
 * Inline notice rendered in dialogs that need AI but can't run because
 * the master kill-switch is off (or the pi config is broken).
 *
 * Used from:
 *   * `LearnSpamRuleDialog` — auto-runs spam analysis on mount; we
 *     short-circuit to this notice when `aiEnabled === false`.
 *   * `WorkflowTrainingDialog` — replaces the "Training starten" button
 *     in the prompt phase when AI is off.
 *
 * Both call sites pass a `onOpenAiSettings` that closes the current
 * dialog and switches Settings to the pi tab — one click from notice
 * to fix.
 */
export function AiRequiredNotice({ onOpenAiSettings }: Props) {
  const { t } = useTranslation();
  return (
    <div
      className="flex flex-col gap-3 rounded-md border px-4 py-3"
      style={{
        borderColor: "rgba(239,68,68,0.45)",
        background: "rgba(239,68,68,0.08)",
        color: "var(--fg-base)",
      }}
    >
      <p className="text-sm font-semibold">
        {t("aiRequired.title")}
      </p>
      <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
        {t("aiRequired.body")}
      </p>
      {onOpenAiSettings && (
        <div>
          <button
            type="button"
            onClick={onOpenAiSettings}
            className="rounded-md border px-3 py-1.5 text-xs"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-base)",
              color: "var(--fg-base)",
            }}
          >
            {t("aiRequired.openSettings")}
          </button>
        </div>
      )}
    </div>
  );
}
