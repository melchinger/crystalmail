import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ComposeSendSnapshot } from "./Compose";

type Props = {
  snapshot: ComposeSendSnapshot;
  /** Fires when the 5 s grace runs out — caller starts the real send. */
  onTimeout: (snap: ComposeSendSnapshot) => void;
  /** Fires when the user clicks "Abbrechen" — caller re-opens Compose. */
  onCancel: (snap: ComposeSendSnapshot) => void;
};

/** Buffer window for the undo-send pattern. Long enough that a "wait, no!"
 *  reaction to a misclicked Send still makes it; short enough that the user
 *  doesn't feel the mail is stuck in limbo. Gmail uses 5–30 s configurable;
 *  we go with the lower bound — every additional second is one the user
 *  has to wait for actual delivery. */
const UNDO_WINDOW_MS = 5_000;

/**
 * Bottom-anchored, non-blocking overlay that counts down to the actual SMTP
 * submit. Sits above the footer status line, far enough from the rest of
 * the UI that the user can keep working (browse mail, read threads) while
 * the timer ticks.
 *
 * Component logic:
 *   - mount: capture deadline = now + UNDO_WINDOW_MS, schedule the final
 *     setTimeout, also start a 100 ms re-render tick so the visible
 *     remaining-seconds value updates smoothly
 *   - cancel button: clear both timers, hand snapshot back to caller
 *   - countdown reaches 0: clear the rerender tick, fire onTimeout
 *   - unmount: clear both timers regardless (avoids stale fires from
 *     a snapshot the parent already took out of `pendingSend`)
 *
 * The countdown timer logic deliberately re-derives the remaining ms from
 * `Date.now()` rather than a state-counter, so a tab that gets backgrounded
 * (browser throttles 1 s timers down to ~minute-grade fires) still ends up
 * sending close to on-time when it wakes up.
 */
export function UndoSendOverlay({ snapshot, onTimeout, onCancel }: Props) {
  const { t } = useTranslation();
  const deadlineRef = useRef<number>(Date.now() + UNDO_WINDOW_MS);
  // Held in state so re-renders show fresh seconds-remaining; keep the
  // ref above as the source of truth so the final-fire timer doesn't
  // depend on render cadence.
  const [remainingMs, setRemainingMs] = useState<number>(UNDO_WINDOW_MS);

  useEffect(() => {
    deadlineRef.current = Date.now() + UNDO_WINDOW_MS;
    setRemainingMs(UNDO_WINDOW_MS);

    const tick = window.setInterval(() => {
      const left = Math.max(0, deadlineRef.current - Date.now());
      setRemainingMs(left);
      if (left <= 0) {
        window.clearInterval(tick);
      }
    }, 100);

    const fire = window.setTimeout(() => {
      onTimeout(snapshot);
    }, UNDO_WINDOW_MS);

    return () => {
      window.clearInterval(tick);
      window.clearTimeout(fire);
    };
    // We deliberately want a *fresh* timer when the snapshot identity
    // changes (a brand-new Send click after the previous one fired):
    // the deps include `snapshot`. `onTimeout` is captured by ref-style
    // pattern in the parent (useCallback), so referencing it here is
    // stable enough not to thrash this effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshot]);

  // Whole seconds count up from 1 → UNDO_WINDOW_MS / 1000 in the
  // reverse direction. Show the ceiling so "5" appears for the entire
  // first second (matches Gmail's UI, more reassuring than seeing "4"
  // 100 ms after the click).
  const secondsLeft = Math.max(1, Math.ceil(remainingMs / 1000));

  return (
    <div
      // The overlay is anchored at the bottom of the viewport but
      // doesn't grab pointer events globally — only the toast itself
      // is clickable. The user can keep browsing the inbox under it.
      className="pointer-events-none fixed inset-x-0 bottom-12 z-40 flex justify-center"
    >
      <div
        className="pointer-events-auto flex items-center gap-3 rounded-full border px-4 py-2 text-sm shadow-lg"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
        role="status"
        aria-live="polite"
      >
        <span aria-hidden style={{ color: "var(--accent)" }}>
          ↗
        </span>
        <span>
          {t("compose.undoSendCountdown", {
            count: secondsLeft,
            seconds: secondsLeft,
          })}
        </span>
        <button
          type="button"
          onClick={() => onCancel(snapshot)}
          className="rounded-md border px-2.5 py-0.5 text-xs"
          style={{
            borderColor: "var(--border-base)",
            color: "var(--accent)",
          }}
        >
          {t("compose.undoSendCancel")}
        </button>
      </div>
    </div>
  );
}
