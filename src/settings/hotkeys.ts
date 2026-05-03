/**
 * Hotkey registry: the single source of truth for which action is bound to
 * which key combo. The `useHotkeys` hook reads from here; the Settings UI
 * writes here. Persisted to `localStorage` — same pattern as the font zoom,
 * no Tauri store needed for pure UI preferences.
 */

export const HOTKEY_ACTION_IDS = [
  "reply",
  "replyAll",
  "forward",
  "archive",
  "delete",
  "move",
  "markSpam",
  "spamCandidate",
  "toggleRead",
  "markUnread",
  "markAllRead",
  "newMail",
  "sync",
  "help",
  "settings",
  "workflow",
  "trainingCandidate",
  "commandPalette",
] as const;

export type HotkeyActionId = (typeof HOTKEY_ACTION_IDS)[number];

/**
 * Human-facing metadata. Labels are German-first and can be i18n-wrapped at
 * render time via the i18n keys in `hotkeys.actions.<id>`.
 */
export const HOTKEY_ACTIONS: Record<
  HotkeyActionId,
  { labelKey: string; group: "message" | "app" }
> = {
  reply: { labelKey: "hotkeys.reply", group: "message" },
  replyAll: { labelKey: "hotkeys.replyAll", group: "message" },
  forward: { labelKey: "hotkeys.forward", group: "message" },
  archive: { labelKey: "hotkeys.archive", group: "message" },
  delete: { labelKey: "hotkeys.delete", group: "message" },
  move: { labelKey: "hotkeys.move", group: "message" },
  markSpam: { labelKey: "hotkeys.markSpam", group: "message" },
  spamCandidate: { labelKey: "hotkeys.spamCandidate", group: "message" },
  toggleRead: { labelKey: "hotkeys.toggleRead", group: "message" },
  markUnread: { labelKey: "hotkeys.markUnread", group: "message" },
  markAllRead: { labelKey: "hotkeys.markAllRead", group: "message" },
  newMail: { labelKey: "hotkeys.newMail", group: "app" },
  sync: { labelKey: "hotkeys.sync", group: "app" },
  help: { labelKey: "hotkeys.help", group: "app" },
  settings: { labelKey: "hotkeys.settings", group: "app" },
  workflow: { labelKey: "hotkeys.workflow", group: "message" },
  trainingCandidate: {
    labelKey: "hotkeys.trainingCandidate",
    group: "message",
  },
  commandPalette: { labelKey: "hotkeys.commandPalette", group: "app" },
};

/**
 * Default bindings per action. Each action can have multiple accepted key
 * combos (e.g. Delete + # for delete, n + c for new). An empty array means
 * "this action is currently unbound" — valid, the user may have explicitly
 * cleared it.
 */
export const HOTKEY_DEFAULTS: Record<HotkeyActionId, string[]> = {
  reply: ["r"],
  replyAll: ["a"],
  forward: ["f"],
  archive: ["e"],
  delete: ["Delete", "#"],
  move: ["v"],
  markSpam: ["!"],
  spamCandidate: ["j"],
  toggleRead: ["u"],
  markUnread: ["Shift+U"],
  markAllRead: ["Shift+A"],
  newMail: ["n", "c"],
  sync: ["s"],
  help: ["?"],
  settings: [","],
  workflow: ["w"],
  trainingCandidate: ["t"],
  commandPalette: ["/"],
};

export type HotkeyBindings = Record<HotkeyActionId, string[]>;

const STORAGE_KEY = "crystalmail:hotkeys";

/**
 * Load the current bindings, falling back to defaults for any action the
 * stored blob doesn't list (so new actions we add in a release pick up a
 * default without the user having to reset).
 */
export function loadHotkeys(): HotkeyBindings {
  const merged: HotkeyBindings = { ...HOTKEY_DEFAULTS };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return merged;
    const parsed = JSON.parse(raw) as Partial<HotkeyBindings>;
    for (const id of HOTKEY_ACTION_IDS) {
      if (Array.isArray(parsed[id])) {
        merged[id] = parsed[id] as string[];
      }
    }
    return merged;
  } catch {
    return merged;
  }
}

export function saveHotkeys(b: HotkeyBindings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(b));
  } catch {
    // localStorage may be disabled; in-memory state still works for the session.
  }
}

export function resetHotkeys(): HotkeyBindings {
  const copy: HotkeyBindings = {} as HotkeyBindings;
  for (const id of HOTKEY_ACTION_IDS) {
    copy[id] = [...HOTKEY_DEFAULTS[id]];
  }
  saveHotkeys(copy);
  return copy;
}

/**
 * Build a reverse lookup: pressed-key string → action id. When multiple
 * actions claim the same key, the *later* one wins (Object.fromEntries
 * semantics); conflict resolution happens at the Settings UI level before
 * we ever reach here.
 */
export function bindingsToLookup(b: HotkeyBindings): Record<string, HotkeyActionId> {
  const out: Record<string, HotkeyActionId> = {};
  for (const id of HOTKEY_ACTION_IDS) {
    for (const key of b[id]) {
      if (key) out[key] = id;
    }
  }
  return out;
}

/**
 * Produce a stable string for a KeyboardEvent we can match against stored
 * bindings. Rules:
 *   * Ctrl/Alt/Meta are always prefixed when held.
 *   * Shift is prefixed *only* for letter keys (so "Shift+U" is explicit and
 *     distinct from "u"). For printable shifted characters like "?" or "#"
 *     we trust the keyboard layout's already-shifted `event.key` and omit
 *     the modifier — otherwise German/US layouts wouldn't share bindings.
 *   * Non-printable special keys (Delete, Escape, ArrowUp, F1 …) are
 *     captured verbatim with all modifiers prefixed.
 */
export function normalizeKeyEvent(e: KeyboardEvent): string | null {
  const k = e.key;
  if (k === "Shift" || k === "Control" || k === "Alt" || k === "Meta") {
    return null;
  }

  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.metaKey) mods.push("Meta");

  // Multi-character key names are always special (Delete, Enter, Tab, F1,
  // ArrowLeft …). Always fold Shift into the combo for these.
  if (k.length > 1) {
    if (e.shiftKey) mods.push("Shift");
    return [...mods, k].join("+");
  }

  // Single-character printable.
  if (/^[a-zA-Z]$/.test(k)) {
    if (e.shiftKey) {
      mods.push("Shift");
      return [...mods, k.toUpperCase()].join("+");
    }
    return [...mods, k.toLowerCase()].join("+");
  }

  // Digits, punctuation, symbols — the layout-produced key already reflects
  // what the user typed, so don't re-apply Shift.
  return [...mods, k].join("+");
}

/**
 * Pretty-print a binding string for the UI. Splits on "+" and lightly
 * localizes modifier names.
 */
export function formatBinding(binding: string): string {
  if (!binding) return "";
  return binding
    .split("+")
    .map((p) => {
      switch (p) {
        case "Ctrl":
          return "Strg";
        case "Meta":
          return "⌘";
        default:
          return p;
      }
    })
    .join(" + ");
}
