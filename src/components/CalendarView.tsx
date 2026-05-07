// Phase 1 calendar — list view of locally stored commitments grouped into
// "Heute / Diese Woche / Später", plus a "Neuer Termin"-button that opens
// the EventEditor in create mode. Click a row to edit.
//
// No grid view in Phase 1 (deliberate — see project memory). Past events
// are hidden by default; a toggle to show them lives in the header.

import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { CalendarSyncReport, Commitment } from "../types";
import { EventEditor } from "./EventEditor";
import { NegotiationStartDialog } from "./NegotiationStartDialog";

type EditorState =
  | { mode: "create" }
  | { mode: "edit"; commitmentId: string }
  | null;

type Bucket = "past" | "today" | "thisWeek" | "later";

export function CalendarView() {
  const { t } = useTranslation();
  const [events, setEvents] = useState<Commitment[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<EditorState>(null);
  const [showPast, setShowPast] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [syncStatus, setSyncStatus] = useState<string | null>(null);
  const [negotiateOpen, setNegotiateOpen] = useState(false);

  const refresh = useCallback(async () => {
    try {
      // Range: 12 months back (so a "Past"-toggle has something to show)
      // through 12 months ahead. The list is small enough that we don't
      // need pagination — even a heavy user has well under 10k events
      // in two years, and the index on `start_at` makes the query fast.
      const now = new Date();
      const from = new Date(now);
      from.setMonth(from.getMonth() - 12);
      const to = new Date(now);
      to.setMonth(to.getMonth() + 12);
      const rows = await invoke<Commitment[]>("cal_list_in_range", {
        from: from.toISOString(),
        to: to.toISOString(),
      });
      setEvents(rows);
      setError(null);
    } catch (e) {
      setError(String(e));
      setEvents([]);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const handleSync = useCallback(async () => {
    if (syncing) return;
    setSyncing(true);
    setSyncStatus(null);
    try {
      const report = await invoke<CalendarSyncReport>("cal_sync_imap");
      const errCount = report.errors.length;
      setSyncStatus(
        t("calendar.list.syncDone", {
          imported: report.imported,
          published: report.published,
          remoteDeleted: report.remoteDeleted,
          errors: errCount,
        }),
      );
      // Refresh the list after sync — imports may have added new rows.
      await refresh();
      // Surface per-event errors in the dev console; the user gets the
      // count in the status line above.
      if (errCount > 0) {
        // eslint-disable-next-line no-console
        console.warn("calendar sync errors:", report.errors);
      }
    } catch (e) {
      setSyncStatus(`✗ ${String(e)}`);
    } finally {
      setSyncing(false);
    }
  }, [refresh, syncing, t]);

  const grouped = useMemo(() => groupByBucket(events ?? []), [events]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header
        className="flex items-center gap-3 border-b px-4 py-3"
        style={{ borderColor: "var(--border-base)" }}
      >
        <h2 className="text-lg font-semibold" style={{ color: "var(--fg-base)" }}>
          {t("calendar.list.title")}
        </h2>
        <span
          className="text-xs"
          style={{ color: "var(--fg-subtle)" }}
        >
          {events?.length ?? 0} {t("calendar.list.eventsCount")}
        </span>
        <div className="ml-auto flex items-center gap-2">
          {syncStatus && (
            <span
              className="text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              {syncStatus}
            </span>
          )}
          <button
            type="button"
            onClick={() => void handleSync()}
            disabled={syncing}
            className="rounded px-3 py-1 text-xs"
            style={{
              background: "var(--bg-soft)",
              color: "var(--fg-base)",
              border: "1px solid var(--border-soft)",
            }}
            title={t("calendar.list.syncTitle")}
          >
            {syncing ? t("calendar.list.syncing") : t("calendar.list.sync")}
          </button>
          <label
            className="flex items-center gap-1 text-xs"
            style={{ color: "var(--fg-muted)" }}
          >
            <input
              type="checkbox"
              checked={showPast}
              onChange={(e) => setShowPast(e.target.checked)}
            />
            {t("calendar.list.showPast")}
          </label>
          <button
            type="button"
            onClick={() => setNegotiateOpen(true)}
            className="rounded px-3 py-1 text-xs"
            style={{
              background: "var(--bg-soft)",
              color: "var(--fg-base)",
              border: "1px solid var(--border-soft)",
            }}
            title={t("calendar.list.negotiateTitle")}
          >
            {t("calendar.list.negotiate")}
          </button>
          <button
            type="button"
            onClick={() => setEditor({ mode: "create" })}
            className="rounded px-3 py-1 text-xs font-medium"
            style={{
              background: "var(--accent)",
              color: "#fff",
              border: "1px solid var(--border-soft)",
            }}
          >
            + {t("calendar.list.newEvent")}
          </button>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        {error && (
          <div
            className="mb-3 rounded border px-3 py-2 text-sm"
            style={{
              borderColor: "var(--border-soft)",
              color: "var(--fg-error, #c00)",
            }}
          >
            {error}
          </div>
        )}

        {events === null ? (
          <div className="text-sm" style={{ color: "var(--fg-muted)" }}>
            {t("calendar.list.loading")}
          </div>
        ) : (
          <div className="flex flex-col gap-4">
            {showPast && grouped.past.length > 0 && (
              <Section
                title={t("calendar.list.bucket.past")}
                events={grouped.past}
                onPick={(id) => setEditor({ mode: "edit", commitmentId: id })}
                muted
              />
            )}
            <Section
              title={t("calendar.list.bucket.today")}
              events={grouped.today}
              onPick={(id) => setEditor({ mode: "edit", commitmentId: id })}
              emptyText={t("calendar.list.bucket.todayEmpty")}
            />
            <Section
              title={t("calendar.list.bucket.thisWeek")}
              events={grouped.thisWeek}
              onPick={(id) => setEditor({ mode: "edit", commitmentId: id })}
            />
            <Section
              title={t("calendar.list.bucket.later")}
              events={grouped.later}
              onPick={(id) => setEditor({ mode: "edit", commitmentId: id })}
            />
          </div>
        )}
      </div>

      {editor && (
        <EventEditor
          mode={editor.mode}
          commitmentId={editor.mode === "edit" ? editor.commitmentId : null}
          onClose={() => setEditor(null)}
          onSaved={() => {
            setEditor(null);
            void refresh();
          }}
          onDeleted={() => {
            setEditor(null);
            void refresh();
          }}
        />
      )}
      {negotiateOpen && (
        <NegotiationStartDialog
          onClose={() => setNegotiateOpen(false)}
          onSent={() => {
            setNegotiateOpen(false);
            // No list-view yet; the panel only renders inline in the
            // Reader when the counterparty's response arrives. We
            // surface a quick toast-equivalent via the sync-status
            // line so the user gets feedback that the request went
            // out.
            setSyncStatus(t("calendar.list.negotiateSent"));
          }}
        />
      )}
    </div>
  );
}

function Section({
  title,
  events,
  onPick,
  emptyText,
  muted,
}: {
  title: string;
  events: Commitment[];
  onPick: (id: string) => void;
  emptyText?: string;
  muted?: boolean;
}) {
  if (events.length === 0 && !emptyText) return null;
  return (
    <section>
      <h3
        className="mb-1 text-xs font-semibold uppercase tracking-wide"
        style={{ color: "var(--fg-subtle)" }}
      >
        {title}
      </h3>
      {events.length === 0 ? (
        <p className="text-sm" style={{ color: "var(--fg-subtle)" }}>
          {emptyText}
        </p>
      ) : (
        <ul
          className="flex flex-col gap-1"
          style={{ opacity: muted ? 0.6 : 1 }}
        >
          {events.map((e) => (
            <li key={e.id}>
              <EventRow event={e} onPick={() => onPick(e.id)} />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function EventRow({ event, onPick }: { event: Commitment; onPick: () => void }) {
  return (
    <button
      type="button"
      onClick={onPick}
      className="flex w-full items-baseline gap-3 rounded border px-3 py-2 text-left text-sm transition hover:opacity-80"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-base)",
      }}
    >
      <span
        className="shrink-0 tabular-nums"
        style={{ color: "var(--fg-muted)", minWidth: "10rem" }}
      >
        {formatRange(event.startAt, event.endAt)}
      </span>
      <span className="flex-1 truncate" style={{ color: "var(--fg-base)" }}>
        {event.summary || "—"}
      </span>
      {event.location && (
        <span
          className="shrink-0 truncate text-xs"
          style={{ color: "var(--fg-subtle)", maxWidth: "12rem" }}
        >
          {event.location}
        </span>
      )}
    </button>
  );
}

// ─── Grouping ────────────────────────────────────────────────────────────

function groupByBucket(events: Commitment[]): Record<Bucket, Commitment[]> {
  const out: Record<Bucket, Commitment[]> = {
    past: [],
    today: [],
    thisWeek: [],
    later: [],
  };
  const now = new Date();
  const todayStart = startOfDay(now);
  const tomorrowStart = new Date(todayStart);
  tomorrowStart.setDate(tomorrowStart.getDate() + 1);
  const weekEnd = endOfWeek(now);

  for (const ev of events) {
    const start = new Date(ev.startAt);
    if (start < todayStart) {
      out.past.push(ev);
    } else if (start < tomorrowStart) {
      out.today.push(ev);
    } else if (start < weekEnd) {
      out.thisWeek.push(ev);
    } else {
      out.later.push(ev);
    }
  }
  // Past: most recent first; everything else: chronological.
  out.past.sort((a, b) => b.startAt.localeCompare(a.startAt));
  return out;
}

function startOfDay(d: Date): Date {
  const out = new Date(d);
  out.setHours(0, 0, 0, 0);
  return out;
}

/** End of the calendar week (Sunday 23:59:59 in ISO 8601: week ends Sun).
 *  We use Mon-as-week-start; the bucket boundary is next Monday 0:00. */
function endOfWeek(d: Date): Date {
  const out = startOfDay(d);
  // JS Date.getDay(): 0 = Sunday, 1 = Monday, ..., 6 = Saturday.
  const dow = out.getDay();
  const daysUntilNextMonday = ((1 - dow + 7) % 7) || 7;
  out.setDate(out.getDate() + daysUntilNextMonday);
  return out;
}

// ─── Date formatting ─────────────────────────────────────────────────────

const FMT_DATE = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  day: "2-digit",
  month: "short",
});
const FMT_TIME = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
});

function formatRange(startAt: string, endAt: string): string {
  const start = new Date(startAt);
  const end = new Date(endAt);
  const sameDay =
    start.getFullYear() === end.getFullYear() &&
    start.getMonth() === end.getMonth() &&
    start.getDate() === end.getDate();
  if (sameDay) {
    return `${FMT_DATE.format(start)} ${FMT_TIME.format(start)}–${FMT_TIME.format(end)}`;
  }
  return `${FMT_DATE.format(start)} ${FMT_TIME.format(start)} – ${FMT_DATE.format(end)} ${FMT_TIME.format(end)}`;
}
