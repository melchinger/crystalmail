// Phase-2 calendar IMAP sync settings.
//
// Three knobs:
//   * `enabled`     — opt-in. Disabled keeps the calendar in Phase-1
//                     local-only mode (current default).
//   * `accountId`   — which mail account hosts the IMAP folder. Single
//                     account for v1; multi-calendar split is post-MVP.
//   * `folderPath`  — raw IMAP path. Default uses `/`-delimiter
//                     convention; Cyrus-style `.`-delimiter servers
//                     need an override (e.g. `INBOX.TimeProtocol.Calendar`).

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { AccountSummary, CalendarConfig } from "../../types";

type Props = {
  accounts: AccountSummary[];
};

export function CalendarSettings({ accounts }: Props) {
  const { t } = useTranslation();
  const [config, setConfig] = useState<CalendarConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const c = await invoke<CalendarConfig>("cal_get_config");
        if (!cancelled) setConfig(c);
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  if (!config) {
    return (
      <p className="text-sm" style={{ color: "var(--fg-muted)" }}>
        {error ?? t("settings.calendar.loading")}
      </p>
    );
  }

  const update = (patch: Partial<CalendarConfig>) =>
    setConfig({ ...config, ...patch });

  const save = async () => {
    if (saving) return;
    setSaving(true);
    setError(null);
    try {
      await invoke<void>("cal_set_config", { config });
      setSavedAt(Date.now());
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="flex flex-col gap-4">
      <header>
        <h2 className="text-base font-semibold" style={{ color: "var(--fg-base)" }}>
          {t("settings.calendar.title")}
        </h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.calendar.description")}
        </p>
      </header>

      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={config.enabled}
          onChange={(e) => update({ enabled: e.target.checked })}
        />
        <span>{t("settings.calendar.enabled")}</span>
      </label>

      <label className="flex flex-col gap-1 text-sm">
        <span style={{ color: "var(--fg-muted)" }}>
          {t("settings.calendar.account")}
        </span>
        <select
          value={config.accountId ?? ""}
          onChange={(e) =>
            update({ accountId: e.target.value || null })
          }
          className="rounded border px-2 py-1"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-base)",
          }}
          disabled={!config.enabled}
        >
          <option value="">{t("settings.calendar.accountNone")}</option>
          {accounts.map((a) => (
            <option key={a.id} value={a.id}>
              {a.address}
            </option>
          ))}
        </select>
      </label>

      <label className="flex flex-col gap-1 text-sm">
        <span style={{ color: "var(--fg-muted)" }}>
          {t("settings.calendar.folder")}
        </span>
        <input
          type="text"
          value={config.folderPath}
          onChange={(e) => update({ folderPath: e.target.value })}
          className="rounded border px-2 py-1 font-mono text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-base)",
          }}
          disabled={!config.enabled}
        />
        <span className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.calendar.folderHint")}
        </span>
      </label>

      <fieldset
        className="flex flex-col gap-2 rounded border px-3 py-2"
        style={{ borderColor: "var(--border-soft)" }}
        disabled={!config.enabled}
      >
        <legend
          className="px-1 text-xs"
          style={{ color: "var(--fg-subtle)" }}
        >
          {t("settings.calendar.advanced")}
        </legend>

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={config.idleEnabled}
            onChange={(e) => update({ idleEnabled: e.target.checked })}
          />
          <span>{t("settings.calendar.idleEnabled")}</span>
          <span
            className="ml-auto text-xs"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("settings.calendar.idleEnabledHint")}
          </span>
        </label>

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={config.syncOnMutation}
            onChange={(e) =>
              update({ syncOnMutation: e.target.checked })
            }
          />
          <span>{t("settings.calendar.syncOnMutation")}</span>
        </label>

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={config.compactionEnabled}
            onChange={(e) =>
              update({ compactionEnabled: e.target.checked })
            }
          />
          <span>{t("settings.calendar.compactionEnabled")}</span>
        </label>

        <label className="flex items-center gap-2 text-sm">
          <span>{t("settings.calendar.autoSyncInterval")}</span>
          <input
            type="number"
            min={0}
            step={60}
            value={config.autoSyncIntervalSeconds}
            onChange={(e) =>
              update({
                autoSyncIntervalSeconds: Number(e.target.value) || 0,
              })
            }
            className="w-20 rounded border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
              color: "var(--fg-base)",
            }}
          />
          <span
            className="text-xs"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("settings.calendar.autoSyncIntervalHint")}
          </span>
        </label>
      </fieldset>

      {error && (
        <div
          className="rounded border px-2 py-1 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            color: "var(--fg-error, #c00)",
          }}
        >
          {error}
        </div>
      )}

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={() => void save()}
          disabled={saving}
          className="rounded px-3 py-1 text-xs font-medium"
          style={{
            background: "var(--accent)",
            color: "#fff",
            border: "1px solid var(--border-soft)",
          }}
        >
          {saving ? t("settings.calendar.saving") : t("settings.calendar.save")}
        </button>
        {savedAt && (
          <span className="text-xs" style={{ color: "var(--fg-muted)" }}>
            {t("settings.calendar.saved")}
          </span>
        )}
      </div>
    </section>
  );
}
