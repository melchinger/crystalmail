import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { Workflow } from "../types";

type Props = {
  /** Close without applying — Esc or backdrop click. */
  onClose: () => void;
  /** Called when the user picks a workflow. Receives the workflow id. */
  onPick: (workflowId: string) => void;
};

/**
 * Keyboard-driven picker for "apply workflow to the focused message".
 * Modelled on `MoveToDialog`: autofocused search input, ↑/↓ navigation,
 * Enter commits, Esc closes. Fetches `list_workflows` once on mount;
 * disabled workflows are filtered out so the picker only surfaces what
 * can actually run.
 *
 * The picker itself doesn't know about the target message — it returns
 * a workflow id to the caller, which then invokes `apply_workflow`.
 * That separation keeps the picker reusable (a future "apply to many"
 * flow just calls it the same way).
 */
export function WorkflowPicker({ onClose, onPick }: Props) {
  const { t } = useTranslation();
  const [workflows, setWorkflows] = useState<Workflow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await invoke<Workflow[]>("list_workflows");
        if (!cancelled) setWorkflows(list.filter((w) => w.enabled));
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const filtered = useMemo(() => {
    if (!workflows) return [];
    const q = query.trim().toLowerCase();
    if (!q) return workflows;
    return workflows.filter((w) => {
      // Match on name + hotkey; users often remember one or the other.
      return (
        w.name.toLowerCase().includes(q) ||
        (w.hotkey ?? "").toLowerCase().includes(q)
      );
    });
  }, [workflows, query]);

  useEffect(() => {
    if (cursor >= filtered.length) setCursor(Math.max(0, filtered.length - 1));
  }, [filtered, cursor]);

  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLLIElement>(
      `li[data-cursor="${cursor}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const commit = (w: Workflow | undefined) => {
    if (!w) return;
    onPick(w.id);
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
        aria-label={t("workflows.pickerTitle")}
        className="flex w-full max-w-md flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setCursor((c) => Math.min(c + 1, filtered.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setCursor((c) => Math.max(c - 1, 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            commit(filtered[cursor]);
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
            ⚙
          </span>
          <input
            ref={inputRef}
            autoFocus
            type="text"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setCursor(0);
            }}
            placeholder={t("workflows.pickerPlaceholder")}
            className="flex-1 bg-transparent text-sm outline-none"
            style={{ color: "var(--fg-base)" }}
          />
          <span
            className="text-[10px] uppercase tracking-wider"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("move.hint")}
          </span>
        </div>

        <div className="max-h-[50vh] overflow-y-auto">
          {error && (
            <div className="px-4 py-3 text-xs" style={{ color: "#ef4444" }}>
              {error}
            </div>
          )}
          {!error && workflows === null && (
            <div
              className="px-4 py-3 text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              …
            </div>
          )}
          {workflows !== null && workflows.length === 0 && (
            <div
              className="px-4 py-3 text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              {t("workflows.noWorkflows")}
            </div>
          )}
          {workflows !== null &&
            workflows.length > 0 &&
            filtered.length === 0 && (
              <div
                className="px-4 py-3 text-xs"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("move.noMatches")}
              </div>
            )}
          <ul ref={listRef} className="flex flex-col">
            {filtered.map((w, i) => {
              const isCursor = i === cursor;
              return (
                <li
                  key={w.id}
                  data-cursor={i}
                  onMouseEnter={() => setCursor(i)}
                  onClick={() => commit(w)}
                  className="flex cursor-pointer items-center gap-2 px-3 py-1.5 text-sm"
                  style={{
                    background: isCursor ? "var(--bg-selected)" : "transparent",
                    color: isCursor ? "var(--accent)" : "var(--fg-base)",
                  }}
                >
                  <span className="truncate">{w.name}</span>
                  <span
                    className="ml-auto shrink-0 text-[10px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {w.hotkey ?? `${w.steps.length} Steps`}
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
