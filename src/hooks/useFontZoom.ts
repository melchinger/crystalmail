import { useCallback, useEffect, useState } from "react";

/**
 * Global font-size zoom with Ctrl+/-, Ctrl+0 reset, and Ctrl+Wheel support.
 *
 * Why `html { font-size }` rather than `transform: scale` or a layout wrapper?
 *   * Tailwind and every `rem`/`em`-based length (padding, margin, gaps)
 *     automatically follows. One knob, everything tracks.
 *   * Pixel-based components (iframe chrome, fixed-width images) stay sharp
 *     at their original resolution — `scale` would blur them.
 *   * Browser hit-testing and scrollbars remain correct.
 *
 * The root element inherits `16px` by default; we move that up/down in 1px
 * steps between `MIN` and `MAX`. Persisted to `localStorage` so the setting
 * survives app restarts without needing to round-trip the Tauri store.
 */

const STORAGE_KEY = "crystalmail:fontSize";
const DEFAULT_SIZE = 16;
const MIN_SIZE = 10;
const MAX_SIZE = 28;
const STEP = 1;

function clamp(n: number): number {
  if (Number.isNaN(n)) return DEFAULT_SIZE;
  return Math.max(MIN_SIZE, Math.min(MAX_SIZE, Math.round(n)));
}

function loadInitial(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw == null) return DEFAULT_SIZE;
    return clamp(Number.parseFloat(raw));
  } catch {
    return DEFAULT_SIZE;
  }
}

export function useFontZoom() {
  const [size, setSize] = useState<number>(loadInitial);

  // Reflect into the DOM and persist.
  useEffect(() => {
    document.documentElement.style.fontSize = `${size}px`;
    try {
      localStorage.setItem(STORAGE_KEY, String(size));
    } catch {
      // localStorage may be disabled in some runtimes (private mode, webviews
      // with storage partitioning). Not a fatal condition — the current
      // session still works, the setting just won't persist.
    }
  }, [size]);

  const bumpBy = useCallback((delta: number) => {
    setSize((s) => clamp(s + delta));
  }, []);
  const reset = useCallback(() => setSize(DEFAULT_SIZE), []);

  // Keyboard: Ctrl/Cmd + Plus / Minus / 0. We listen at window level and
  // swallow the native browser zoom so the app controls its own scale.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      // Covers the main-row `+`/`-`/`=` as well as the numpad.
      switch (e.key) {
        case "+":
        case "=":
          e.preventDefault();
          bumpBy(STEP);
          break;
        case "-":
        case "_":
          e.preventDefault();
          bumpBy(-STEP);
          break;
        case "0":
          e.preventDefault();
          reset();
          break;
        default:
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [bumpBy, reset]);

  // Wheel: Ctrl+ScrollUp = bigger, Ctrl+ScrollDown = smaller. `passive: false`
  // is required to let us preventDefault the page-zoom default. We also don't
  // want to zoom while the user is scrolling a content area without Ctrl.
  useEffect(() => {
    const onWheel = (e: WheelEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      e.preventDefault();
      // Normalize deltaY: trackpads produce fractional values; any negative
      // delta ("up") grows, positive shrinks.
      bumpBy(e.deltaY < 0 ? STEP : -STEP);
    };
    window.addEventListener("wheel", onWheel, { passive: false });
    return () => window.removeEventListener("wheel", onWheel);
  }, [bumpBy]);

  return { size, bumpBy, reset, min: MIN_SIZE, max: MAX_SIZE, default: DEFAULT_SIZE };
}
