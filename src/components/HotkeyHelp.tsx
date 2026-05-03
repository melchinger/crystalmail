import { useTranslation } from "react-i18next";
import {
  formatBinding,
  HOTKEY_ACTIONS,
  HOTKEY_ACTION_IDS,
  type HotkeyBindings,
} from "../settings/hotkeys";

type Props = {
  onClose: () => void;
  bindings: HotkeyBindings;
};

/**
 * Read-only cheat-sheet rendered from the current hotkey registry. Stays
 * in sync with user customizations automatically — no hardcoded list.
 */
export function HotkeyHelp({ onClose, bindings }: Props) {
  const { t } = useTranslation();

  const messageIds = HOTKEY_ACTION_IDS.filter(
    (id) => HOTKEY_ACTIONS[id].group === "message",
  );
  const appIds = HOTKEY_ACTION_IDS.filter(
    (id) => HOTKEY_ACTIONS[id].group === "app",
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
        className="w-full max-w-lg rounded-xl border p-5 shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-base font-semibold">{t("hotkeys.title")}</h2>
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-2 py-1 text-sm"
            style={{ color: "var(--fg-muted)" }}
            aria-label={t("hotkeys.close")}
          >
            ✕
          </button>
        </div>

        <div className="flex flex-col gap-4">
          <Section
            title={t("hotkeys.groups.message")}
            ids={messageIds}
            bindings={bindings}
          />
          <Section
            title={t("hotkeys.groups.app")}
            ids={appIds}
            bindings={bindings}
          />
          <Section
            title={t("hotkeys.groups.system")}
            ids={[]}
            bindings={bindings}
            extraRows={[
              ["Strg+0 / +/−", t("hotkeys.zoom")],
              ["Esc", t("hotkeys.escape")],
            ]}
          />
        </div>

        <p
          className="mt-4 text-[11px]"
          style={{ color: "var(--fg-subtle)" }}
        >
          {t("hotkeys.footnote")}
        </p>
      </div>
    </div>
  );
}

function Section({
  title,
  ids,
  bindings,
  extraRows,
}: {
  title: string;
  ids: readonly (keyof typeof HOTKEY_ACTIONS)[];
  bindings: HotkeyBindings;
  extraRows?: [string, string][];
}) {
  const { t } = useTranslation();
  if (ids.length === 0 && !extraRows?.length) return null;
  return (
    <section>
      <h3
        className="mb-1.5 text-[11px] uppercase tracking-[0.15em]"
        style={{ color: "var(--fg-subtle)" }}
      >
        {title}
      </h3>
      <ul className="flex flex-col gap-1">
        {ids.map((id) => {
          const combo = bindings[id].map(formatBinding).join(" / ") || "—";
          return (
            <li
              key={id}
              className="grid grid-cols-[9rem_1fr] items-center gap-3 text-sm"
            >
              <KeyBox>{combo}</KeyBox>
              <span style={{ color: "var(--fg-muted)" }}>
                {t(HOTKEY_ACTIONS[id].labelKey)}
              </span>
            </li>
          );
        })}
        {extraRows?.map(([key, label]) => (
          <li
            key={key}
            className="grid grid-cols-[9rem_1fr] items-center gap-3 text-sm"
          >
            <KeyBox>{key}</KeyBox>
            <span style={{ color: "var(--fg-muted)" }}>{label}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function KeyBox({ children }: { children: React.ReactNode }) {
  return (
    <kbd
      className="inline-block rounded border px-2 py-0.5 text-center font-mono text-[11px]"
      style={{
        borderColor: "var(--border-base)",
        background: "var(--bg-base)",
        color: "var(--fg-base)",
      }}
    >
      {children}
    </kbd>
  );
}
