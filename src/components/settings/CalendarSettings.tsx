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

import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type {
  AccountSummary,
  CalendarConfig,
  CalendarSubscription,
  RefreshReport,
  SubscriptionSource,
} from "../../types";
import { SUBSCRIPTION_PALETTE } from "../../utils/calendarEvent";

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

      <SubscriptionsPanel />
    </section>
  );
}

// ─── Subscriptions sub-panel ─────────────────────────────────────────────

function SubscriptionsPanel() {
  const { t } = useTranslation();
  const [subs, setSubs] = useState<CalendarSubscription[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [showAdd, setShowAdd] = useState(false);

  const reload = useCallback(async () => {
    try {
      const list = await invoke<CalendarSubscription[]>("cal_subs_list");
      setSubs(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const handleRefresh = async (id: string) => {
    setBusyId(id);
    try {
      await invoke<RefreshReport>("cal_subs_refresh", { id });
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  };

  const handleRemove = async (id: string, name: string) => {
    if (!window.confirm(t("settings.calendar.subs.confirmRemove", { name }))) return;
    setBusyId(id);
    try {
      await invoke<void>("cal_subs_remove", { id });
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  };

  const handleToggle = async (id: string, enabled: boolean) => {
    try {
      await invoke<CalendarSubscription>("cal_subs_set_enabled", {
        id,
        enabled,
      });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSetInterval = async (id: string, minutes: number) => {
    try {
      await invoke<CalendarSubscription>("cal_subs_set_interval", {
        id,
        minutes,
      });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSetColor = async (id: string, color: string) => {
    try {
      await invoke<CalendarSubscription>("cal_subs_set_color", { id, color });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <fieldset
      className="flex flex-col gap-3 rounded border px-3 py-3"
      style={{ borderColor: "var(--border-soft)" }}
    >
      <legend className="px-1 text-xs" style={{ color: "var(--fg-subtle)" }}>
        {t("settings.calendar.subs.title")}
      </legend>

      <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
        {t("settings.calendar.subs.description")}
      </p>

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

      {subs === null ? (
        <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.calendar.subs.loading")}
        </p>
      ) : subs.length === 0 ? (
        <p className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("settings.calendar.subs.empty")}
        </p>
      ) : (
        <ul className="flex flex-col gap-2">
          {subs.map((sub) => (
            <li
              key={sub.id}
              className="flex flex-col gap-1 rounded border px-2 py-2 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
              }}
            >
              <div className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={sub.enabled}
                  onChange={(e) =>
                    void handleToggle(sub.id, e.target.checked)
                  }
                  title={t("settings.calendar.subs.enabledTitle")}
                />
                <ColorSwatch
                  color={sub.color}
                  onPick={(c) => void handleSetColor(sub.id, c)}
                />
                <span className="font-medium" style={{ color: "var(--fg-base)" }}>
                  {sub.name}
                </span>
                <span
                  className="ml-auto text-xs"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {sub.lastEventCount !== null
                    ? t("settings.calendar.subs.events", {
                        count: sub.lastEventCount,
                      })
                    : ""}
                </span>
              </div>

              <div
                className="truncate text-xs"
                style={{ color: "var(--fg-muted)" }}
                title={
                  sub.source.kind === "file" ? sub.source.path : sub.source.url
                }
              >
                {sub.source.kind === "file" ? "📄 " : "🌐 "}
                {sub.source.kind === "file" ? sub.source.path : sub.source.url}
              </div>

              <div className="flex items-center gap-2 text-xs">
                <label className="flex items-center gap-1">
                  <span style={{ color: "var(--fg-muted)" }}>
                    {t("settings.calendar.subs.interval")}
                  </span>
                  <input
                    type="number"
                    min={0}
                    step={5}
                    value={sub.refreshIntervalMinutes}
                    onChange={(e) =>
                      void handleSetInterval(
                        sub.id,
                        Number(e.target.value) || 0,
                      )
                    }
                    className="w-16 rounded border px-1 py-0.5 text-xs"
                    style={{
                      borderColor: "var(--border-soft)",
                      background: "var(--bg-base)",
                      color: "var(--fg-base)",
                    }}
                  />
                  <span style={{ color: "var(--fg-subtle)" }}>
                    {t("settings.calendar.subs.minutes")}
                  </span>
                </label>
                <span
                  className="ml-auto"
                  style={{ color: "var(--fg-subtle)" }}
                  title={sub.lastError ?? undefined}
                >
                  {sub.lastError
                    ? `✗ ${shortError(sub.lastError)}`
                    : sub.lastRefreshed
                      ? t("settings.calendar.subs.lastRefreshed", {
                          time: formatRelative(sub.lastRefreshed),
                        })
                      : t("settings.calendar.subs.notYetRefreshed")}
                </span>
                <button
                  type="button"
                  onClick={() => void handleRefresh(sub.id)}
                  disabled={busyId === sub.id}
                  className="rounded px-2 py-0.5 text-xs"
                  style={{
                    background: "var(--bg-soft)",
                    color: "var(--fg-base)",
                    border: "1px solid var(--border-soft)",
                  }}
                >
                  {busyId === sub.id
                    ? t("settings.calendar.subs.refreshing")
                    : t("settings.calendar.subs.refresh")}
                </button>
                <button
                  type="button"
                  onClick={() => void handleRemove(sub.id, sub.name)}
                  disabled={busyId === sub.id}
                  className="rounded px-2 py-0.5 text-xs"
                  style={{
                    background: "var(--bg-soft)",
                    color: "var(--fg-error, #c00)",
                    border: "1px solid var(--border-soft)",
                  }}
                >
                  {t("settings.calendar.subs.remove")}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {showAdd ? (
        <AddSubscriptionForm
          onCancel={() => setShowAdd(false)}
          onAdded={() => {
            setShowAdd(false);
            void reload();
          }}
        />
      ) : (
        <button
          type="button"
          onClick={() => setShowAdd(true)}
          className="self-start rounded px-3 py-1 text-xs"
          style={{
            background: "var(--bg-soft)",
            color: "var(--fg-base)",
            border: "1px solid var(--border-soft)",
          }}
        >
          + {t("settings.calendar.subs.add")}
        </button>
      )}
    </fieldset>
  );
}

function AddSubscriptionForm({
  onCancel,
  onAdded,
}: {
  onCancel: () => void;
  onAdded: () => void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<"url" | "file">("url");
  const [url, setUrl] = useState("");
  const [path, setPath] = useState("");
  const [interval, setInterval] = useState(60);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pickFile = async () => {
    try {
      const picked = await openDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "iCalendar", extensions: ["ics", "ICS"] }],
      });
      if (!picked) return;
      const p = Array.isArray(picked) ? picked[0] : picked;
      if (p) setPath(p);
    } catch (e) {
      setError(String(e));
    }
  };

  const submit = async () => {
    if (busy) return;
    const trimmedName = name.trim();
    if (!trimmedName) {
      setError(t("settings.calendar.subs.nameRequired"));
      return;
    }
    const source: SubscriptionSource =
      kind === "url"
        ? { kind: "url", url: url.trim() }
        : { kind: "file", path: path.trim() };
    // Discriminate on `source.kind` rather than the parallel `kind`
    // state — TS narrows the union from the discriminator field, not
    // from an externally-tracked variable.
    if (source.kind === "url" && !source.url) {
      setError(t("settings.calendar.subs.urlRequired"));
      return;
    }
    if (source.kind === "file" && !source.path) {
      setError(t("settings.calendar.subs.pathRequired"));
      return;
    }
    setBusy(true);
    try {
      await invoke<CalendarSubscription>("cal_subs_add", {
        name: trimmedName,
        source,
        refreshIntervalMinutes: interval,
      });
      onAdded();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <div
      className="flex flex-col gap-2 rounded border px-2 py-2"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-soft)",
      }}
    >
      <input
        type="text"
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder={t("settings.calendar.subs.namePlaceholder")}
        className="rounded border px-2 py-1 text-sm"
        style={{
          borderColor: "var(--border-soft)",
          background: "var(--bg-base)",
          color: "var(--fg-base)",
        }}
      />

      <div className="flex gap-3 text-xs" style={{ color: "var(--fg-muted)" }}>
        <label className="flex items-center gap-1">
          <input
            type="radio"
            name="sub-kind"
            checked={kind === "url"}
            onChange={() => setKind("url")}
          />
          {t("settings.calendar.subs.kindUrl")}
        </label>
        <label className="flex items-center gap-1">
          <input
            type="radio"
            name="sub-kind"
            checked={kind === "file"}
            onChange={() => setKind("file")}
          />
          {t("settings.calendar.subs.kindFile")}
        </label>
      </div>

      {kind === "url" ? (
        <input
          type="url"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://… (or webcal://…)"
          className="rounded border px-2 py-1 text-xs font-mono"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-base)",
          }}
        />
      ) : (
        <div className="flex gap-2">
          <input
            type="text"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="C:\path\to\calendar.ics"
            className="flex-1 rounded border px-2 py-1 text-xs font-mono"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
              color: "var(--fg-base)",
            }}
          />
          <button
            type="button"
            onClick={() => void pickFile()}
            className="rounded px-2 py-1 text-xs"
            style={{
              background: "var(--bg-base)",
              color: "var(--fg-base)",
              border: "1px solid var(--border-soft)",
            }}
          >
            {t("settings.calendar.subs.browse")}
          </button>
        </div>
      )}

      <label
        className="flex items-center gap-2 text-xs"
        style={{ color: "var(--fg-muted)" }}
      >
        <span>{t("settings.calendar.subs.interval")}</span>
        <input
          type="number"
          min={0}
          step={5}
          value={interval}
          onChange={(e) => setInterval(Number(e.target.value) || 0)}
          className="w-16 rounded border px-1 py-0.5 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-base)",
          }}
        />
        <span style={{ color: "var(--fg-subtle)" }}>
          {t("settings.calendar.subs.minutes")}
        </span>
      </label>

      {error && (
        <div className="text-xs" style={{ color: "var(--fg-error, #c00)" }}>
          {error}
        </div>
      )}

      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onCancel}
          disabled={busy}
          className="rounded px-3 py-1 text-xs"
          style={{
            background: "var(--bg-base)",
            color: "var(--fg-base)",
            border: "1px solid var(--border-soft)",
          }}
        >
          {t("settings.calendar.subs.cancel")}
        </button>
        <button
          type="button"
          onClick={() => void submit()}
          disabled={busy}
          className="rounded px-3 py-1 text-xs font-medium"
          style={{
            background: "var(--accent)",
            color: "#fff",
            border: "1px solid var(--border-soft)",
          }}
        >
          {busy
            ? t("settings.calendar.subs.adding")
            : t("settings.calendar.subs.confirmAdd")}
        </button>
      </div>
    </div>
  );
}

/**
 * Click-to-cycle color picker for a subscription. A single click steps
 * through the shared palette; a right-click opens the native color
 * input as an "anything else" escape hatch (rare path — palette covers
 * the common case). The current selection is shown as a small filled
 * circle.
 */
function ColorSwatch({
  color,
  onPick,
}: {
  color: string;
  onPick: (c: string) => void;
}) {
  const cycleNext = () => {
    const idx = SUBSCRIPTION_PALETTE.findIndex(
      (c) => c.toLowerCase() === color.toLowerCase(),
    );
    const next =
      SUBSCRIPTION_PALETTE[(idx + 1) % SUBSCRIPTION_PALETTE.length];
    onPick(next);
  };
  return (
    <label
      className="relative inline-flex h-4 w-4 cursor-pointer items-center justify-center rounded-full"
      title="Klick: nächste Palette-Farbe · Rechtsklick: freie Farbe"
      style={{
        background: color,
        border: "1px solid rgba(0,0,0,0.2)",
      }}
      onClick={(e) => {
        // Only cycle on plain left-clicks; right-click handled by the
        // native context menu opening the hidden color input.
        if (e.button === 0 && !e.shiftKey) {
          e.preventDefault();
          cycleNext();
        }
      }}
    >
      <input
        type="color"
        value={color}
        onChange={(e) => onPick(e.target.value)}
        // Hidden but reachable via right-click + "Choose color…" on
        // some platforms; on others the user shift-clicks and the
        // browser focuses the input directly.
        className="absolute inset-0 h-0 w-0 cursor-pointer opacity-0"
        aria-label="Eigene Farbe wählen"
      />
    </label>
  );
}

function formatRelative(iso: string): string {
  const d = new Date(iso);
  const diffMs = Date.now() - d.getTime();
  const min = Math.round(diffMs / 60_000);
  if (min < 1) return "<1min";
  if (min < 60) return `${min}min`;
  const h = Math.round(min / 60);
  if (h < 48) return `${h}h`;
  const days = Math.round(h / 24);
  return `${days}d`;
}

function shortError(msg: string): string {
  if (msg.length <= 40) return msg;
  return msg.slice(0, 37) + "…";
}
