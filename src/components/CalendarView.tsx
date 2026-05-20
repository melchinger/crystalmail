// Phase 1+3.4 calendar — three views over the same locally stored
// commitments:
//   - "Liste"  (Phase 1): Heute / Diese Woche / Später buckets.
//     Past events hidden by default behind a toggle.
//   - "Woche"  (Phase 3.4): Mon–Sun × hours grid.
//   - "Monat" (Phase 3.4): 6×7 day-cell grid.
//
// All three share the same `events` query and the same EventEditor
// modal. The grid views also support click-on-empty-area to open the
// editor in create mode pre-filled with that time.

import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type {
  AccountSummary,
  CalendarSubscription,
  CalendarSyncReport,
  Commitment,
  CommitmentAttendee,
  ComposeDraft,
  IcsImportReport,
} from "../types";
import { CalendarMonthView } from "./CalendarMonthView";
import { CalendarWeekView } from "./CalendarWeekView";
import { EventEditor } from "./EventEditor";
import { NegotiationStartDialog } from "./NegotiationStartDialog";

type EditorState =
  | { mode: "create" }
  | { mode: "create"; initialStartAt: string; initialEndAt: string }
  | { mode: "create"; initialAttendees: CommitmentAttendee[] }
  | {
      mode: "create";
      /** Mail-extraction-Seed: alle Felder optional, EventEditor füllt
       *  was vorhanden ist. Eigene Variante (statt einer aufgeblähten
       *  obigen) damit die Diskriminierungs-Branches simpel bleiben. */
      seed: {
        summary?: string;
        location?: string;
        description?: string;
        startAt?: string;
        endAt?: string;
      };
    }
  | { mode: "edit"; commitmentId: string }
  | null;

type ViewMode = "list" | "week" | "month";

const VIEW_MODE_STORAGE_KEY = "crystalmail.calendar.viewMode";

type Bucket = "past" | "today" | "thisWeek" | "later";

type Props = {
  /** Loaded once at app startup; threaded down so the EventEditor can
   *  resolve the sending account for "Einladung versenden" without an
   *  extra `list_accounts` round-trip. */
  accounts: AccountSummary[];
  /** Hand a draft up to App so Compose opens with our prefilled REQUEST
   *  ICS attachment. Same plumbing the IcsInvitePanel uses for REPLY. */
  onCompose: (draft: ComposeDraft) => void;
  /** Deep-Link aus ContactDetail: wenn gesetzt, beim Mount/Prop-Change
   *  den EventEditor für diese commitment-id im edit-mode öffnen.
   *  Wird nach Konsum via `onOpenedCommitment` zurückgesetzt — sonst
   *  würde ein zweiter Klick auf denselben Termin (gleicher Wert)
   *  keinen Effect-Run auslösen. */
  openCommitmentId?: string | null;
  onOpenedCommitment?: () => void;
  /** Deep-Link aus ContactDetail / Reader: öffnet EventEditor im
   *  create-mode mit Feldern aus dem Seed vorbefüllt. Zwei Pfade
   *  nutzen das:
   *  - "Termin planen mit Kontakt" — nur `attendees` gesetzt.
   *  - "Termin aus Mail" — Title/Zeit/Ort/Beschreibung gesetzt.
   *  Beide One-Shot: nach Konsum via `onSeededNewEvent` zurücksetzen. */
  seedNewEvent?: {
    attendees?: CommitmentAttendee[];
    summary?: string;
    location?: string;
    description?: string;
    startAt?: string;
    endAt?: string;
  } | null;
  onSeededNewEvent?: () => void;
};

export function CalendarView({
  accounts,
  onCompose,
  openCommitmentId,
  onOpenedCommitment,
  seedNewEvent,
  onSeededNewEvent,
}: Props) {
  const { t } = useTranslation();
  const [events, setEvents] = useState<Commitment[] | null>(null);
  // Subscriptions are loaded alongside events so the views can tint
  // each bar by its source calendar. We only need the colors at render
  // time, but we hold the full record so a later expansion (per-source
  // toggles, status icons, …) doesn't need a second round-trip.
  const [subscriptions, setSubscriptions] = useState<CalendarSubscription[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<EditorState>(null);
  const [showPast, setShowPast] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [syncStatus, setSyncStatus] = useState<string | null>(null);
  const [negotiateOpen, setNegotiateOpen] = useState(false);
  // Persist the last picked view across app restarts. Anchor date is
  // *not* persisted on purpose: opening the calendar on a stale week
  // from last week is more confusing than helpful.
  const [viewMode, setViewMode] = useState<ViewMode>(() => {
    const stored = localStorage.getItem(VIEW_MODE_STORAGE_KEY);
    return stored === "list" || stored === "week" || stored === "month"
      ? stored
      : "list";
  });
  useEffect(() => {
    try {
      localStorage.setItem(VIEW_MODE_STORAGE_KEY, viewMode);
    } catch {
      // localStorage can throw in private mode etc — non-fatal.
    }
  }, [viewMode]);
  // Anchor date for the grid views — drives prev/today/next
  // navigation. List view ignores this.
  const [anchorDate, setAnchorDate] = useState<Date>(() => new Date());

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
      const [rows, subs] = await Promise.all([
        invoke<Commitment[]>("cal_list_in_range", {
          from: from.toISOString(),
          to: to.toISOString(),
        }),
        invoke<CalendarSubscription[]>("cal_subs_list").catch(() =>
          // Subs are an enhancement, not a hard dep — the view should
          // still render if the store hasn't initialised yet.
          [] as CalendarSubscription[],
        ),
      ]);
      setEvents(rows);
      setSubscriptions(subs);
      setError(null);
    } catch (e) {
      setError(String(e));
      setEvents([]);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Deep-Link-Konsum: ein gesetztes `openCommitmentId` ⇒ EventEditor im
  // edit-mode für genau diese row öffnen. Nach dem State-Push den
  // parent-state via callback clearen, damit ein erneuter Klick auf
  // denselben Termin (identischer Wert) den Effect wieder auslöst —
  // ohne das clear wäre der zweite Klick ein No-Op weil die deps-Liste
  // gleich bleibt.
  useEffect(() => {
    if (!openCommitmentId) return;
    setEditor({ mode: "edit", commitmentId: openCommitmentId });
    onOpenedCommitment?.();
  }, [openCommitmentId, onOpenedCommitment]);

  // Gleiches Muster für "Termin planen mit Kontakt" + "Termin aus
  // Mail": öffnet create-mode mit vorbefüllten Feldern. App.tsx setzt
  // immer nur einen pending-State zur Zeit.
  useEffect(() => {
    if (!seedNewEvent) return;
    const hasAttendees =
      !!seedNewEvent.attendees && seedNewEvent.attendees.length > 0;
    const hasContent =
      !!seedNewEvent.summary ||
      !!seedNewEvent.location ||
      !!seedNewEvent.description ||
      !!seedNewEvent.startAt;
    if (!hasAttendees && !hasContent) return;
    if (hasContent) {
      // Mail-extraction-Pfad: nutze die generische `seed`-Variante,
      // die EventEditor unter den initialXxx-Props auspackt.
      setEditor({
        mode: "create",
        seed: {
          summary: seedNewEvent.summary,
          location: seedNewEvent.location,
          description: seedNewEvent.description,
          startAt: seedNewEvent.startAt,
          endAt: seedNewEvent.endAt,
        },
      });
    } else {
      // Reines Kontakt-Plan: nur attendees.
      setEditor({
        mode: "create",
        initialAttendees: seedNewEvent.attendees!,
      });
    }
    onSeededNewEvent?.();
  }, [seedNewEvent, onSeededNewEvent]);

  const handleImportFile = useCallback(async () => {
    try {
      const picked = await openDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "iCalendar", extensions: ["ics", "ICS"] }],
      });
      if (!picked) return;
      // openDialog returns `string | string[] | null` depending on options.
      // With `multiple: false` it's `string | null`, but the typing widens
      // — narrow defensively.
      const path = Array.isArray(picked) ? picked[0] : picked;
      if (!path) return;
      const report = await invoke<IcsImportReport>("cal_import_ics_file", {
        path,
      });
      setSyncStatus(
        t("calendar.list.importDone", {
          imported: report.imported,
          skipped: report.skipped,
          errors: report.errors.length,
        }),
      );
      await refresh();
      if (report.errors.length > 0) {
        // eslint-disable-next-line no-console
        console.warn("ics import errors:", report.errors);
      }
    } catch (e) {
      setSyncStatus(`✗ ${String(e)}`);
    }
  }, [refresh, t]);

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

  /** Indexed view of subscriptions, passed to the grid views so they
   *  can colorize bars per source calendar with O(1) lookup. */
  const subscriptionsById = useMemo(
    () =>
      new Map(
        subscriptions.map((s) => [s.id, { color: s.color, name: s.name }]),
      ),
    [subscriptions],
  );

  const shiftAnchor = useCallback((step: -1 | 0 | 1) => {
    setAnchorDate((prev) => {
      if (step === 0) return new Date();
      const next = new Date(prev);
      if (viewMode === "week") {
        next.setDate(next.getDate() + step * 7);
      } else if (viewMode === "month") {
        next.setMonth(next.getMonth() + step);
      }
      return next;
    });
  }, [viewMode]);

  const handleCreateAt = useCallback((start: Date) => {
    const end = new Date(start);
    end.setHours(end.getHours() + 1);
    setEditor({
      mode: "create",
      initialStartAt: start.toISOString(),
      initialEndAt: end.toISOString(),
    });
  }, []);

  const navLabel = useMemo(() => {
    if (viewMode === "week") return formatWeekRange(anchorDate);
    if (viewMode === "month") return formatMonthYear(anchorDate);
    return "";
  }, [viewMode, anchorDate]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header
        className="flex flex-wrap items-center gap-3 border-b px-4 py-3"
        style={{ borderColor: "var(--border-base)" }}
      >
        <h2 className="text-lg font-semibold" style={{ color: "var(--fg-base)" }}>
          {t("calendar.list.title")}
        </h2>

        {/* View toggle: List / Week / Month */}
        <div
          className="flex overflow-hidden rounded text-xs"
          style={{ border: "1px solid var(--border-soft)" }}
        >
          {(["list", "week", "month"] as const).map((mode) => (
            <button
              key={mode}
              type="button"
              onClick={() => setViewMode(mode)}
              className="px-2 py-1"
              style={{
                background:
                  viewMode === mode ? "var(--accent)" : "var(--bg-soft)",
                color: viewMode === mode ? "#fff" : "var(--fg-base)",
              }}
            >
              {t(`calendar.view.${mode}`)}
            </button>
          ))}
        </div>

        {/* Navigation: only meaningful for grid views */}
        {viewMode !== "list" && (
          <div className="flex items-center gap-1 text-xs">
            <button
              type="button"
              onClick={() => shiftAnchor(-1)}
              className="rounded px-2 py-1"
              style={{
                background: "var(--bg-soft)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-soft)",
              }}
              title={t("calendar.nav.prev")}
            >
              ‹
            </button>
            <button
              type="button"
              onClick={() => shiftAnchor(0)}
              className="rounded px-2 py-1"
              style={{
                background: "var(--bg-soft)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-soft)",
              }}
            >
              {t("calendar.nav.today")}
            </button>
            <button
              type="button"
              onClick={() => shiftAnchor(1)}
              className="rounded px-2 py-1"
              style={{
                background: "var(--bg-soft)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-soft)",
              }}
              title={t("calendar.nav.next")}
            >
              ›
            </button>
            <span
              className="ml-2 text-xs"
              style={{ color: "var(--fg-muted)" }}
            >
              {navLabel}
            </span>
          </div>
        )}

        {viewMode === "list" && (
          <span
            className="text-xs"
            style={{ color: "var(--fg-subtle)" }}
          >
            {events?.length ?? 0} {t("calendar.list.eventsCount")}
          </span>
        )}

        <div className="ml-auto flex flex-wrap items-center gap-2">
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
          <button
            type="button"
            onClick={() => void handleImportFile()}
            className="rounded px-3 py-1 text-xs"
            style={{
              background: "var(--bg-soft)",
              color: "var(--fg-base)",
              border: "1px solid var(--border-soft)",
            }}
            title={t("calendar.list.importTitle")}
          >
            {t("calendar.list.import")}
          </button>
          {viewMode === "list" && (
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
          )}
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

      <div className="min-h-0 flex-1 overflow-y-auto">
        {error && (
          <div
            className="m-3 rounded border px-3 py-2 text-sm"
            style={{
              borderColor: "var(--border-soft)",
              color: "var(--fg-error, #c00)",
            }}
          >
            {error}
          </div>
        )}

        {events === null ? (
          <div className="px-4 py-3 text-sm" style={{ color: "var(--fg-muted)" }}>
            {t("calendar.list.loading")}
          </div>
        ) : viewMode === "list" ? (
          <div className="flex flex-col gap-4 px-4 py-3">
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
        ) : viewMode === "week" ? (
          <CalendarWeekView
            anchorDate={anchorDate}
            events={events}
            subscriptionsById={subscriptionsById}
            onPickEvent={(id) =>
              setEditor({ mode: "edit", commitmentId: id })
            }
            onCreateAt={handleCreateAt}
          />
        ) : (
          <CalendarMonthView
            anchorDate={anchorDate}
            events={events}
            subscriptionsById={subscriptionsById}
            onPickEvent={(id) =>
              setEditor({ mode: "edit", commitmentId: id })
            }
            onCreateAt={handleCreateAt}
          />
        )}
      </div>

      {editor && (
        <EventEditor
          mode={editor.mode}
          commitmentId={editor.mode === "edit" ? editor.commitmentId : null}
          initialStartAt={
            editor.mode === "create" && "initialStartAt" in editor
              ? editor.initialStartAt
              : editor.mode === "create" && "seed" in editor
                ? editor.seed.startAt ?? null
                : null
          }
          initialEndAt={
            editor.mode === "create" && "initialEndAt" in editor
              ? editor.initialEndAt
              : editor.mode === "create" && "seed" in editor
                ? editor.seed.endAt ?? null
                : null
          }
          initialAttendees={
            editor.mode === "create" && "initialAttendees" in editor
              ? editor.initialAttendees
              : null
          }
          initialSummary={
            editor.mode === "create" && "seed" in editor
              ? editor.seed.summary ?? null
              : null
          }
          initialLocation={
            editor.mode === "create" && "seed" in editor
              ? editor.seed.location ?? null
              : null
          }
          initialDescription={
            editor.mode === "create" && "seed" in editor
              ? editor.seed.description ?? null
              : null
          }
          accounts={accounts}
          onCompose={onCompose}
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

/** Header label for the week view: "Mo, 5. – So, 11. Mai 2026" or
 *  cross-month "29. Apr – 5. Mai 2026". Locale-driven via Intl. */
function formatWeekRange(anchor: Date): string {
  const start = startOfDay(anchor);
  const dow = start.getDay();
  const back = (dow + 6) % 7;
  start.setDate(start.getDate() - back);
  const end = new Date(start);
  end.setDate(end.getDate() + 6);
  const sameMonth =
    start.getMonth() === end.getMonth() &&
    start.getFullYear() === end.getFullYear();
  const dateFmt = new Intl.DateTimeFormat(undefined, {
    day: "2-digit",
    month: "short",
  });
  const dateYearFmt = new Intl.DateTimeFormat(undefined, {
    day: "2-digit",
    month: "short",
    year: "numeric",
  });
  if (sameMonth) {
    return `${start.getDate()}. – ${dateYearFmt.format(end)}`;
  }
  return `${dateFmt.format(start)} – ${dateYearFmt.format(end)}`;
}

/** Header label for the month view: "Mai 2026". */
function formatMonthYear(anchor: Date): string {
  const fmt = new Intl.DateTimeFormat(undefined, {
    month: "long",
    year: "numeric",
  });
  return fmt.format(anchor);
}

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
