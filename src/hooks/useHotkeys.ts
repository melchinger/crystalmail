import { useEffect, useRef } from "react";
import {
  bindingsToLookup,
  normalizeKeyEvent,
  type HotkeyActionId,
  type HotkeyBindings,
} from "../settings/hotkeys";

/**
 * Global hotkey dispatcher driven by the user-editable registry.
 *
 * The hook takes a `bindings` map and a set of callbacks; on each keydown
 * it normalizes the event to a combo string, looks up which action (if
 * any) owns that combo, and invokes the matching callback. Message-scoped
 * actions (reply, archive, …) are dispatched as `CustomEvent`s on
 * `window` so the Reader can subscribe without the App having to manage
 * MessageDetail state.
 *
 * The same dispatch table powers the command palette (`/`): it shows
 * every action and, on commit, calls `dispatchHotkeyAction` so a
 * mouse pick walks the same path as a keypress.
 */

export const HOTKEY_EVENTS: Record<HotkeyActionId, string> = {
  reply: "cm:hotkey:reply",
  replyAll: "cm:hotkey:replyAll",
  forward: "cm:hotkey:forward",
  archive: "cm:hotkey:archive",
  delete: "cm:hotkey:delete",
  move: "cm:hotkey:move",
  markSpam: "cm:hotkey:markSpam",
  spamCandidate: "cm:hotkey:spamCandidate",
  toggleRead: "cm:hotkey:toggleRead",
  markUnread: "cm:hotkey:markUnread",
  markAllRead: "cm:hotkey:markAllRead",
  newMail: "cm:hotkey:newMail",
  sync: "cm:hotkey:sync",
  help: "cm:hotkey:help",
  settings: "cm:hotkey:settings",
  workflow: "cm:hotkey:workflow",
  trainingCandidate: "cm:hotkey:trainingCandidate",
  commandPalette: "cm:hotkey:commandPalette",
};

export type HotkeyCallbacks = {
  onCompose: () => void;
  onSyncAll: () => void;
  onMarkAllRead: () => void;
  onShowHelp: () => void;
  onShowSettings: () => void;
  onShowCommandPalette: () => void;
  onEscape: () => void;
};

/**
 * Run a hotkey action. Extracted so the command palette can re-use the
 * same dispatch path — picking "Antworten" from the palette walks the
 * exact same code as pressing `r`.
 *
 * Message-scoped actions (reply/archive/delete/…) are routed through a
 * `CustomEvent` on `window`. The Reader subscribes to those events with
 * the message detail in closure, so the dispatcher doesn't need to
 * know about the selected message.
 */
export function dispatchHotkeyAction(
  action: HotkeyActionId,
  cb: HotkeyCallbacks,
): void {
  switch (action) {
    case "newMail":
      cb.onCompose();
      return;
    case "sync":
      cb.onSyncAll();
      return;
    case "markAllRead":
      cb.onMarkAllRead();
      return;
    case "help":
      cb.onShowHelp();
      return;
    case "settings":
      cb.onShowSettings();
      return;
    case "commandPalette":
      cb.onShowCommandPalette();
      return;
    case "reply":
    case "replyAll":
    case "forward":
    case "archive":
    case "delete":
    case "move":
    case "markSpam":
    case "spamCandidate":
    case "toggleRead":
    case "markUnread":
    case "workflow":
    case "trainingCandidate":
      window.dispatchEvent(new CustomEvent(HOTKEY_EVENTS[action]));
      return;
  }
}

function isTypingTarget(t: EventTarget | null): boolean {
  if (!(t instanceof HTMLElement)) return false;
  const tag = t.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  if (t.isContentEditable) return true;
  return false;
}

export function useHotkeys(bindings: HotkeyBindings, cb: HotkeyCallbacks) {
  // Rebuild the lookup whenever bindings change. A ref lets the handler
  // read the latest version without re-registering the listener each
  // time (which would thrash a lot in settings rebinding).
  const lookupRef = useRef(bindingsToLookup(bindings));
  useEffect(() => {
    lookupRef.current = bindingsToLookup(bindings);
  }, [bindings]);

  const cbRef = useRef(cb);
  useEffect(() => {
    cbRef.current = cb;
  }, [cb]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // Escape is always live so the user can dismiss dialogs even from
      // inside an input.
      if (e.key === "Escape") {
        cbRef.current.onEscape();
        return;
      }
      if (isTypingTarget(e.target)) return;

      const combo = normalizeKeyEvent(e);
      if (!combo) return;

      const action = lookupRef.current[combo];
      if (!action) return;

      e.preventDefault();
      dispatchHotkeyAction(action, cbRef.current);
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
