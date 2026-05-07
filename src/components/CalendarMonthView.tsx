// Phase 3.4 calendar — month grid (6 rows × 7 cols, ISO week start).
//
// Each cell shows the day number + up to MAX_VISIBLE event bars +
// "+N more" link if overflow. Clicking an empty area in a cell opens
// the EventEditor in create mode pre-filled with that day at 09:00;
// clicking an event bar opens it in edit mode.
//
// Days outside the anchor's month are rendered muted but still
// clickable, so a user can drift into adjacent months without first
// hitting the navigation arrows.

import { useMemo } from "react";
import type { Commitment } from "../types";

type Props = {
  /** Any date in the month to render. */
  anchorDate: Date;
  events: Commitment[];
  onPickEvent: (id: string) => void;
  /** Click on an empty area of a day cell. Defaults to 09:00 local
   *  on that day; the parent opens the EventEditor with that
   *  pre-fill. */
  onCreateAt: (start: Date) => void;
};

const MAX_VISIBLE = 3;
const DEFAULT_HOUR = 9;

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
  onPickEvent,
  onCreateAt,
}: Props) {
  const gridStart = useMemo(() => firstGridDay(anchorDate), [anchorDate]);
  const cells = useMemo(
    () =>
      Array.from({ length: 42 }, (_, i) => {
        const d = new Date(gridStart);
        d.setDate(d.getDate() + i);
        return d;
      }),
    [gridStart],
  );

  const today = startOfDay(new Date());
  const anchorMonth = anchorDate.getMonth();
  const anchorYear = anchorDate.getFullYear();

  // Map: yyyy-mm-dd → events starting on that day. We render events
  // only on their start day for v1 — multi-day events show as a bar
  // in the start cell only. Spanning bars across cells is a known
  // gap (see file header).
  const eventsByDay = useMemo(() => {
    const map = new Map<string, Commitment[]>();
    for (const e of events) {
      if (e.status === "CANCELLED") continue;
      const d = new Date(e.startAt);
      const key = dayKey(d);
      const list = map.get(key) ?? [];
      list.push(e);
      map.set(key, list);
    }
    // Sort each day's events by start time so the bars line up
    // chronologically.
    for (const list of map.values()) {
      list.sort((a, b) => a.startAt.localeCompare(b.startAt));
    }
    return map;
  }, [events]);

  // Header weekday labels — derived from `gridStart` so we follow the
  // active locale's first-day convention (we hard-coded ISO-Mon below;
  // if you switch `firstGridDay` to Sunday, this header follows).
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

      {/* 6 rows × 7 cols grid */}
      <div
        className="grid flex-1"
        style={{
          gridTemplateColumns: "repeat(7, 1fr)",
          gridTemplateRows: "repeat(6, minmax(0, 1fr))",
        }}
      >
        {cells.map((cellDate) => {
          const isToday = startOfDay(cellDate).getTime() === today.getTime();
          const inAnchorMonth =
            cellDate.getMonth() === anchorMonth &&
            cellDate.getFullYear() === anchorYear;
          const dayEvents = eventsByDay.get(dayKey(cellDate)) ?? [];
          const overflow = Math.max(0, dayEvents.length - MAX_VISIBLE);
          const visible = dayEvents.slice(0, MAX_VISIBLE);

          const handleCellClick = () => {
            const start = new Date(cellDate);
            start.setHours(DEFAULT_HOUR, 0, 0, 0);
            onCreateAt(start);
          };

          return (
            <div
              key={cellDate.toISOString()}
              className="relative flex min-h-0 flex-col gap-0.5 px-1 py-1 text-[11px]"
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
                {DAY_NUM_FMT.format(cellDate)}
              </div>
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
                      background: "var(--accent)",
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
      </div>
    </div>
  );
}

// ─── Helpers ─────────────────────────────────────────────────────────────

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
