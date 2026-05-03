import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  open as openDialog,
  save as saveDialog,
} from "@tauri-apps/plugin-dialog";
import type { ContactSummary } from "../types";

type Props = {
  selectedId: string | undefined;
  onSelect: (id: string | undefined) => void;
  /** Bumpt der Parent z. B. nach delete/create — wir reagieren auf den
   *  geänderten Wert mit einem Refresh ohne dass wir interne Mutation-
   *  State führen müssen. */
  refreshKey?: number;
  onCreateNew: () => void;
};

type ExportResult = { path: string; count: number };
type ImportReport = {
  created: number;
  skippedExistingEmail: number;
  tagsCreated: number;
  skippedInvalid: number;
  skippedAddresses: string[];
};

const SEARCH_DEBOUNCE_MS = 200;

export function ContactsView({
  selectedId,
  onSelect,
  refreshKey,
  onCreateNew,
}: Props) {
  const { t } = useTranslation();
  const [items, setItems] = useState<ContactSummary[]>([]);
  const [searchInput, setSearchInput] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  // Toast-Style-Feedback nach Export/Import-Aktion. `null` wenn ruhig.
  const [actionStatus, setActionStatus] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  // Click-outside / Esc schließt das Menü.
  useEffect(() => {
    if (!menuOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (!menuRef.current) return;
      if (!menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  // 200ms Debounce auf der Suche — sonst feuert jeder Tastenanschlag
  // einen DB-Roundtrip.
  useEffect(() => {
    const handle = window.setTimeout(
      () => setSearchQuery(searchInput.trim()),
      SEARCH_DEBOUNCE_MS,
    );
    return () => window.clearTimeout(handle);
  }, [searchInput]);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const rows = await invoke<ContactSummary[]>("list_contacts", {
        query: searchQuery.length > 0 ? searchQuery : null,
        limit: 200,
        offset: 0,
      });
      setItems(rows);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [searchQuery]);

  const onExport = async (kind: "vcf" | "csv") => {
    setMenuOpen(false);
    setActionStatus(null);
    try {
      const stamp = new Date()
        .toISOString()
        .replace(/[:.]/g, "-")
        .slice(0, 19);
      const ext = kind;
      const dest = await saveDialog({
        defaultPath: `crystalmail-contacts-${stamp}.${ext}`,
        title: t("contacts.exportDialogTitle"),
        filters: [
          {
            name:
              kind === "vcf"
                ? t("contacts.vcfFiles")
                : t("contacts.csvFiles"),
            extensions: [ext],
          },
        ],
      });
      if (!dest) return;
      const cmd =
        kind === "vcf" ? "export_contacts_vcf" : "export_contacts_csv";
      const result = await invoke<ExportResult>(cmd, { path: dest });
      setActionStatus(
        t("contacts.exportDone", { count: result.count, path: result.path }),
      );
    } catch (e) {
      setActionStatus(t("contacts.actionFailed", { detail: String(e) }));
    }
  };

  const onImport = async (kind: "vcf" | "csv") => {
    setMenuOpen(false);
    setActionStatus(null);
    try {
      const picked = await openDialog({
        multiple: false,
        title: t("contacts.importDialogTitle"),
        filters: [
          {
            name:
              kind === "vcf"
                ? t("contacts.vcfFiles")
                : t("contacts.csvFiles"),
            extensions: [kind],
          },
        ],
      });
      if (!picked || Array.isArray(picked)) return;
      const cmd =
        kind === "vcf" ? "import_contacts_vcf" : "import_contacts_csv";
      const report = await invoke<ImportReport>(cmd, { path: picked });
      const summary = [
        t("contacts.importCreated", { count: report.created }),
        report.skippedExistingEmail > 0
          ? t("contacts.importSkippedExisting", {
              count: report.skippedExistingEmail,
            })
          : null,
        report.skippedInvalid > 0
          ? t("contacts.importSkippedInvalid", { count: report.skippedInvalid })
          : null,
        report.tagsCreated > 0
          ? t("contacts.importTagsCreated", { count: report.tagsCreated })
          : null,
      ]
        .filter(Boolean)
        .join(" · ");
      setActionStatus(summary);
      await load();
    } catch (e) {
      setActionStatus(t("contacts.actionFailed", { detail: String(e) }));
    }
  };

  useEffect(() => {
    void load();
  }, [load, refreshKey]);

  return (
    <div
      className="flex h-full flex-col"
      style={{ background: "var(--bg-panel)" }}
    >
      <div
        className="flex items-center gap-2 border-b px-3 py-2"
        style={{ borderColor: "var(--border-base)" }}
      >
        <input
          value={searchInput}
          onChange={(e) => setSearchInput(e.target.value)}
          placeholder={t("contacts.searchPlaceholder")}
          className="flex-1 rounded-md px-2 py-1 text-sm outline-none"
          style={{
            background: "var(--bg-base)",
            color: "var(--fg-base)",
            border: "1px solid var(--border-base)",
          }}
        />
        <button
          type="button"
          onClick={onCreateNew}
          className="rounded-md border px-2 py-1 text-xs"
          style={{
            borderColor: "var(--border-base)",
            color: "var(--fg-base)",
          }}
          title={t("contacts.createNew")}
        >
          + {t("contacts.createNewShort")}
        </button>
        {/* Burger-Menü mit Import/Export. Click-outside via globalem
            mousedown-Listener oben — ref auf den Container damit der
            Listener nicht die Buttons im Menu selbst killt. */}
        <div className="relative" ref={menuRef}>
          <button
            type="button"
            onClick={() => setMenuOpen((v) => !v)}
            className="rounded-md border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-muted)",
            }}
            title={t("contacts.moreActions")}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
          >
            ⋯
          </button>
          {menuOpen && (
            <div
              role="menu"
              className="absolute right-0 top-full z-40 mt-1 w-56 overflow-hidden rounded-md border shadow-lg"
              style={{
                background: "var(--bg-panel)",
                borderColor: "var(--border-base)",
              }}
            >
              <MenuItem
                label={t("contacts.exportVcf")}
                onClick={() => void onExport("vcf")}
              />
              <MenuItem
                label={t("contacts.exportCsv")}
                onClick={() => void onExport("csv")}
              />
              <div
                className="border-t"
                style={{ borderColor: "var(--border-soft)" }}
              />
              <MenuItem
                label={t("contacts.importVcf")}
                onClick={() => void onImport("vcf")}
              />
              <MenuItem
                label={t("contacts.importCsv")}
                onClick={() => void onImport("csv")}
              />
            </div>
          )}
        </div>
      </div>

      {actionStatus && (
        <div
          className="flex items-start justify-between gap-2 border-b px-3 py-2 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-muted)",
          }}
        >
          <span className="min-w-0 flex-1 break-all">{actionStatus}</span>
          <button
            type="button"
            onClick={() => setActionStatus(null)}
            style={{ color: "var(--fg-subtle)" }}
            aria-label="dismiss"
          >
            ✕
          </button>
        </div>
      )}

      {error && (
        <div
          className="px-3 py-2 text-xs"
          style={{
            background: "rgba(248,113,113,0.12)",
            color: "#ef4444",
          }}
        >
          {error}
        </div>
      )}

      {items.length === 0 ? (
        <div
          className="flex flex-1 items-center justify-center px-6 text-sm"
          style={{ color: "var(--fg-subtle)" }}
        >
          {loading
            ? t("common.loading")
            : searchQuery.length > 0
              ? t("contacts.searchEmpty")
              : t("contacts.empty")}
        </div>
      ) : (
        <ul className="flex-1 overflow-y-auto">
          {items.map((c) => {
            const selected = c.id === selectedId;
            return (
              <li
                key={c.id}
                onClick={() => onSelect(c.id)}
                className="cursor-pointer border-b px-3 py-2 transition-colors"
                style={{
                  borderColor: "var(--border-soft)",
                  background: selected ? "var(--bg-selected)" : "transparent",
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
                <div className="flex items-baseline justify-between gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      {c.pinned && (
                        <span
                          aria-hidden
                          title={t("contacts.pinned")}
                          style={{ color: "var(--accent)" }}
                        >
                          ★
                        </span>
                      )}
                      <span
                        className="truncate text-sm font-medium"
                        style={{ color: "var(--fg-base)" }}
                      >
                        {c.displayName}
                      </span>
                    </div>
                    <div
                      className="truncate text-[11px]"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      {[c.organization, c.city].filter(Boolean).join(" · ")}
                      {c.primaryEmail ? ` · ${c.primaryEmail}` : ""}
                    </div>
                  </div>
                  {c.messageCount > 0 && (
                    <span
                      className="shrink-0 text-[10px]"
                      style={{ color: "var(--fg-muted)" }}
                      title={t("contacts.messageCount", {
                        count: c.messageCount,
                      })}
                    >
                      {c.messageCount}
                    </span>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

function MenuItem({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      className="flex w-full items-center px-3 py-1.5 text-left text-sm transition-colors"
      style={{ color: "var(--fg-base)", background: "transparent" }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--bg-hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {label}
    </button>
  );
}
