import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { AccountSummary, FolderSummary } from "../../types";
import { decodeImapFolderName } from "../../utils/imapFolderName";

/** Global broadcast so the sidebar / inbox list can refetch their
 *  folder-driven state when a folder was created or deleted from
 *  here. The Settings dialog is modal, so anyone observing folder
 *  state outside it would otherwise stay stale until a restart. */
function announceFolderChange() {
  window.dispatchEvent(new CustomEvent("cm:folders:changed"));
}

type Props = {
  accounts: AccountSummary[];
};

type FoldersByAccount = Record<string, FolderSummary[]>;

/**
 * Per-account folder sync toggles. Every folder the server exposes
 * for an account can be opted out of both the eager special-folder
 * sync and the lazy on-open sync. Default is always on — users only
 * see this panel to turn things *off* (e.g. a giant Archive they
 * don't want hitting their laptop every five minutes).
 *
 * The pi-discovered "specials" (INBOX, Archive, Sent, Drafts, Trash,
 * Spam) are marked with a badge so the user understands turning one
 * off silences the main sync for that mailbox on that account.
 */
export function FolderSyncSettings({ accounts }: Props) {
  const { t } = useTranslation();
  const [byAccount, setByAccount] = useState<FoldersByAccount>({});
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const next: FoldersByAccount = {};
      for (const a of accounts) {
        const rows = await invoke<FolderSummary[]>("list_account_folders", {
          accountId: a.id,
        });
        next[a.id] = rows;
      }
      setByAccount(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [accounts]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const onCreate = useCallback(
    async (accountId: string, name: string) => {
      await invoke("create_folder", { accountId, name });
      announceFolderChange();
      await refresh();
    },
    [refresh],
  );

  const onDelete = useCallback(
    async (accountId: string, name: string) => {
      await invoke("delete_folder", { accountId, name });
      announceFolderChange();
      await refresh();
    },
    [refresh],
  );

  const onToggle = useCallback(
    async (accountId: string, folderId: string, next: boolean) => {
      // Optimistic flip so the checkbox feels instant. Revert on error.
      setByAccount((prev) => {
        const list = prev[accountId];
        if (!list) return prev;
        return {
          ...prev,
          [accountId]: list.map((f) =>
            f.id === folderId ? { ...f, syncEnabled: next } : f,
          ),
        };
      });
      try {
        await invoke("set_folder_sync_enabled", {
          folderId,
          enabled: next,
        });
      } catch (e) {
        setError(String(e));
        // Revert the optimistic flip.
        setByAccount((prev) => {
          const list = prev[accountId];
          if (!list) return prev;
          return {
            ...prev,
            [accountId]: list.map((f) =>
              f.id === folderId ? { ...f, syncEnabled: !next } : f,
            ),
          };
        });
      }
    },
    [],
  );

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">
          {t("settings.folders.title")}
        </h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.folders.hint")}
        </p>
      </header>

      {error && (
        <div
          className="rounded-md border px-3 py-2 text-xs"
          style={{
            borderColor: "#ef4444",
            background: "rgba(239,68,68,0.08)",
            color: "#ef4444",
          }}
        >
          {error}
        </div>
      )}

      {loading && accounts.length > 0 && (
        <div className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("common.loading")}
        </div>
      )}

      {!loading && accounts.length === 0 && (
        <div
          className="rounded-md border px-4 py-6 text-center text-sm"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-muted)",
          }}
        >
          {t("common.noAccounts")}
        </div>
      )}

      {!loading &&
        accounts.map((account) => (
          <AccountFolderList
            key={account.id}
            account={account}
            folders={byAccount[account.id] ?? []}
            onToggle={onToggle}
            onCreate={onCreate}
            onDelete={onDelete}
          />
        ))}
    </div>
  );
}

type AccountFolderListProps = {
  account: AccountSummary;
  folders: FolderSummary[];
  onToggle: (accountId: string, folderId: string, next: boolean) => void;
  onCreate: (accountId: string, name: string) => Promise<void>;
  onDelete: (accountId: string, name: string) => Promise<void>;
};

function AccountFolderList({
  account,
  folders,
  onToggle,
  onCreate,
  onDelete,
}: AccountFolderListProps) {
  const { t } = useTranslation();
  const [adding, setAdding] = useState(false);
  const [newName, setNewName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const commitAdd = async () => {
    const name = newName.trim();
    if (!name || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onCreate(account.id, name);
      setAdding(false);
      setNewName("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const commitDelete = async (name: string) => {
    // Two-step confirm — IMAP delete is quick but permanent for the
    // folder's content. Decoded display name for the prompt so users
    // recognise what they're nuking.
    const label = decodeImapFolderName(name);
    if (!confirm(t("settings.folders.confirmDelete", { name: label }))) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onDelete(account.id, name);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // Specials get a badge. Comparison is on the raw IMAP name since
  // that's what both sides store. INBOX is always a special; the
  // other five come from the account config.
  const specialNames = useMemo(
    () =>
      new Set<string>(
        [
          "INBOX",
          account.archiveFolder,
          account.sentFolder,
          account.draftsFolder,
          account.trashFolder,
          account.spamFolder,
        ].filter(Boolean),
      ),
    [account],
  );

  const sorted = useMemo(
    () =>
      [...folders].sort((a, b) =>
        decodeImapFolderName(a.name).localeCompare(
          decodeImapFolderName(b.name),
          undefined,
          { sensitivity: "base" },
        ),
      ),
    [folders],
  );

  return (
    <section className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <span
          aria-hidden
          className="inline-block h-2.5 w-2.5 rounded-full"
          style={{ background: account.color }}
        />
        <h3 className="text-sm font-semibold">{account.displayName}</h3>
        <span className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {account.address}
        </span>
        <button
          type="button"
          onClick={() => {
            setAdding((v) => !v);
            setError(null);
          }}
          disabled={busy}
          className="ml-auto rounded-md border px-2 py-0.5 text-[11px]"
          style={{
            borderColor: "var(--border-base)",
            color: "var(--accent)",
          }}
        >
          {adding
            ? t("settings.folders.cancelAdd")
            : `+ ${t("settings.folders.addFolder")}`}
        </button>
      </div>

      {adding && (
        <div
          className="flex items-center gap-2 rounded-md border p-2"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
          }}
        >
          <input
            autoFocus
            type="text"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void commitAdd();
              } else if (e.key === "Escape") {
                e.preventDefault();
                setAdding(false);
                setNewName("");
              }
            }}
            placeholder={t("settings.folders.newFolderPlaceholder")}
            className="flex-1 rounded-md border px-2 py-1 text-sm"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
            }}
          />
          <button
            type="button"
            onClick={() => void commitAdd()}
            disabled={busy || !newName.trim()}
            className="rounded-md border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              background: "var(--accent)",
              color: "var(--bg-panel)",
              opacity: busy || !newName.trim() ? 0.6 : 1,
            }}
          >
            {busy
              ? t("settings.folders.creating")
              : t("settings.folders.create")}
          </button>
        </div>
      )}

      {error && (
        <div
          className="rounded-md border px-3 py-2 text-xs"
          style={{
            borderColor: "#ef4444",
            background: "rgba(239,68,68,0.08)",
            color: "#ef4444",
          }}
        >
          {error}
        </div>
      )}

      {folders.length === 0 ? (
        <div
          className="rounded-md border px-3 py-3 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-muted)",
          }}
        >
          {t("settings.folders.empty")}
        </div>
      ) : (
        <ul
          className="flex flex-col overflow-hidden rounded-md border"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-base)",
          }}
        >
          {sorted.map((f, i) => {
            const isSpecial = specialNames.has(f.name);
            const decoded = decodeImapFolderName(f.name);
            return (
              <li
                key={f.id}
                className={`flex items-center gap-3 px-3 py-2 text-sm ${
                  i === 0 ? "" : "border-t"
                }`}
                style={{ borderColor: "var(--border-soft)" }}
              >
                <input
                  id={`sync-${f.id}`}
                  type="checkbox"
                  checked={f.syncEnabled}
                  onChange={(e) =>
                    onToggle(account.id, f.id, e.target.checked)
                  }
                  className="h-4 w-4 cursor-pointer"
                />
                <label
                  htmlFor={`sync-${f.id}`}
                  className="flex min-w-0 flex-1 cursor-pointer items-center gap-2"
                  title={f.name}
                >
                  <span
                    className="truncate"
                    style={{
                      color: f.syncEnabled
                        ? "var(--fg-base)"
                        : "var(--fg-muted)",
                      textDecoration: f.syncEnabled ? "none" : "line-through",
                    }}
                  >
                    {decoded}
                  </span>
                  {isSpecial && (
                    <span
                      className="rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wide"
                      style={{
                        background: "var(--bg-hover)",
                        color: "var(--fg-subtle)",
                      }}
                    >
                      {t("settings.folders.specialBadge")}
                    </span>
                  )}
                </label>
                <span
                  className="shrink-0 text-[11px] tabular-nums"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {f.unread > 0
                    ? t("settings.folders.counts", {
                        unread: f.unread,
                        total: f.total,
                      })
                    : t("settings.folders.total", { total: f.total })}
                </span>
                {/* Specials (INBOX + account-configured folders) stay
                    un-deletable at the UI level — backend refuses
                    them too, but hiding the button avoids a failing
                    click entirely. */}
                {!isSpecial && (
                  <button
                    type="button"
                    onClick={() => void commitDelete(f.name)}
                    disabled={busy}
                    title={t("settings.folders.deleteTooltip")}
                    className="rounded px-1.5 py-0.5 text-xs"
                    style={{ color: "#ef4444", opacity: busy ? 0.5 : 1 }}
                  >
                    ✕
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
