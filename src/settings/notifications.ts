/**
 * Notification preferences. Lives in localStorage because these are
 * pure UI hints — no need to round-trip through the Rust store, and
 * no need to sync across devices (a chime preference is inherently
 * per-machine).
 */

export type NotificationSettings = {
  /** Play the chime when new mail lands in the unified inbox. */
  soundEnabled: boolean;
  /** Linear 0..1. 0.5 is a reasonable "subtle" default. */
  soundVolume: number;
};

export const NOTIFICATION_DEFAULTS: NotificationSettings = {
  soundEnabled: true,
  soundVolume: 0.5,
};

const STORAGE_KEY = "crystalmail:notifications";

export function loadNotificationSettings(): NotificationSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...NOTIFICATION_DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<NotificationSettings>;
    return {
      soundEnabled:
        typeof parsed.soundEnabled === "boolean"
          ? parsed.soundEnabled
          : NOTIFICATION_DEFAULTS.soundEnabled,
      soundVolume:
        typeof parsed.soundVolume === "number"
          ? Math.min(1, Math.max(0, parsed.soundVolume))
          : NOTIFICATION_DEFAULTS.soundVolume,
    };
  } catch {
    return { ...NOTIFICATION_DEFAULTS };
  }
}

export function saveNotificationSettings(s: NotificationSettings): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(s));
  } catch {
    // localStorage may be disabled; in-memory state still works for
    // the session.
  }
}
