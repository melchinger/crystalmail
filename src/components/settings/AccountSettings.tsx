import { useTranslation } from "react-i18next";
import type { AccountSummary } from "../../types";
import { moveAccount } from "../../utils/accountOrder";

type Props = {
  accounts: AccountSummary[];
  onAdd: () => void;
  onEdit: (a: AccountSummary) => void;
  onReorder: (next: AccountSummary[]) => void;
};

/**
 * Account management panel inside the Settings dialog. Shows a read-only
 * summary list (color + display name + address + server), each row
 * clickable to open the edit dialog. The actual add/edit UI is the
 * existing `AddAccountDialog`, triggered via `onAdd` / `onEdit` callbacks
 * so this component stays purely presentational.
 */
export function AccountSettings({
  accounts,
  onAdd,
  onEdit,
  onReorder,
}: Props) {
  const { t } = useTranslation();

  const move = (id: string, direction: "up" | "down") => {
    const next = moveAccount(accounts, id, direction);
    if (next !== accounts) onReorder(next);
  };

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">
          {t("settings.accounts.title")}
        </h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.accounts.hint")}
        </p>
      </header>

      {accounts.length === 0 ? (
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
      ) : (
        <ul
          className="flex flex-col overflow-hidden rounded-md border"
          style={{
            borderColor: "var(--border-base)",
            background: "var(--bg-base)",
          }}
        >
          {accounts.map((a, i) => (
            <li
              key={a.id}
              className={`flex items-center gap-1 ${i === 0 ? "" : "border-t"}`}
              style={{ borderColor: "var(--border-soft)" }}
            >
              {/* Up/Down reorder controls — disabled at list edges.
                  Click events stop propagation so they don't open the
                  edit dialog. Keep them compact to not steal focus
                  from the main click target (the row body). */}
              <div className="flex flex-col gap-0.5 pl-1.5">
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    move(a.id, "up");
                  }}
                  disabled={i === 0}
                  aria-label={t("settings.accounts.moveUp")}
                  title={t("settings.accounts.moveUp")}
                  className="rounded px-1 text-[10px] leading-none disabled:opacity-30"
                  style={{ color: "var(--fg-muted)" }}
                >
                  ▲
                </button>
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    move(a.id, "down");
                  }}
                  disabled={i === accounts.length - 1}
                  aria-label={t("settings.accounts.moveDown")}
                  title={t("settings.accounts.moveDown")}
                  className="rounded px-1 text-[10px] leading-none disabled:opacity-30"
                  style={{ color: "var(--fg-muted)" }}
                >
                  ▼
                </button>
              </div>
              <button
                type="button"
                onClick={() => onEdit(a)}
                className="flex min-w-0 flex-1 items-center gap-3 px-3 py-3 text-left transition-colors"
                style={{ color: "var(--fg-base)" }}
                onMouseEnter={(e) => {
                  e.currentTarget.style.background = "var(--bg-hover)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "transparent";
                }}
              >
                <span
                  className="inline-block h-3 w-3 shrink-0 rounded-full"
                  style={{ background: a.color }}
                  aria-hidden
                />
                <div className="min-w-0 flex-1">
                  <div className="flex items-baseline gap-2">
                    <span className="truncate text-sm font-medium">
                      {a.displayName}
                    </span>
                    <span
                      className="truncate text-xs"
                      style={{ color: "var(--fg-subtle)" }}
                    >
                      {a.address}
                    </span>
                  </div>
                  <div
                    className="mt-0.5 truncate text-[11px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    {a.imapHost}:{a.imapPort} · {a.smtpHost}:{a.smtpPort}
                    {a.aliases.length > 0 && (
                      <>
                        {" · "}
                        {t("settings.accounts.aliasCount", {
                          count: a.aliases.length,
                        })}
                      </>
                    )}
                  </div>
                </div>
                <span
                  aria-hidden
                  className="shrink-0 text-[11px] uppercase tracking-wider"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("settings.accounts.editShort")}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="flex justify-end">
        <button
          type="button"
          onClick={onAdd}
          className="rounded-md px-4 py-1.5 text-sm font-medium"
          style={{ background: "var(--accent)", color: "white" }}
        >
          {t("accounts.addButton")}
        </button>
      </div>
    </div>
  );
}
