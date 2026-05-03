import { useTranslation } from "react-i18next";
import type { AccountSummary } from "../types";

type Props = {
  accounts: AccountSummary[];
  /** `null` = unified across all accounts. */
  selectedAccountId: string | null;
  onSelect: (accountId: string | null) => void;
};

/**
 * Compact account filter above the inbox list. Each account is a small
 * square chip with the account color as background and 1–2 initials inside;
 * the full display name + address is in the tooltip. The "Alle" chip uses
 * a neutral background with the ∀ sigil to stay visually distinct.
 *
 * Hidden entirely when only one account exists — filtering a single-account
 * setup is noise.
 */
export function AccountFilterBar({ accounts, selectedAccountId, onSelect }: Props) {
  const { t } = useTranslation();
  if (accounts.length <= 1) return null;

  return (
    <div
      className="flex flex-wrap items-center gap-1 border-b px-2 py-1.5"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-base)",
      }}
    >
      <InitialChip
        active={selectedAccountId === null}
        onClick={() => onSelect(null)}
        label="∀"
        title={t("inbox.filterAll")}
      />
      {accounts.map((a) => (
        <InitialChip
          key={a.id}
          active={selectedAccountId === a.id}
          onClick={() => onSelect(a.id)}
          label={initials(a.displayName || a.address)}
          title={`${a.displayName} · ${a.address}`}
          color={a.color}
        />
      ))}
    </div>
  );
}

function InitialChip({
  active,
  onClick,
  label,
  title,
  color,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  title: string;
  color?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      className="relative inline-flex h-6 w-6 items-center justify-center rounded-md text-[10px] font-semibold uppercase transition-all"
      style={{
        background: color ?? "transparent",
        color: color ? "#fff" : "var(--fg-muted)",
        border: color
          ? active
            ? "2px solid var(--accent)"
            : "1px solid transparent"
          : active
            ? "2px solid var(--accent)"
            : "1px solid var(--border-base)",
        boxShadow: color && active ? "0 0 0 1px var(--bg-panel) inset" : "none",
        opacity: active ? 1 : 0.72,
      }}
      onMouseEnter={(e) => {
        e.currentTarget.style.opacity = "1";
      }}
      onMouseLeave={(e) => {
        e.currentTarget.style.opacity = active ? "1" : "0.72";
      }}
    >
      {label}
    </button>
  );
}

/// Derive up to two uppercase initials from a display name or email, e.g.
/// "Thomas Melchinger" → "TM", "support@firma.tld" → "SU", "work" → "WO".
function initials(name: string): string {
  const trimmed = name.trim();
  if (trimmed.length === 0) return "?";
  // Words first — "Thomas Melchinger" → TM
  const words = trimmed.split(/\s+/).filter((w) => w.length > 0);
  if (words.length >= 2) {
    return (words[0][0] + words[1][0]).toUpperCase();
  }
  // Email local part
  const emailLocal = trimmed.split("@")[0] ?? trimmed;
  const cleaned = emailLocal.replace(/[^a-zA-Z0-9]/g, "");
  return (cleaned.slice(0, 2) || trimmed.slice(0, 2)).toUpperCase();
}
