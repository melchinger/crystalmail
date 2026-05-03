import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { HotkeyBindings } from "../settings/hotkeys";
import type { AccountSummary } from "../types";
import { HotkeySettings } from "./settings/HotkeySettings";
import { PiSettings } from "./settings/PiSettings";
import { AccountSettings } from "./settings/AccountSettings";
import { SpamRulesSettings } from "./settings/SpamRulesSettings";
import { FolderSyncSettings } from "./settings/FolderSyncSettings";
import { WorkflowSettings } from "./settings/WorkflowSettings";
import { NotificationSettings } from "./settings/NotificationSettings";
import { BackupSettings } from "./settings/BackupSettings";
import { TagsSettings } from "./settings/TagsSettings";
import { TrustedSendersSettings } from "./settings/TrustedSendersSettings";

type CategoryId =
  | "accounts"
  | "folders"
  | "spam"
  | "workflows"
  | "notifications"
  | "hotkeys"
  | "pi"
  | "tags"
  | "trusted"
  | "backup";

type Props = {
  onClose: () => void;
  hotkeys: HotkeyBindings;
  onHotkeysChange: (next: HotkeyBindings) => void;
  accounts: AccountSummary[];
  onAddAccount: () => void;
  onEditAccount: (a: AccountSummary) => void;
  onReorderAccounts: (next: AccountSummary[]) => void;
  /**
   * Optional initial category. Lets external "Jump to settings"
   * affordances (e.g. the AI-required notice in dialogs) deep-link
   * directly to the relevant pane instead of dropping the user on
   * the Accounts list and making them hunt.
   */
  initialCategory?: CategoryId;
};

const CATEGORIES: { id: CategoryId; labelKey: string; icon: string }[] = [
  { id: "accounts", labelKey: "settings.categories.accounts", icon: "✉" },
  { id: "folders", labelKey: "settings.categories.folders", icon: "📁" },
  { id: "spam", labelKey: "settings.categories.spam", icon: "⚠" },
  { id: "workflows", labelKey: "settings.categories.workflows", icon: "⚙" },
  {
    id: "notifications",
    labelKey: "settings.categories.notifications",
    icon: "🔔",
  },
  { id: "hotkeys", labelKey: "settings.categories.hotkeys", icon: "⌨" },
  { id: "pi", labelKey: "settings.categories.pi", icon: "π" },
  { id: "tags", labelKey: "settings.categories.tags", icon: "🏷" },
  { id: "trusted", labelKey: "settings.categories.trusted", icon: "✓" },
  { id: "backup", labelKey: "settings.categories.backup", icon: "⤒" },
];

export function SettingsDialog({
  onClose,
  hotkeys,
  onHotkeysChange,
  accounts,
  onAddAccount,
  onEditAccount,
  onReorderAccounts,
  initialCategory,
}: Props) {
  const { t } = useTranslation();
  // Accounts first by default — most common entry point for new users.
  // `initialCategory` lets external deep-links (e.g. "fix AI now"
  // button) start on a different tab.
  const [active, setActive] = useState<CategoryId>(
    initialCategory ?? "accounts",
  );

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center px-4"
      style={{ background: "rgba(0,0,0,0.45)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        className="flex max-h-[92vh] w-full max-w-4xl overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        {/* Left: category list. Fixed width so the right pane has a stable
            layout regardless of how many categories are added later. */}
        <aside
          className="flex w-52 shrink-0 flex-col border-r"
          style={{
            background: "var(--bg-base)",
            borderColor: "var(--border-soft)",
          }}
        >
          <div
            className="px-4 py-3 text-[11px] uppercase tracking-[0.15em]"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("settings.title")}
          </div>
          <nav className="flex flex-1 flex-col gap-0.5 px-2">
            {CATEGORIES.map((c) => {
              const selected = c.id === active;
              return (
                <button
                  key={c.id}
                  type="button"
                  onClick={() => setActive(c.id)}
                  className="flex items-center gap-2 rounded-md px-3 py-1.5 text-left text-sm transition-colors"
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
                    className="w-4 text-center"
                    style={{ color: "var(--fg-muted)" }}
                  >
                    {c.icon}
                  </span>
                  <span>{t(c.labelKey)}</span>
                </button>
              );
            })}
          </nav>
          <button
            type="button"
            onClick={onClose}
            className="m-2 rounded-md border px-3 py-1.5 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-muted)",
            }}
          >
            {t("settings.close")}
          </button>
        </aside>

        {/* Right: content pane. Each category owns its own layout. */}
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {active === "accounts" && (
            <AccountSettings
              accounts={accounts}
              onAdd={onAddAccount}
              onEdit={onEditAccount}
              onReorder={onReorderAccounts}
            />
          )}
          {active === "folders" && <FolderSyncSettings accounts={accounts} />}
          {active === "spam" && <SpamRulesSettings accounts={accounts} />}
          {active === "workflows" && (
            <WorkflowSettings
              accounts={accounts}
              onOpenAiSettings={() => setActive("pi")}
            />
          )}
          {active === "notifications" && <NotificationSettings />}
          {active === "hotkeys" && (
            <HotkeySettings
              bindings={hotkeys}
              onChange={onHotkeysChange}
            />
          )}
          {active === "pi" && <PiSettings />}
          {active === "tags" && <TagsSettings />}
          {active === "trusted" && <TrustedSendersSettings />}
          {active === "backup" && <BackupSettings />}
        </div>
      </div>
    </div>
  );
}
