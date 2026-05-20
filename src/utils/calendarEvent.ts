/**
 * Hex palette mirroring the Rust-side `PALETTE` in
 * `timeprotocol/subscriptions.rs`. Kept in sync by convention; if you
 * add/remove a slot here, do the same there so round-robin assignment
 * lines up. We re-export the values for the settings color-picker.
 */
export const SUBSCRIPTION_PALETTE = [
  "#3b82f6", // blue
  "#ef4444", // red
  "#10b981", // emerald
  "#f59e0b", // amber
  "#8b5cf6", // violet
  "#ec4899", // pink
  "#06b6d4", // cyan
  "#84cc16", // lime
] as const;

/**
 * Color CSS value to use for one event's bar / pill in the calendar
 * views. Subscribed events use their subscription's `color`; everything
 * else (manual, ICS-imports, negotiations) falls back to the app's
 * accent variable so light/dark theme switching still works.
 *
 * Pass the subscription map pre-keyed by id so this stays O(1) per call
 * — the caller does the `new Map(subs.map(s => [s.id, s]))` build once.
 */
export function eventColor(
  ev: { subscriptionId: string | null },
  subscriptionsById: ReadonlyMap<string, { color: string }>,
): string {
  if (ev.subscriptionId) {
    const sub = subscriptionsById.get(ev.subscriptionId);
    if (sub) return sub.color;
  }
  return "var(--accent)";
}

// Display-time heuristics for calendar events. Phase 1 doesn't carry an
// `all_day` boolean on the wire — DATE-only ICS imports land as RFC 3339
// timestamps at local midnight, and the user-facing distinction lives
// entirely in the renderer. These helpers centralize the recognition so
// the week- and month-views agree on what counts as a bar-on-top event.
//
// When the schema gains an explicit `allDay` field later, only this file
// changes; callers keep the same boolean.

import type { Commitment } from "../types";

const DAY_MS = 24 * 60 * 60 * 1000;
/// A few seconds of slack so events stored as `23:59:59`-ends (some
/// legacy clients write that instead of next-midnight) still register as
/// all-day. Anything within 90s of a clean day-multiple counts.
const SLACK_MS = 90_000;

/**
 * `true` when the event should render as a bar in the all-day track
 * rather than inside the hour grid. Heuristic:
 *   1. Start is at local midnight (00:00:00)
 *   2. Duration is a non-zero multiple of 24h (within ±90s slack)
 *
 * That covers:
 *   * ICS imports with `DTSTART;VALUE=DATE:…` (Rust parser maps these
 *     to local-midnight RFC 3339 — see `ics_time_to_rfc3339`)
 *   * Manually created "ganztägige" events the user happened to enter
 *     as 00:00 → 00:00 next day
 *   * Multi-day spans of the same shape
 *
 * It deliberately does NOT match a "regular" 24h event that happens to
 * start at midnight by coincidence — those are vanishingly rare in
 * practice and the cost of mis-classifying one of them is just "shows
 * up in the wrong track", not data loss.
 */
export function isAllDayEvent(ev: Commitment): boolean {
  const start = new Date(ev.startAt);
  const end = new Date(ev.endAt);
  if (
    start.getHours() !== 0 ||
    start.getMinutes() !== 0 ||
    start.getSeconds() !== 0
  ) {
    return false;
  }
  const durationMs = end.getTime() - start.getTime();
  if (durationMs <= 0) return false;
  const remainder = durationMs % DAY_MS;
  return remainder <= SLACK_MS || remainder >= DAY_MS - SLACK_MS;
}

/**
 * Convert an HTML5 `datetime-local`-shaped string (timezone-naïve,
 * `YYYY-MM-DDTHH:MM` or `YYYY-MM-DDTHH:MM:SS`) into an RFC 3339
 * timestamp with the system's local offset applied. The naive input is
 * interpreted as wall-clock time in the user's TZ.
 *
 * Used by:
 *   - EventEditor: form input → backend round-trip
 *   - Reader: pi-extracted naïve event-times → backend-shaped startAt
 */
export function localDateTimeToRfc3339(local: string): string {
  const d = new Date(local);
  const Y = d.getFullYear();
  const M = pad2(d.getMonth() + 1);
  const D = pad2(d.getDate());
  const h = pad2(d.getHours());
  const m = pad2(d.getMinutes());
  const s = pad2(d.getSeconds());
  // JS getTimezoneOffset returns minutes WEST of UTC (positive for negative
  // offsets), so flip the sign.
  const offMin = -d.getTimezoneOffset();
  const sign = offMin >= 0 ? "+" : "-";
  const absMin = Math.abs(offMin);
  const oh = pad2(Math.floor(absMin / 60));
  const om = pad2(absMin % 60);
  return `${Y}-${M}-${D}T${h}:${m}:${s}${sign}${oh}:${om}`;
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/**
 * How many local days an all-day event covers. Returns `1` for a single
 * day, `7` for a full week, etc. Uses ceil(duration/24h) so a 23:59:59-
 * end-time-style event still reports the right span.
 *
 * Result is clamped to a sensible minimum of 1 so a malformed
 * zero-duration event still renders as a one-day bar instead of vanishing.
 */
export function allDaySpanInDays(ev: Commitment): number {
  const start = new Date(ev.startAt);
  const end = new Date(ev.endAt);
  const durationMs = end.getTime() - start.getTime();
  if (durationMs <= 0) return 1;
  return Math.max(1, Math.round(durationMs / DAY_MS));
}
