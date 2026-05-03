import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { decodeImapFolderName, displayFolderName } from "../utils/imapFolderName";
import type { FolderSummary } from "../types";

type Props = {
  accountId: string;
  /** IMAP path of the folder the message currently lives in — excluded from the picker. */
  currentFolder?: string | null;
  onPick: (folderName: string) => void;
  onClose: () => void;
};

/**
 * Folder picker popup for the Move-to-Folder hotkey (`v`).
 *
 * Scope rules:
 *   * Destinations are always within the same account — IMAP can't move
 *     across servers, so cross-account picks are simply absent.
 *   * The source folder is filtered out (moving to yourself is a no-op).
 *
 * Interaction:
 *   * Autofocused search input, fuzzy substring match over both the
 *     display name and the full IMAP path.
 *   * ↑/↓ navigate, Enter commits, Esc closes.
 *   * Mouse click also commits; hover syncs the keyboard cursor so keyboard
 *     and pointer stay in agreement.
 */
export function MoveToDialog({
  accountId,
  currentFolder,
  onPick,
  onClose,
}: Props) {
  const { t } = useTranslation();
  const [folders, setFolders] = useState<FolderSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const rows = await invoke<FolderSummary[]>("list_account_folders", {
          accountId,
        });
        if (!cancelled) setFolders(rows);
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [accountId]);

  const filtered = useMemo(() => {
    if (!folders) return [];
    const base = folders.filter((f) => f.name !== currentFolder);
    const q = query.trim().toLowerCase();
    if (!q) return base;
    return base.filter((f) => {
      // Match on decoded + display + raw so users can search by the
      // umlaut-correct name ("entwürfe") or the raw server form.
      return (
        f.name.toLowerCase().includes(q) ||
        decodeImapFolderName(f.name).toLowerCase().includes(q) ||
        displayFolderName(f.name).toLowerCase().includes(q)
      );
    });
  }, [folders, query, currentFolder]);

  // Keep the cursor in range as the filter narrows. Scroll the active row
  // into view so the keyboard navigation is visible even with long lists.
  useEffect(() => {
    if (cursor >= filtered.length) setCursor(Math.max(0, filtered.length - 1));
  }, [filtered, cursor]);

  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLLIElement>(
      `li[data-cursor="${cursor}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  const commit = (f: FolderSummary | undefined) => {
    if (!f) return;
    // Hand the raw server-form name back so the backend can feed it to
    // `SELECT` without guessing. The decoded version is display-only.
    onPick(f.name);
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
        aria-label={t("move.title")}
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
            →
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
            placeholder={t("move.placeholder")}
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
            <div
              className="px-4 py-3 text-xs"
              style={{ color: "#ef4444" }}
            >
              {error}
            </div>
          )}
          {!error && folders === null && (
            <div
              className="px-4 py-3 text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              …
            </div>
          )}
          {folders !== null && filtered.length === 0 && (
            <div
              className="px-4 py-3 text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              {t("move.noMatches")}
            </div>
          )}
          <ul ref={listRef} className="flex flex-col">
            {filtered.map((f, i) => {
              const isCursor = i === cursor;
              return (
                <li
                  key={f.id}
                  data-cursor={i}
                  onMouseEnter={() => setCursor(i)}
                  onClick={() => commit(f)}
                  className="flex cursor-pointer items-center gap-2 px-3 py-1.5 text-sm"
                  style={{
                    background: isCursor ? "var(--bg-selected)" : "transparent",
                    color: isCursor ? "var(--accent)" : "var(--fg-base)",
                  }}
                >
                  <span
                    aria-hidden
                    className="w-3 shrink-0 text-center text-[11px]"
                    style={{
                      color: isCursor ? "var(--accent)" : "var(--fg-subtle)",
                    }}
                  >
                    {f.name.toUpperCase() === "INBOX" ? "◆" : "·"}
                  </span>
                  <span className="truncate">{displayFolderName(f.name)}</span>
                  <span
                    className="ml-auto shrink-0 truncate text-[10px]"
                    style={{ color: "var(--fg-subtle)" }}
                    title={f.name}
                  >
                    {decodeImapFolderName(f.name) !== displayFolderName(f.name)
                      ? decodeImapFolderName(f.name)
                      : ""}
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

