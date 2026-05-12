// Phase 3.4 calendar — month grid (6 weeks × 7 days).
//
// Layout split into six WeekRow components, each handling:
//   1. Day-number header row (7 cells)
//   2. All-day track: bars spanning the day columns the event covers,
//      clipped at the week boundary (a multi-week event continues into
//      the next WeekRow's track with the same visual treatment).
//   3. Timed-event lane: per-day vertical stack of bars showing
//      hour-anchored events, capped at MAX_VISIBLE with "+N more".
//
// Days outside the anchor's month render muted but stay clickable so
// the user can drift into adjacent months without first hitting the
// navigation arrows.

import { useMemo } from "react";
import type { Commitment } from "../types";
import {
  allDaySpanInDays,
  eventColor,
  isAllDayEvent,
} from "../utils/calendarEvent";

type SubInfo = { color: string; name: string };

type Props = {
  /** Any date in the month to render. */
  anchorDate: Date;
  events: Commitment[];
  /** Subscription id → display info. See CalendarWeekView for details. */
  subscriptionsById: ReadonlyMap<string, SubInfo>;
  onPickEvent: (id: string) => void;
  /** Click on an empty area of a day cell. Defaults to 09:00 local
   *  on that day; the parent opens the EventEditor with that
   *  pre-fill. */
  onCreateAt: (start: Date) => void;
};

const MAX_VISIBLE_TIMED = 3;
const DEFAULT_HOUR = 9;
const DAY_MS = 24 * 60 * 60 * 1000;

const DAY_NUM_FMT = new Intl.DateTimeFormat(undefined, { day: "2-digit" });
const WEEKDAY_HEAD_FMT = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
});
const TIME_FMT = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
});

export function CalendarMonthView({
  anchorDate,
  events,
  subscriptionsById,
  onPickEvent,
  onCreateAt,
}: Props) {
  const gridStart = useMemo(() => firstGridDay(anchorDate), [anchorDate]);
  const weeks = useMemo(
    () =>
      Array.from({ length: 6 }, (_, w) => {
        const wStart = new Date(gridStart);
        wStart.setDate(wStart.getDate() + w * 7);
        return wStart;
      }),
    [gridStart],
  );
  const anchorMonth = anchorDate.getMonth();
  const anchorYear = anchorDate.getFullYear();

  // Split events once, share the result across all week rows. The two
  // tracks have very different layout rules — pre-partitioning saves
  // each WeekRow from doing the same isAllDay() pass.
  const { timed, allDay } = useMemo(() => {
    const t: Commitment[] = [];
    const a: Commitment[] = [];
    for (const e of events) {
      if (e.status === "CANCELLED") continue;
      if (isAllDayEvent(e)) a.push(e);
      else t.push(e);
    }
    return { timed: t, allDay: a };
  }, [events]);

  // Header weekday labels — derived from `gridStart` so we follow the
  // active locale's first-day convention.
  const headerDays = Array.from({ length: 7 }, (_, i) => {
    const d = new Date(gridStart);
    d.setDate(d.getDate() + i);
    return d;
  });

  return (
    <div className="flex h-full flex-col">
      {/* Weekday header */}
      <div
        className="grid"
        style={{
          gridTemplateColumns: "repeat(7, 1fr)",
          borderBottom: "1px solid var(--border-base)",
        }}
      >
        {headerDays.map((d) => (
          <div
            key={d.toISOString()}
            className="px-2 py-1 text-[11px] font-medium uppercase tracking-wide"
            style={{ color: "var(--fg-subtle)" }}
          >
            {WEEKDAY_HEAD_FMT.format(d)}
          </div>
        ))}
      </div>

      {/* Six week rows */}
      <div className="flex flex-1 flex-col">
        {weeks.map((wStart) => (
          <WeekRow
            key={wStart.toISOString()}
            weekStart={wStart}
            anchorMonth={anchorMonth}
            anchorYear={anchorYear}
            timedEvents={timed}
            allDayEvents={allDay}
            subscriptionsById={subscriptionsById}
            onPickEvent={onPickEvent}
            onCreateAt={onCreateAt}
          />
        ))}
      </div>
    </div>
  );
}

// ─── WeekRow ─────────────────────────────────────────────────────────────

function WeekRow({
  weekStart,
  anchorMonth,
  anchorYear,
  timedEvents,
  allDayEvents,
  subscriptionsById,
  onPickEvent,
  onCreateAt,
}: {
  weekStart: Date;
  anchorMonth: number;
  anchorYear: number;
  timedEvents: Commitment[];
  allDayEvents: Commitment[];
  subscriptionsById: ReadonlyMap<string, SubInfo>;
  onPickEvent: (id: string) => void;
  onCreateAt: (start: Date) => void;
}) {
  const days = useMemo(
    () =>
      Array.from({ length: 7 }, (_, i) => {
        const d = new Date(weekStart);
        d.setDate(d.getDate() + i);
        return d;
      }),
    [weekStart],
  );

  const today = startOfDay(new Date());
  const weekStartMs = startOfDay(weekStart).getTime();
  const weekEndMs = weekStartMs + 7 * DAY_MS;

  // Layout all-day bars that touch this week — start column + span,
  // clipped to the week. Bars stacked one per row (no packing): real
  // users rarely have more than 2-3 overlapping, and the simplicity
  // pays for itself.
  const bars = useMemo(
    () => layoutAllDayBars(allDayEvents, weekStartMs, weekEndMs),
    [allDayEvents, weekStartMs, weekEndMs],
  );

  // Timed events bucketed by start day (yyyy-mm-dd local) so each cell
  // can pull its slice in O(1).
  const timedByDay = useMemo(() => {
    const map = new Map<string, Commitment[]>();
    for (const e of timedEvents) {
      const s = new Date(e.startAt).getTime();
      if (s < weekStartMs || s >= weekEndMs) continue;
      const key = dayKey(new Date(e.startAt));
      const list = map.get(key) ?? [];
      list.push(e);
      map.set(key, list);
    }
    for (const list of map.values()) {
      list.sort((a, b) => a.startAt.localeCompare(b.startAt));
    }
    return map;
  }, [timedEvents, weekStartMs, weekEndMs]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* Day-number strip + cell backgrounds. The whole row is one
          grid; backgrounds, day numbers, all-day bars, and timed
          events all align to the same 7 columns. */}
      <div
        className="relative grid flex-1"
        style={{ gridTemplateColumns: "repeat(7, 1fr)" }}
      >
        {/* One cell per weekday — provides border + click-target + the
            timed events listing. */}
        {days.map((d) => {
          const dayStart = startOfDay(d);
          const isToday = dayStart.getTime() === today.getTime();
          const inAnchorMonth =
            d.getMonth() === anchorMonth && d.getFullYear() === anchorYear;
          const dayList = timedByDay.get(dayKey(d)) ?? [];
          const visible = dayList.slice(0, MAX_VISIBLE_TIMED);
          const overflow = Math.max(0, dayList.length - MAX_VISIBLE_TIMED);

          const handleCellClick = () => {
            const start = new Date(d);
            start.setHours(DEFAULT_HOUR, 0, 0, 0);
            onCreateAt(start);
          };

          return (
            <div
              key={d.toISOString()}
              className="flex min-h-0 flex-col gap-0.5 px-1 py-1 text-[11px]"
              style={{
                borderRight: "1px solid var(--border-soft)",
                borderBottom: "1px solid var(--border-soft)",
                background: isToday ? "var(--bg-soft)" : "transparent",
                opacity: inAnchorMonth ? 1 : 0.45,
                cursor: "pointer",
              }}
              onClick={handleCellClick}
            >
              <div
                className="text-[11px] font-medium tabular-nums"
                style={{
                  color: isToday ? "var(--accent)" : "var(--fg-base)",
                }}
              >
                {DAY_NUM_FMT.format(d)}
              </div>

              {/* Reserve vertical space equal to the number of all-day
                  bars in this week — so the timed events start *below*
                  the bar lane regardless of which day they fall on.
                  Each bar slot is ~18px tall. */}
              {bars.length > 0 && (
                <div
                  aria-hidden
                  style={{ height: `${bars.length * 18}px` }}
                />
              )}

              <div className="flex min-h-0 flex-col gap-0.5 overflow-hidden">
                {visible.map((e) => (
                  <button
                    key={e.id}
                    type="button"
                    onClick={(ev) => {
                      ev.stopPropagation();
                      onPickEvent(e.id);
                    }}
                    className="truncate rounded px-1 py-0.5 text-left text-[10px] hover:opacity-80"
                    style={{
                      background: eventColor(e, subscriptionsById),
                      color: "#fff",
                    }}
                    title={`${e.summary ?? "—"}\n${TIME_FMT.format(new Date(e.startAt))}`}
                  >
                    {TIME_FMT.format(new Date(e.startAt))}{" "}
                    {e.summary ?? "—"}
                  </button>
                ))}
                {overflow > 0 && (
                  <span
                    className="px-1 text-[10px]"
                    style={{ color: "var(--fg-subtle)" }}
                  >
                    +{overflow}
                  </span>
                )}
              </div>
            </div>
          );
        })}

        {/* All-day bars: absolute-positioned over the day cells so a
            single bar visually spans multiple columns. We use the same
            7-column flex math as the underlying grid; `left` and
            `width` are percent of the row's width. The bar lane sits
            below the day-number row (top: ~1.5em) — height tweakable
            via the bar-slot constant. */}
        {bars.map((bar, idx) => (
          <button
            key={bar.commitment.id}
            type="button"
            onClick={(ev) => {
              ev.stopPropagation();
              onPickEvent(bar.commitment.id);
            }}
            className="absolute truncate rounded px-1.5 text-left text-[10px] leading-tight hover:opacity-80"
            style={{
              left: `calc(${(bar.startCol / 7) * 100}% + 2px)`,
              width: `calc(${(bar.spanCols / 7) * 100}% - 4px)`,
              top: `calc(1.4em + ${idx * 18}px)`,
              height: "16px",
              background: eventColor(bar.commitment, subscriptionsById),
              color: "#fff",
              border: "1px solid rgba(0,0,0,0.1)",
            }}
            title={bar.commitment.summary ?? "—"}
          >
            {bar.continuesLeft ? "← " : ""}
            {bar.commitment.summary ?? "—"}
            {bar.continuesRight ? " →" : ""}
          </button>
        ))}
      </div>
    </div>
  );
}

// ─── All-day layout ──────────────────────────────────────────────────────

type AllDayBar = {
  commitment: Commitment;
  startCol: number;
  spanCols: number;
  continuesLeft: boolean;
  continuesRight: boolean;
};

/** Clip each event to `[weekStartMs, weekEndMs)`, compute its column
 *  span, and order by descending length (longest at the top of the
 *  bar lane). One row per event — no packing. */
function layoutAllDayBars(
  events: Commitment[],
  weekStartMs: number,
  weekEndMs: number,
): AllDayBar[] {
  const out: AllDayBar[] = [];
  for (const e of events) {
    const startMs = new Date(e.startAt).getTime();
    const endMs = new Date(e.endAt).getTime();
    if (startMs >= weekEndMs || endMs <= weekStartMs) continue;
    const clampedStart = Math.max(startMs, weekStartMs);
    const clampedEnd = Math.min(endMs, weekEndMs);
    const startCol = Math.floor((clampedStart - weekStartMs) / DAY_MS);
    const totalDays = allDaySpanInDays(e);
    const visibleDays = Math.min(totalDays, 7 - startCol);
    const spanCols = Math.max(
      1,
      Math.min(visibleDays, Math.ceil((clampedEnd - clampedStart) / DAY_MS)),
    );
    out.push({
      commitment: e,
      startCol,
      spanCols,
      continuesLeft: startMs < weekStartMs,
      continuesRight: endMs > weekEndMs,
    });
  }
  out.sort((a, b) => {
    if (b.spanCols !== a.spanCols) return b.spanCols - a.spanCols;
    return a.startCol - b.startCol;
  });
  return out;
}

// ─── Date helpers ────────────────────────────────────────────────────────

function startOfDay(d: Date): Date {
  const out = new Date(d);
  out.setHours(0, 0, 0, 0);
  return out;
}

/** First day of the 6×7 grid: the Monday on or before the first of
 *  `anchor`'s month. JS getDay(): 0=Sun…6=Sat. */
function firstGridDay(anchor: Date): Date {
  const firstOfMonth = new Date(anchor.getFullYear(), anchor.getMonth(), 1);
  const dow = firstOfMonth.getDay();
  const back = (dow + 6) % 7; // Mon=0, Sun=6
  firstOfMonth.setDate(firstOfMonth.getDate() - back);
  firstOfMonth.setHours(0, 0, 0, 0);
  return firstOfMonth;
}

function dayKey(d: Date): string {
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}
