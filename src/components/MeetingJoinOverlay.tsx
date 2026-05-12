// Bottom-anchored, non-blocking overlay that surfaces the next upcoming
// meeting once it gets close, then sticks around with a remaining-time
// readout while the meeting is running. Pattern intentionally mirrors
// `UndoSendOverlay` — same Tailwind utilities, same color tokens, same
// "pill above the footer" placement — so the two read as a family.
//
// Activation logic:
//   - "prejoin": now is within `PRE_JOIN_MS` of `startAt` and the event's
//     LOCATION carries a clickable http(s) URL.
//   - "running": now is between `startAt` and `endAt`. The URL is still
//     surfaced via Beitreten so the user can re-open a closed tab.
// Both phases offer ✕ to suppress this specific (event, phase) pair until
// the session ends — we hold the suppression set in sessionStorage so a
// reload during the same workday respects the user's dismissal but a
// fresh app launch tomorrow won't.
//
// Data source: this component polls `cal_list_in_range` over a tight
// window every `POLL_INTERVAL_MS`. Cheap query (local SQLite, indexed on
// `start_at`) and avoids lifting calendar state to App — at the cost of
// up-to-60s lag when a brand-new event with a meeting link is created
// elsewhere. Acceptable tradeoff for a v1.

import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import type { Commitment } from "../types";
import { useNow } from "../hooks/useNow";
import { detectMeetingUrl } from "../utils/meetingLink";

/** Show the prejoin pill this long before `startAt`. Matches Outlook /
 *  Google Calendar default "10 min before"... we go shorter (5 min) so
 *  the pill doesn't squat on the screen during the previous meeting. */
const PRE_JOIN_MS = 5 * 60 * 1000;

/** How often we re-fetch events. The visible countdown updates at 1 Hz
 *  via `useNow` regardless of this. */
const POLL_INTERVAL_MS = 60 * 1000;

/** How far back / forward we look. Back-window covers events that
 *  started before app launch but are still running. Forward window only
 *  needs to span PRE_JOIN_MS + safety margin. */
const LOOKBACK_MS = 4 * 60 * 60 * 1000;
const LOOKAHEAD_MS = 30 * 60 * 1000;

const DISMISS_STORAGE_KEY = "crystalmail.meetingOverlay.dismissed";

type Phase = "prejoin" | "running";

type Candidate = {
  event: Commitment;
  url: string;
  phase: Phase;
  /** `${event.id}|${phase}` — distinguishes the two banners that the same
   *  event surfaces. Dismissing "prejoin" must not also dismiss "running". */
  phaseKey: string;
};

function loadDismissed(): Set<string> {
  try {
    const raw = sessionStorage.getItem(DISMISS_STORAGE_KEY);
    if (!raw) return new Set();
    const arr = JSON.parse(raw) as unknown;
    if (!Array.isArray(arr)) return new Set();
    return new Set(arr.filter((v): v is string => typeof v === "string"));
  } catch {
    return new Set();
  }
}

function saveDismissed(s: Set<string>): void {
  try {
    sessionStorage.setItem(DISMISS_STORAGE_KEY, JSON.stringify([...s]));
  } catch {
    // sessionStorage can throw in private mode etc — non-fatal, the user
    // just won't get persistence across reload.
  }
}

export function MeetingJoinOverlay() {
  const { t } = useTranslation();
  const now = useNow(1000);
  const [events, setEvents] = useState<Commitment[]>([]);
  const [dismissed, setDismissed] = useState<Set<string>>(() => loadDismissed());

  const fetchEvents = useCallback(async () => {
    try {
      const nowMs = Date.now();
      const from = new Date(nowMs - LOOKBACK_MS).toISOString();
      const to = new Date(nowMs + LOOKAHEAD_MS).toISOString();
      const rows = await invoke<Commitment[]>("cal_list_in_range", { from, to });
      setEvents(rows);
    } catch (err) {
      // The overlay is best-effort UX — never throw an error toast at the
      // user. Console-log for triage.
      // eslint-disable-next-line no-console
      console.warn("MeetingJoinOverlay fetch failed", err);
    }
  }, []);

  useEffect(() => {
    void fetchEvents();
    const interval = window.setInterval(() => void fetchEvents(), POLL_INTERVAL_MS);
    const onFocus = () => void fetchEvents();
    window.addEventListener("focus", onFocus);
    return () => {
      window.clearInterval(interval);
      window.removeEventListener("focus", onFocus);
    };
  }, [fetchEvents]);

  const candidate: Candidate | null = useMemo(() => {
    const nowMs = now.getTime();
    let best: Candidate | null = null;
    let bestStartMs = Number.POSITIVE_INFINITY;
    for (const ev of events) {
      if (ev.status === "CANCELLED") continue;
      const url = detectMeetingUrl(ev.location);
      if (!url) continue;
      const startMs = new Date(ev.startAt).getTime();
      const endMs = new Date(ev.endAt).getTime();
      let phase: Phase | null = null;
      if (nowMs >= startMs && nowMs < endMs) {
        phase = "running";
      } else if (nowMs >= startMs - PRE_JOIN_MS && nowMs < startMs) {
        phase = "prejoin";
      }
      if (!phase) continue;
      const phaseKey = `${ev.id}|${phase}`;
      if (dismissed.has(phaseKey)) continue;
      // Pick the soonest-starting candidate. Two overlapping meetings is
      // rare; if it happens, the earlier one wins until it ends.
      if (startMs < bestStartMs) {
        best = { event: ev, url, phase, phaseKey };
        bestStartMs = startMs;
      }
    }
    return best;
  }, [events, now, dismissed]);

  if (!candidate) return null;

  const { event, url, phase, phaseKey } = candidate;
  const summary = event.summary || t("meetingOverlay.untitled");
  const startMs = new Date(event.startAt).getTime();
  const endMs = new Date(event.endAt).getTime();

  // Round up so "in 1 Min" stays on screen for the whole final minute
  // before tipping into "running".
  const minutesUntilStart = Math.max(1, Math.ceil((startMs - now.getTime()) / 60000));
  const minutesRemaining = Math.max(0, Math.ceil((endMs - now.getTime()) / 60000));

  const handleJoin = () => {
    void openUrl(url).catch((err: unknown) => {
      // eslint-disable-next-line no-console
      console.warn("MeetingJoinOverlay openUrl failed", err);
    });
    // Auto-dismiss the prejoin pill once the user clicked Beitreten — the
    // job's done. During running we keep the pill so the user still sees
    // remaining time; the explicit ✕ dismisses it.
    if (phase === "prejoin") {
      handleDismiss();
    }
  };

  const handleDismiss = () => {
    setDismissed((prev) => {
      const next = new Set(prev);
      next.add(phaseKey);
      saveDismissed(next);
      return next;
    });
  };

  const subtitle =
    phase === "prejoin"
      ? t("meetingOverlay.startsInMin", {
          count: minutesUntilStart,
          minutes: minutesUntilStart,
        })
      : t("meetingOverlay.remainingMin", {
          count: minutesRemaining,
          minutes: minutesRemaining,
        });

  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-24 z-40 flex justify-center">
      <div
        className="pointer-events-auto flex items-center gap-3 rounded-full border px-4 py-2 text-sm shadow-lg"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
          maxWidth: "min(90vw, 640px)",
        }}
        role="status"
        aria-live="polite"
      >
        <span aria-hidden style={{ color: "var(--accent)", fontSize: "1.1em" }}>
          {phase === "running" ? "●" : "▶"}
        </span>
        <div className="flex min-w-0 flex-col">
          <span className="truncate font-medium">{summary}</span>
          <span className="truncate text-xs" style={{ color: "var(--fg-muted)" }}>
            {subtitle}
          </span>
        </div>
        <button
          type="button"
          onClick={handleJoin}
          className="rounded-md border px-2.5 py-0.5 text-xs font-medium"
          style={{
            borderColor: "var(--border-base)",
            background: phase === "prejoin" ? "var(--accent)" : "transparent",
            color: phase === "prejoin" ? "#fff" : "var(--accent)",
          }}
        >
          {t("meetingOverlay.join")}
        </button>
        <button
          type="button"
          onClick={handleDismiss}
          aria-label={t("meetingOverlay.dismiss")}
          title={t("meetingOverlay.dismiss")}
          className="rounded-md px-1.5 py-0.5 text-sm leading-none"
          style={{ color: "var(--fg-muted)" }}
        >
          ✕
        </button>
      </div>
    </div>
  );
}
