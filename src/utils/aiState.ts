import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * Sentinel error string the backend returns from every AI entry point
 * (chat, spam analysis, workflow training) when the user has flipped
 * the master kill-switch off. Mirrors `AI_DISABLED_ERR` in
 * `src-tauri/src/commands/pi.rs` — keep them in sync.
 */
export const AI_DISABLED_ERR = "ai_disabled";

/** True when the given thrown value is the AI-disabled sentinel. */
export function isAiDisabledError(e: unknown): boolean {
  return String(e).includes(AI_DISABLED_ERR);
}

/**
 * DOM-level event the settings switch (and any future toggle entry
 * point) dispatches when the master AI flag flips. Components that
 * mirror the flag use it to update without polling.
 */
export const AI_ENABLED_CHANGED_EVENT = "cm:ai-enabled-changed";

type AiEnabledChangedDetail = { enabled: boolean };

/**
 * React hook giving components a live view of the master AI flag.
 *
 * Source of truth lives in the Rust state (`PiConfig.enabled`,
 * persisted to `pi_config.json`). On mount we fetch it once via the
 * `get_ai_enabled` Tauri command; thereafter we listen on the
 * `cm:ai-enabled-changed` window event and rely on whoever flips the
 * flag to dispatch one — see `PiSettings.toggleAi` and
 * `App.tsx#footer toggle`.
 *
 * Returns `[enabled, setEnabled]` where `setEnabled(next)` does the
 * full round-trip: optimistic local update → Tauri call → broadcast
 * event so other instances of the hook see it. Failures roll back.
 */
export function useAiEnabled(): [boolean, (next: boolean) => Promise<void>] {
  // Default to `true` so a slow first fetch doesn't briefly render the
  // "off" state before we know the truth.
  const [enabled, setEnabledLocal] = useState<boolean>(true);

  useEffect(() => {
    let cancelled = false;
    void invoke<boolean>("get_ai_enabled")
      .then((v) => {
        if (!cancelled) setEnabledLocal(v);
      })
      .catch(() => {
        // Backend not ready or command missing — leave the optimistic
        // default in place. Worst case the user sees "on" while it's
        // actually "off"; the next AI call will surface the sentinel.
      });
    const onChange = (e: Event) => {
      const detail = (e as CustomEvent<AiEnabledChangedDetail>).detail;
      if (typeof detail?.enabled === "boolean") {
        setEnabledLocal(detail.enabled);
      }
    };
    window.addEventListener(AI_ENABLED_CHANGED_EVENT, onChange);
    return () => {
      cancelled = true;
      window.removeEventListener(AI_ENABLED_CHANGED_EVENT, onChange);
    };
  }, []);

  const setEnabled = async (next: boolean): Promise<void> => {
    const prev = enabled;
    setEnabledLocal(next);
    try {
      await invoke("set_ai_enabled", { enabled: next });
      window.dispatchEvent(
        new CustomEvent(AI_ENABLED_CHANGED_EVENT, {
          detail: { enabled: next },
        }),
      );
    } catch (e) {
      setEnabledLocal(prev);
      throw e;
    }
  };

  return [enabled, setEnabled];
}
