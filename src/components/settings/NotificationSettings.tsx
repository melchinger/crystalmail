import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  loadNotificationSettings,
  saveNotificationSettings,
  type NotificationSettings as Prefs,
} from "../../settings/notifications";
import { playNotifySound } from "../../utils/notifySound";

/**
 * Notification preferences panel. Kept deliberately small: sound
 * toggle + volume slider. Window-title badging is automatic and
 * doesn't need user choice.
 *
 * "Test" button plays the chime at the current volume so the user
 * can dial it in without waiting for a real mail to arrive.
 */
export function NotificationSettings() {
  const { t } = useTranslation();
  const [prefs, setPrefs] = useState<Prefs>(() => loadNotificationSettings());

  const update = (next: Prefs) => {
    setPrefs(next);
    saveNotificationSettings(next);
    // Broadcast so App-level listeners can pick up the new prefs
    // without re-reading localStorage on every tick.
    window.dispatchEvent(new CustomEvent("cm:notifications:changed"));
  };

  return (
    <div className="flex flex-col gap-5">
      <header>
        <h2 className="text-base font-semibold">
          {t("settings.notifications.title")}
        </h2>
        <p className="mt-1 text-xs" style={{ color: "var(--fg-muted)" }}>
          {t("settings.notifications.hint")}
        </p>
      </header>

      <section
        className="flex flex-col gap-3 rounded-md border p-3"
        style={{
          borderColor: "var(--border-soft)",
          background: "var(--bg-base)",
        }}
      >
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={prefs.soundEnabled}
            onChange={(e) =>
              update({ ...prefs, soundEnabled: e.target.checked })
            }
          />
          <span>{t("settings.notifications.soundEnabled")}</span>
        </label>

        <div className="flex flex-col gap-1">
          <label
            className="text-xs"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("settings.notifications.soundVolume")}:{" "}
            {Math.round(prefs.soundVolume * 100)}%
          </label>
          <div className="flex items-center gap-2">
            <input
              type="range"
              min={0}
              max={1}
              step={0.05}
              value={prefs.soundVolume}
              disabled={!prefs.soundEnabled}
              onChange={(e) =>
                update({
                  ...prefs,
                  soundVolume: Number(e.target.value),
                })
              }
              className="flex-1"
            />
            <button
              type="button"
              onClick={() => playNotifySound(prefs.soundVolume)}
              disabled={!prefs.soundEnabled}
              className="rounded-md border px-2 py-0.5 text-xs"
              style={{
                borderColor: "var(--border-base)",
                color: "var(--fg-base)",
                opacity: prefs.soundEnabled ? 1 : 0.5,
              }}
            >
              {t("settings.notifications.test")}
            </button>
          </div>
          <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t("settings.notifications.soundVolumeHint")}
          </p>
        </div>
      </section>
    </div>
  );
}
