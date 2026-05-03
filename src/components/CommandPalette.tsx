import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  HOTKEY_ACTION_IDS,
  HOTKEY_ACTIONS,
  type HotkeyActionId,
  type HotkeyBindings,
} from "../settings/hotkeys";
import { dispatchHotkeyAction, type HotkeyCallbacks } from "../hooks/useHotkeys";

type Props = {
  bindings: HotkeyBindings;
  /**
   * Callbacks for app-scoped actions. Same shape as the one passed to
   * `useHotkeys` — the palette routes both keypresses and clicks through
   * `dispatchHotkeyAction`, so picking from the list is indistinguishable
   * from pressing the bound key. Closing the palette is the caller's
   * concern, not the dispatcher's.
   */
  callbacks: HotkeyCallbacks;
  /**
   * True when a message is selected in the inbox list. Message-scoped
   * actions (reply/archive/delete/…) are dimmed and unselectable when
   * this is false — the dispatched event would have no MessageView
   * mounted to receive it.
   */
  hasSelection: boolean;
  onClose: () => void;
};

/**
 * Searchable command list bound to the `/` key. Lists every entry from
 * the hotkey registry — name, group, currently-bound combos — and runs
 * the matching action when the user picks one. Same UI shape as the
 * Move-to-Folder dialog so the keyboard muscle memory carries over:
 * autofocused search input, ↑/↓ to navigate, Enter to commit, Esc to
 * close, click also commits.
 *
 * The palette itself is hidden from the list it shows — picking
 * "Befehlspalette öffnen" while it's open would just close-and-reopen
 * for no benefit.
 */
export function CommandPalette({
  bindings,
  callbacks,
  hasSelection,
  onClose,
}: Props) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  // Pre-compute the candidate rows once per binding/selection change. The
  // expensive part is the i18n lookup + sort; the filter below is cheap.
  const allRows = useMemo(() => {
    type Row = {
      id: HotkeyActionId;
      label: string;
      group: "message" | "app";
      combos: string[];
      enabled: boolean;
    };
    const rows: Row[] = HOTKEY_ACTION_IDS.filter(
      // Hide the palette opener itself — picking it from inside the
      // open palette would be a recursive no-op.
      (id) => id !== "commandPalette",
    ).map((id) => {
      const meta = HOTKEY_ACTIONS[id];
      const combos = bindings[id] ?? [];
      const enabled = meta.group === "app" || hasSelection;
      return {
        id,
        label: t(meta.labelKey),
        group: meta.group,
        combos,
        enabled,
      };
    });
    // Sort: enabled first (so disabled rows sink to the bottom), then by
    // group (app actions before message actions — app actions are the
    // ones that always work), then by label alphabetically. Keeps the
    // list predictable as the user types.
    rows.sort((a, b) => {
      if (a.enabled !== b.enabled) return a.enabled ? -1 : 1;
      if (a.group !== b.group) return a.group === "app" ? -1 : 1;
      return a.label.localeCompare(b.label);
    });
    return rows;
  }, [bindings, hasSelection, t]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return allRows;
    return allRows.filter((r) => {
      // Match on the label and on each combo string so the user can
      // type "r" or "antworten" and find Reply both ways.
      if (r.label.toLowerCase().includes(q)) return true;
      if (r.combos.some((c) => c.toLowerCase().includes(q))) return true;
      return false;
    });
  }, [allRows, query]);

  // Keep the cursor inside the filtered range and, ideally, on the first
  // *enabled* row so Enter does something useful.
  useEffect(() => {
    if (filtered.length === 0) {
      setCursor(0);
      return;
    }
    if (cursor >= filtered.length) {
      const firstEnabled = filtered.findIndex((r) => r.enabled);
      setCursor(firstEnabled >= 0 ? firstEnabled : filtered.length - 1);
    }
  }, [filtered, cursor]);

  // Reset cursor when the query changes — the row at index 0 of the
  // filtered list is most likely what the user wants.
  useEffect(() => {
    const firstEnabled = filtered.findIndex((r) => r.enabled);
    setCursor(firstEnabled >= 0 ? firstEnabled : 0);
  }, [query]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLLIElement>(
      `li[data-cursor="${cursor}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const commit = (idx: number) => {
    const row = filtered[idx];
    if (!row || !row.enabled) return;
    onClose();
    // Run the action *after* close so any modal the action opens
    // (compose, settings, move-picker) replaces the palette cleanly
    // instead of stacking on top.
    dispatchHotkeyAction(row.id, callbacks);
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center px-4 pt-[14vh]"
      style={{ background: "rgba(0,0,0,0.45)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-label={t("palette.title")}
        className="flex w-full max-w-md flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            // Skip past disabled rows during keyboard nav so Enter
            // always lands on something actionable.
            setCursor((c) => {
              for (let i = c + 1; i < filtered.length; i++) {
                if (filtered[i].enabled) return i;
              }
              return c;
            });
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setCursor((c) => {
              for (let i = c - 1; i >= 0; i--) {
                if (filtered[i].enabled) return i;
              }
              return c;
            });
          } else if (e.key === "Enter") {
            e.preventDefault();
            commit(cursor);
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
          }
        }}
      >
        <div
          className="flex items-center gap-2 border-b px-3 py-2"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <span
            aria-hidden
            className="text-sm"
            style={{ color: "var(--fg-subtle)" }}
          >
            /
          </span>
          <input
            ref={inputRef}
            autoFocus
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("palette.placeholder")}
            className="flex-1 bg-transparent text-sm outline-none"
            style={{ color: "var(--fg-base)" }}
          />
          <span
            className="text-[10px] uppercase tracking-wider"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("palette.hint")}
          </span>
        </div>

        <div className="max-h-[50vh] overflow-y-auto">
          {filtered.length === 0 && (
            <div
              className="px-4 py-3 text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              {t("palette.noMatches")}
            </div>
          )}
          <ul ref={listRef} className="flex flex-col">
            {filtered.map((row, i) => {
              const isCursor = i === cursor;
              const groupLabel =
                row.group === "app"
                  ? t("palette.groupApp")
                  : t("palette.groupMessage");
              return (
                <li
                  key={row.id}
                  data-cursor={i}
                  onMouseEnter={() => row.enabled && setCursor(i)}
                  onClick={() => row.enabled && commit(i)}
                  className="flex items-center gap-2 px-3 py-1.5 text-sm"
                  style={{
                    cursor: row.enabled ? "pointer" : "not-allowed",
                    background: isCursor
                      ? "var(--bg-selected)"
                      : "transparent",
                    color: !row.enabled
                      ? "var(--fg-subtle)"
                      : isCursor
                        ? "var(--accent)"
                        : "var(--fg-base)",
                    opacity: row.enabled ? 1 : 0.55,
                  }}
                  title={
                    !row.enabled ? t("palette.needsSelection") : undefined
                  }
                >
                  {/* Group bullet — same shape as the move dialog's. */}
                  <span
                    aria-hidden
                    className="w-3 shrink-0 text-center text-[11px]"
                    style={{
                      color: isCursor ? "var(--accent)" : "var(--fg-subtle)",
                    }}
                  >
                    {row.group === "app" ? "◆" : "·"}
                  </span>
                  <span className="truncate">{row.label}</span>
                  <span
                    className="ml-auto flex shrink-0 items-center gap-1.5 text-[10px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    <span className="uppercase tracking-wider">
                      {groupLabel}
                    </span>
                    <span>·</span>
                    {row.combos.length === 0 ? (
                      <span>{t("palette.noBinding")}</span>
                    ) : (
                      <span className="flex items-center gap-1">
                        {row.combos.map((c, ci) => (
                          <kbd
                            key={ci}
                            className="rounded border px-1.5 py-0.5 font-mono text-[10px]"
                            style={{
                              borderColor: "var(--border-base)",
                              background: "var(--bg-base)",
                              color: "var(--fg-base)",
                            }}
                          >
                            {c}
                          </kbd>
                        ))}
                      </span>
                    )}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      </div>
    </div>
  );
}
