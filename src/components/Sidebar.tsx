import { useCallback, useEffect, useState, type FC } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { decodeImapFolderName, displayFolderName } from "../utils/imapFolderName";
import type { AccountSummary, FolderSummary, SyncProgress } from "../types";
import { HoverTip } from "./HoverTip";
import {
  IconArchive,
  IconCalendar,
  IconCompose,
  IconContacts,
  IconDrafts,
  IconInbox,
  IconSent,
  IconSettings,
  IconSpam,
  IconStarred,
  IconSync,
  IconTrash,
} from "./SidebarIcons";

type FolderKey =
  | "unified"
  | "starred"
  | "contacts"
  | "calendar"
  | "archive"
  | "drafts"
  | "sent"
  | "trash"
  | "spam";

/**
 * Identifies the ad-hoc sidebar folder selection. `null` means the main
 * unified nav (inbox/archive/…) is driving the list.
 */
export type FolderSelection = {
  accountId: string;
  folderId: string;
  /** Display name — copied so the list header can render without a lookup. */
  name: string;
};

type Props = {
  active: FolderKey;
  onSelect: (f: FolderKey) => void;
  accounts: AccountSummary[];
  onSyncAll: () => void;
  onCompose: () => void;
  onOpenSettings: () => void;
  syncing: boolean;
  /** Live progress emitted by the current sync run, or `null` when idle. */
  syncProgress: SyncProgress | null;
  /** Current per-account folder pin. null = canonical nav is active. */
  selectedFolder: FolderSelection | null;
  onSelectFolder: (sel: FolderSelection | null) => void;
  /** Unread counts per canonical unified folder, keyed by folder string. */
  unreadCounts: Record<string, number>;
};

/**
 * Canonical folder list. `Icon` is a component (not a glyph string)
 * so it inherits `currentColor` from the button — selected rows get
 * the accent color, unselected rows stay in the muted foreground,
 * all without per-icon styling.
 *
 * `unified` reuses the Inbox icon: the global-inbox shortcut *is*
 * semantically "all inboxes", so giving it a distinct glyph would
 * confuse rather than help.
 */
const FOLDERS: { key: FolderKey; Icon: FC<{ size?: number }> }[] = [
  { key: "unified", Icon: IconInbox },
  { key: "starred", Icon: IconStarred },
  { key: "contacts", Icon: IconContacts },
  { key: "calendar", Icon: IconCalendar },
  { key: "archive", Icon: IconArchive },
  { key: "drafts", Icon: IconDrafts },
  { key: "sent", Icon: IconSent },
  { key: "spam", Icon: IconSpam },
  { key: "trash", Icon: IconTrash },
];

export function Sidebar({
  active,
  onSelect,
  accounts,
  onSyncAll,
  onCompose,
  onOpenSettings,
  syncing,
  syncProgress,
  selectedFolder,
  onSelectFolder,
  unreadCounts,
}: Props) {
  const { t } = useTranslation();
  return (
    <aside
      // Width in `rem` (= 13.75rem ≈ 220px at default zoom) so the column
      // scales with the root font-size. Otherwise raising the zoom via
      // Ctrl+Wheel blows the nav labels past the fixed 220px and icon
      // buttons in the header get clipped.
      className="flex w-[13.75rem] shrink-0 flex-col border-r"
      style={{
        background: "var(--bg-panel)",
        borderColor: "var(--border-base)",
      }}
    >
      <div
        className="flex items-start justify-between gap-2 px-4 pt-5 pb-4 select-none"
        style={{ color: "var(--fg-base)" }}
      >
        <div>
          <div
            className="text-[11px] uppercase tracking-[0.18em]"
            style={{ color: "var(--fg-subtle)" }}
          >
            Mail
          </div>
          <div className="text-lg font-semibold">{t("app.title")}</div>
        </div>
        <div className="flex gap-1">
          {accounts.length > 0 && (
            <>
              <IconButton
                onClick={onCompose}
                title={t("compose.title")}
                icon={<IconCompose size={16} />}
              />
              {/* Custom tooltip because the native `title` attribute
                  caches its text at hover-start — sync progress ticks
                  would never update while the pointer is held still.
                  Fallback `title=` stays set for OS-level assistive
                  readers and for the initial state before any tick. */}
              <HoverTip label={syncTooltip(t, syncing, syncProgress)}>
                <IconButton
                  onClick={onSyncAll}
                  disabled={syncing}
                  title={syncTooltip(t, syncing, syncProgress)}
                  icon={<IconSync size={16} />}
                  spinning={syncing}
                />
              </HoverTip>
            </>
          )}
          <IconButton
            onClick={onOpenSettings}
            title={t("settings.open")}
            icon={<IconSettings size={16} />}
          />
        </div>
      </div>

      <nav className="flex-1 overflow-y-auto px-2 pb-2">
        <ul className="flex flex-col gap-0.5">
          {FOLDERS.map((f) => {
            // Highlight a canonical folder only when no per-account folder
            // override is pinned — otherwise the sub-folder selection owns
            // the "active" styling.
            const selected = f.key === active && !selectedFolder;
            return (
              <li key={f.key}>
                <button
                  onClick={() => {
                    onSelectFolder(null); // clear ad-hoc pin on canonical nav
                    onSelect(f.key);
                  }}
                  className="flex w-full items-center gap-2 rounded-md px-3 py-1.5 text-left text-sm transition-colors"
                  style={{
                    background: selected ? "var(--bg-selected)" : "transparent",
                    color: selected ? "var(--accent)" : "var(--fg-base)",
                  }}
                  onMouseEnter={(e) => {
                    if (!selected)
                      e.currentTarget.style.background = "var(--bg-hover)";
                  }}
                  onMouseLeave={(e) => {
                    if (!selected)
                      e.currentTarget.style.background = "transparent";
                  }}
                >
                  <span
                    className="inline-flex w-4 items-center justify-center"
                    style={{ color: selected ? "var(--accent)" : "var(--fg-muted)" }}
                  >
                    <f.Icon size={16} />
                  </span>
                  <span className="flex-1">{t(`inbox.${f.key}`)}</span>
                  {/* Unread badge. "unified" maps to the inbox count — the
                      global-inbox shortcut at the top inherits the inbox
                      number so users see "5" on both entries. Zero is
                      hidden to avoid noise. */}
                  {(() => {
                    const key = f.key === "unified" ? "inbox" : f.key;
                    const n = unreadCounts[key] ?? 0;
                    if (n === 0) return null;
                    return (
                      <span
                        className="inline-flex h-4 min-w-[1.25rem] items-center justify-center rounded-full px-1.5 text-[10px] font-semibold"
                        style={{
                          background: selected
                            ? "var(--accent)"
                            : "var(--bg-hover)",
                          color: selected ? "white" : "var(--fg-base)",
                        }}
                      >
                        {n > 999 ? "999+" : n}
                      </span>
                    );
                  })()}
                </button>
              </li>
            );
          })}
        </ul>

        {accounts.length > 0 && (
          <>
            <div
              className="mt-6 px-3 text-[11px] uppercase tracking-[0.18em]"
              style={{ color: "var(--fg-subtle)" }}
              title={t("accounts.sidebarHint")}
            >
              {t("accounts.heading")}
            </div>

            <ul className="mt-1 flex flex-col gap-0.5">
              {accounts.map((a) => (
                <AccountExpander
                  key={a.id}
                  account={a}
                  selectedFolder={selectedFolder}
                  onSelectFolder={onSelectFolder}
                />
              ))}
            </ul>
          </>
        )}
      </nav>
    </aside>
  );
}

/**
 * One account row in the sidebar. Clicking the row toggles an expander that
 * lists every IMAP folder under the account (lazy-fetched on first open so
 * accounts that never get expanded cost nothing). Each folder is itself a
 * button that pins the envelope list to that specific mailbox.
 */
function AccountExpander({
  account,
  selectedFolder,
  onSelectFolder,
}: {
  account: AccountSummary;
  selectedFolder: FolderSelection | null;
  onSelectFolder: (sel: FolderSelection | null) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [folders, setFolders] = useState<FolderSummary[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchFolders = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const rows = await invoke<FolderSummary[]>("list_account_folders", {
        accountId: account.id,
      });
      setFolders(rows);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [account.id]);

  // Auto-expand whenever a sub-folder of this account is pinned — e.g. after
  // refresh(), so the user still sees the context of their selection.
  useEffect(() => {
    if (selectedFolder && selectedFolder.accountId === account.id) {
      setExpanded(true);
      if (!folders) void fetchFolders();
    }
  }, [selectedFolder, account.id, folders, fetchFolders]);

  // Sync-Progress-Listener: wenn ein Background-Sync (IDLE-Push, Polling-
  // Tick oder manueller Refresh) für genau dieses Konto neue INBOX-Mails
  // einliefert, ist die per-Folder-Unread-Zahl im Tree veraltet. Re-Fetch
  // genau dann — billiger als ein Permanent-Polling-Loop und matched die
  // gleiche Trigger-Bedingung wie der Auto-Refresh der Mailliste in App.tsx.
  // Greift nur wenn der Tree schon einmal aufgeklappt wurde (folders cached
  // sind) — vor dem ersten Klick gibt's nix zu refreshen.
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    (async () => {
      const fn = await listen<SyncProgress>("sync-progress", (e) => {
        const p = e.payload;
        if (!p.done) return;
        if (p.accountId !== account.id) return;
        if (p.newInInbox === 0) return;
        if (folders === null) return; // nie aufgeklappt → nix zu refreshen
        void fetchFolders();
      });
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [account.id, folders, fetchFolders]);

  const toggle = () => {
    const next = !expanded;
    setExpanded(next);
    // Lazy-fetch on first open. Cached afterwards.
    if (next && !folders && !loading) void fetchFolders();
  };

  return (
    <li>
      <button
        type="button"
        onClick={toggle}
        title={account.address}
        className="flex w-full items-center gap-2 rounded-md px-3 py-1.5 text-left text-sm transition-colors"
        style={{ color: "var(--fg-base)" }}
        onMouseEnter={(e) => {
          e.currentTarget.style.background = "var(--bg-hover)";
        }}
        onMouseLeave={(e) => {
          e.currentTarget.style.background = "transparent";
        }}
      >
        <span
          aria-hidden
          className="inline-block w-3 shrink-0 text-center text-[10px]"
          style={{
            color: "var(--fg-subtle)",
            transform: expanded ? "rotate(90deg)" : "rotate(0deg)",
            transition: "transform 120ms ease",
          }}
        >
          ▶
        </span>
        <span
          className="inline-block h-2 w-2 shrink-0 rounded-full"
          style={{ background: account.color }}
          aria-hidden
        />
        <span className="truncate">{account.displayName}</span>
      </button>

      {expanded && (
        <ul className="mt-0.5 ml-4 flex flex-col gap-0.5 border-l pl-2"
            style={{ borderColor: "var(--border-soft)" }}>
          {loading && (
            <li
              className="px-2 py-1 text-[11px]"
              style={{ color: "var(--fg-subtle)" }}
            >
              …
            </li>
          )}
          {error && (
            <li
              className="px-2 py-1 text-[11px]"
              style={{ color: "#ef4444" }}
              title={error}
            >
              ⚠
            </li>
          )}
          {folders?.length === 0 && !loading && (
            <li
              className="px-2 py-1 text-[11px]"
              style={{ color: "var(--fg-subtle)" }}
            >
              –
            </li>
          )}
          {folders?.map((f) => {
            const selected =
              selectedFolder?.folderId === f.id &&
              selectedFolder?.accountId === account.id;
            const isInbox = f.name.toUpperCase() === "INBOX";
            // Server form (`Entw&APw-rfe`) is kept in the selection so
            // downstream IMAP operations see the exact folder path the
            // server expects. The decoded version is display-only.
            const decoded = decodeImapFolderName(f.name);
            return (
              <li key={f.id}>
                <button
                  type="button"
                  onClick={() =>
                    onSelectFolder({
                      accountId: account.id,
                      folderId: f.id,
                      name: decoded,
                    })
                  }
                  title={f.name}
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-[12px] transition-colors"
                  style={{
                    background: selected ? "var(--bg-selected)" : "transparent",
                    color: selected
                      ? "var(--accent)"
                      : f.unread > 0
                        ? "var(--fg-base)"
                        : "var(--fg-muted)",
                    fontWeight: f.unread > 0 ? 600 : 400,
                  }}
                  onMouseEnter={(e) => {
                    if (!selected)
                      e.currentTarget.style.background = "var(--bg-hover)";
                  }}
                  onMouseLeave={(e) => {
                    if (!selected)
                      e.currentTarget.style.background = "transparent";
                  }}
                >
                  <span
                    aria-hidden
                    className="w-3 shrink-0 text-center text-[10px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {isInbox ? "◆" : "·"}
                  </span>
                  <span className="truncate">{displayFolderName(f.name)}</span>
                  {f.unread > 0 && (
                    <span
                      className="ml-auto shrink-0 rounded-full px-1.5 text-[10px] leading-tight"
                      style={{
                        background: "var(--bg-selected)",
                        color: "var(--accent)",
                      }}
                    >
                      {f.unread}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </li>
  );
}

/**
 * Build the tooltip for the sync icon.
 *   - Not syncing → "Synchronisieren" (same as before)
 *   - Syncing, no folder in flight yet → "Synchronisiert …"
 *   - Syncing with folder info → "Privat · INBOX: 125 / 150"
 *   - Syncing with unknown total (total=0, e.g. empty folder) → hide the total
 *   - Done tick → "Privat · abgeschlossen"
 */
function syncTooltip(
  t: (k: string, opts?: Record<string, unknown>) => string,
  syncing: boolean,
  progress: SyncProgress | null,
): string {
  if (!syncing && !progress) return t("sync.button");
  if (progress) {
    if (progress.done) {
      return t("sync.tooltipDone", { account: progress.accountName });
    }
    const folder = progress.folder || "…";
    if (progress.total > 0) {
      return t("sync.tooltipActive", {
        account: progress.accountName,
        folder,
        fetched: progress.fetched,
        total: progress.total,
      });
    }
    return t("sync.tooltipActiveNoTotal", {
      account: progress.accountName,
      folder,
    });
  }
  return t("sync.running");
}

function IconButton({
  onClick,
  title,
  icon,
  disabled,
  spinning,
}: {
  onClick: () => void;
  title: string;
  /** Rendered inside the button. Inherits color from the button's CSS. */
  icon: React.ReactNode;
  disabled?: boolean;
  spinning?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      className="inline-flex items-center justify-center rounded-md p-1.5 disabled:opacity-50"
      style={{
        color: "var(--fg-muted)",
        border: "1px solid var(--border-soft)",
        background: "transparent",
      }}
      onMouseEnter={(e) => {
        if (!disabled) e.currentTarget.style.background = "var(--bg-hover)";
      }}
      onMouseLeave={(e) => {
        e.currentTarget.style.background = "transparent";
      }}
    >
      <span
        className="inline-flex"
        style={{ animation: spinning ? "cm-spin 1.4s linear infinite" : "none" }}
      >
        {icon}
      </span>
    </button>
  );
}
