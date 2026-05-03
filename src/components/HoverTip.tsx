import { useState, type ReactNode } from "react";

/**
 * Minimal hover-tooltip wrapper. Purpose-built because the native
 * HTML `title` attribute isn't reactive — the OS tooltip renders the
 * value that was present when the mouse entered the element, and
 * subsequent updates are ignored until the next hover cycle. That
 * caught us with the sync-progress tooltip which needs to tick
 * live while the user holds the pointer still.
 *
 * Design choices:
 *   - No portal / positioning library — absolute positioning against
 *     the wrapping `div` is enough for our two call sites (sidebar
 *     icons, reader toolbar).
 *   - Shown after a small delay to avoid flicker on drive-by hovers,
 *     mirrors the native tooltip feel.
 *   - Empty / undefined `label` hides the tip entirely so callers can
 *     conditionally suppress it without wrapping everything in a
 *     ternary.
 */
type Side = "bottom" | "right";

export function HoverTip({
  children,
  label,
  side = "bottom",
  delayMs = 350,
}: {
  children: ReactNode;
  label: string | undefined;
  side?: Side;
  delayMs?: number;
}) {
  const [open, setOpen] = useState(false);
  const [timer, setTimer] = useState<number | null>(null);

  const enter = () => {
    if (!label) return;
    if (timer !== null) window.clearTimeout(timer);
    const id = window.setTimeout(() => setOpen(true), delayMs);
    setTimer(id);
  };
  const leave = () => {
    if (timer !== null) {
      window.clearTimeout(timer);
      setTimer(null);
    }
    setOpen(false);
  };

  // Absolute position rules: below for most toolbar-style icons,
  // right-of-target for sidebar-header buttons that are already at the
  // top edge of the window (no room below).
  const position =
    side === "right"
      ? "left-full top-1/2 -translate-y-1/2 ml-2"
      : "top-full left-1/2 -translate-x-1/2 mt-1.5";

  return (
    <span
      className="relative inline-flex"
      onMouseEnter={enter}
      onMouseLeave={leave}
    >
      {children}
      {open && label && (
        <span
          role="tooltip"
          className={`pointer-events-none absolute ${position} z-50 whitespace-nowrap rounded-md border px-2 py-1 text-[11px] shadow`}
          style={{
            background: "var(--bg-panel)",
            color: "var(--fg-base)",
            borderColor: "var(--border-base)",
          }}
        >
          {label}
        </span>
      )}
    </span>
  );
}
