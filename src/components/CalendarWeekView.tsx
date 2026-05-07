// Phase 3.4 calendar — week grid (Mon–Sun × hours).
//
// Layout strategy:
//   - Outer flex column that fills the available height of its
//     scrollable parent. The header (weekday strip) is fixed-height;
//     the body grid takes `flex: 1` and grows as the window grows.
//   - Inside the body: an 8-column CSS grid (1 narrow time-label
//     column + 7 day columns). Each day column is `position: relative;
//     height: 100%`; events inside the column are absolutely
//     positioned with `top` and `height` expressed as **percentages**
//     of the visible time window. That makes the grid scale-to-fit:
//     bigger window → bigger event blocks → larger readable text.
//   - Display window 06:00–22:00 (16 hours). Events outside this
//     range are clamped to the visible edge with a small "earlier"/
//     "later" marker.
//   - Click on an event → onPickEvent.
//   - Click on empty space inside a day column → onCreateAt with the
//     y-position translated to a time (rounded down to the nearest
//     30-minute slot). 1h default duration.
//
// Out of scope (Phase 3.4 minimal cut):
//   - All-day-event strip above the grid (we have no all-day events
//     in the model yet — both `startAt` and `endAt` are timestamps).
//   - Multi-day events as horizontal bars across day columns. We
//     render each day's portion of a multi-day event as a separate
//     bar in that day's column (clamped to the day boundary). Rare
//     in practice for a v1 list.
//   - Drag-to-move / drag-to-resize.

import { useMemo } from "react";
import type { Commitment } from "../types";

type Props = {
  /** Any date in the week to render. The view auto-snaps to the
   *  containing Mon–Sun pair. */
  anchorDate: Date;
  events: Commitment[];
  onPickEvent: (id: string) => void;
  /** Click on empty space inside a day column. The Date carries the
   *  resolved start time (rounded down to a 30-min slot). The parent
   *  opens the EventEditor with that pre-fill. */
  onCreateAt: (start: Date) => void;
};

const VISIBLE_START_HOUR = 6;
const VISIBLE_END_HOUR = 22;
const VISIBLE_HOURS = VISIBLE_END_HOUR - VISIBLE_START_HOUR;
const VISIBLE_MINUTES = VISIBLE_HOURS * 60;

/** Minimum body height in px so the grid stays usable on small
 *  windows. Above this the body fills the parent and scales freely. */
const MIN_BODY_HEIGHT_PX = 720;

const DAY_FMT = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  day: "2-digit",
  month: "short",
});
const TIME_FMT = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
});

export function CalendarWeekView({
  anchorDate,
  events,
  onPickEvent,
  onCreateAt,
}: Props) {
  const weekStart = useMemo(() => mondayOf(anchorDate), [anchorDate]);
  const days = useMemo(
    () =>
      Array.from({ length: 7 }, (_, i) => {
        const d = new Date(weekStart);
        d.setDate(d.getDate() + i);
        return d;
      }),
    [weekStart],
  );
  const weekEnd = useMemo(() => {
    const d = new Date(weekStart);
    d.setDate(d.getDate() + 7);
    return d;
  }, [weekStart]);

  // Filter events overlapping this week (half-open).
  const weekEvents = useMemo(() => {
    return events.filter((e) => {
      if (e.status === "CANCELLED") return false;
      const s = new Date(e.startAt).getTime();
      const en = new Date(e.endAt).getTime();
      return s < weekEnd.getTime() && en > weekStart.getTime();
    });
  }, [events, weekStart, weekEnd]);

  const today = startOfDay(new Date());

  // Hour labels (06:00 … 21:00).
  const hours = Array.from({ length: VISIBLE_HOURS }, (_, i) => VISIBLE_START_HOUR + i);

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* Header row: weekday labels */}
      <div
        className="grid"
        style={{
          gridTemplateColumns: "3rem repeat(7, 1fr)",
          background: "var(--bg-base)",
          borderBottom: "1px solid var(--border-base)",
        }}
      >
        <div />
        {days.map((d) => {
          const isToday = startOfDay(d).getTime() === today.getTime();
          return (
            <div
              key={d.toISOString()}
              className="px-2 py-2 text-xs font-medium"
              style={{
                color: isToday ? "var(--accent)" : "var(--fg-base)",
                borderLeft: "1px solid var(--border-soft)",
              }}
            >
              {DAY_FMT.format(d)}
            </div>
          );
        })}
      </div>

      {/* Body: time-label column + 7 day columns. Fills remaining
          height of the parent so the grid scales with the window
          (bigger window → bigger event blocks → larger readable
          text). `min-height` keeps it usable on small viewports. */}
      <div
        className="grid flex-1 min-h-0"
        style={{
          gridTemplateColumns: "3rem repeat(7, 1fr)",
          minHeight: `${MIN_BODY_HEIGHT_PX}px`,
        }}
      >
        {/* Time-label column */}
        <div className="relative h-full">
          {hours.map((h) => (
            <div
              key={h}
              className="absolute left-0 right-0 pr-1 text-right text-[11px] tabular-nums"
              style={{
                top: `${pctOfDay(h * 60)}%`,
                color: "var(--fg-subtle)",
                transform: "translateY(-50%)",
              }}
            >
              {pad2(h)}:00
            </div>
          ))}
        </div>

        {/* Seven day columns */}
        {days.map((day) => (
          <DayColumn
            key={day.toISOString()}
            day={day}
            events={weekEvents}
            onPickEvent={onPickEvent}
            onCreateAt={onCreateAt}
          />
        ))}
      </div>
    </div>
  );
}

function DayColumn({
  day,
  events,
  onPickEvent,
  onCreateAt,
}: {
  day: Date;
  events: Commitment[];
  onPickEvent: (id: string) => void;
  onCreateAt: (start: Date) => void;
}) {
  const dayStart = startOfDay(day);
  const dayEnd = new Date(dayStart);
  dayEnd.setDate(dayEnd.getDate() + 1);

  // Events overlapping this specific day, with the bar clamped to
  // [dayStart, dayEnd) and to the visible-time window.
  const visibleStartMs =
    dayStart.getTime() + VISIBLE_START_HOUR * 60 * 60 * 1000;
  const visibleEndMs = dayStart.getTime() + VISIBLE_END_HOUR * 60 * 60 * 1000;

  const dayEvents = events
    .map((e) => {
      const s = new Date(e.startAt).getTime();
      const en = new Date(e.endAt).getTime();
      if (s >= dayEnd.getTime() || en <= dayStart.getTime()) return null;
      const clampedStart = Math.max(s, dayStart.getTime());
      const clampedEnd = Math.min(en, dayEnd.getTime());
      const overflowsTop = clampedStart < visibleStartMs;
      const overflowsBottom = clampedEnd > visibleEndMs;
      const visiblyClampedStart = Math.max(clampedStart, visibleStartMs);
      const visiblyClampedEnd = Math.min(clampedEnd, visibleEndMs);
      if (visiblyClampedStart >= visiblyClampedEnd) return null;
      const startMin = (visiblyClampedStart - visibleStartMs) / 60000;
      const durMin = (visiblyClampedEnd - visiblyClampedStart) / 60000;
      return {
        commitment: e,
        topPct: pctOfDay(startMin + VISIBLE_START_HOUR * 60),
        heightPct: (durMin / VISIBLE_MINUTES) * 100,
        overflowsTop,
        overflowsBottom,
        startsAt: new Date(s),
        endsAt: new Date(en),
        durMin,
      };
    })
    .filter((x): x is NonNullable<typeof x> => x !== null);

  const today = startOfDay(new Date());
  const isToday = dayStart.getTime() === today.getTime();
  const isWeekend = day.getDay() === 0 || day.getDay() === 6;

  // Snap clicked y-coordinate to a 30-minute slot. The y position is
  // expressed as a fraction of the column's actual rendered height,
  // so this works no matter how the parent has scaled the grid.
  const onBgClick = (e: React.MouseEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const fraction = (e.clientY - rect.top) / rect.height;
    const minutesFromVisibleStart = fraction * VISIBLE_MINUTES;
    const totalMinutes =
      VISIBLE_START_HOUR * 60 + Math.floor(minutesFromVisibleStart / 30) * 30;
    const start = new Date(dayStart);
    start.setMinutes(totalMinutes);
    onCreateAt(start);
  };

  return (
    <div
      className="relative h-full"
      style={{
        borderLeft: "1px solid var(--border-soft)",
        background: isToday
          ? "var(--bg-soft)"
          : isWeekend
            ? "var(--bg-subtle, transparent)"
            : "transparent",
        cursor: "pointer",
      }}
      onClick={onBgClick}
    >
      {/* Hour grid lines (every full hour) */}
      {Array.from({ length: VISIBLE_HOURS }, (_, i) => (
        <div
          key={i}
          className="absolute left-0 right-0"
          style={{
            top: `${(i / VISIBLE_HOURS) * 100}%`,
            borderTop: "1px solid var(--border-soft)",
            opacity: 0.4,
          }}
        />
      ))}

      {/* Events */}
      {dayEvents.map(
        ({
          commitment,
          topPct,
          heightPct,
          overflowsTop,
          overflowsBottom,
          startsAt,
          endsAt,
          durMin,
        }) => (
          <button
            key={commitment.id}
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onPickEvent(commitment.id);
            }}
            className="absolute left-0.5 right-0.5 overflow-hidden rounded px-1.5 py-1 text-left text-xs leading-tight transition hover:opacity-80"
            style={{
              top: `${topPct}%`,
              height: `${heightPct}%`,
              minHeight: "20px",
              background: "var(--accent)",
              color: "#fff",
              border: "1px solid rgba(0,0,0,0.1)",
            }}
            title={`${commitment.summary ?? "—"}\n${TIME_FMT.format(startsAt)}–${TIME_FMT.format(endsAt)}`}
          >
            {overflowsTop && <span>↑ </span>}
            <span className="font-medium">{commitment.summary ?? "—"}</span>
            {/* Show time line only when the bar is tall enough that
                a second line of text doesn't make it look cramped.
                30 min @ usual scale ≈ enough room. */}
            {durMin >= 30 && (
              <div className="text-[11px] opacity-80">
                {TIME_FMT.format(startsAt)}–{TIME_FMT.format(endsAt)}
              </div>
            )}
            {overflowsBottom && <span> ↓</span>}
          </button>
        ),
      )}
    </div>
  );
}

// ─── Helpers ─────────────────────────────────────────────────────────────

/** Convert "minutes since 00:00" → percentage of the visible window
 *  (0% = top of the visible grid, 100% = bottom). Used for both the
 *  hour labels and the event bar `top` positions, so they stay
 *  aligned at any container height. */
function pctOfDay(minutesSinceMidnight: number): number {
  const m = minutesSinceMidnight - VISIBLE_START_HOUR * 60;
  return (m / VISIBLE_MINUTES) * 100;
}

function startOfDay(d: Date): Date {
  const out = new Date(d);
  out.setHours(0, 0, 0, 0);
  return out;
}

/** Monday of the ISO 8601 week containing `d`. JS getDay(): 0=Sun…6=Sat. */
function mondayOf(d: Date): Date {
  const out = startOfDay(d);
  const dow = out.getDay();
  // Days to subtract: Sun=6, Mon=0, Tue=1, ... Sat=5
  const back = (dow + 6) % 7;
  out.setDate(out.getDate() - back);
  return out;
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}
