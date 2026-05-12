// Phase 1 calendar editor — modal dialog for creating/editing a stored
// commitment. Datetime inputs are HTML5 `datetime-local` (timezone-naive
// in the input), converted to RFC 3339 with the system's local offset on
// save. The resulting backend timestamps embed the offset so the row
// round-trips back to the same wall-clock display next to the editor.
//
// What's intentionally absent in Phase 1: recurrence, multiple attendees
// editing UI, organizer picker, attachments. Those land in Phase 3+ when
// negotiation makes them meaningful. The "Anhang"-button is replaced with
// an "Export als ICS"-action so the user can hand a stored event to a
// non-CrystalMail peer manually.

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import type { Commitment, CommitmentDraft, ExportedIcs } from "../types";
import { isAllDayEvent } from "../utils/calendarEvent";

type Props = {
  mode: "create" | "edit";
  commitmentId: string | null;
  onClose: () => void;
  onSaved: (commitment: Commitment) => void;
  onDeleted: () => void;
  /** Pre-fill the start/end times in `create` mode. Used by the
   *  week/month grid views: clicking an empty area opens the editor
   *  with the clicked time already filled in. RFC 3339 with offset.
   *  Ignored in `edit` mode. */
  initialStartAt?: string | null;
  initialEndAt?: string | null;
};

type FormState = {
  summary: string;
  location: string;
  description: string;
  /** Toggle: when true, start/end are interpreted as plain dates
   *  (`YYYY-MM-DD`); when false they're datetime-local
   *  (`YYYY-MM-DDTHH:MM`). The input controls swap based on this. */
  allDay: boolean;
  /** Form value of the start input. Format depends on `allDay`. */
  startLocal: string;
  /** Form value of the end input. For `allDay`, this is the **inclusive
   *  end date** — the day the event is last visible. Conversion to the
   *  RFC-5545-style exclusive end (next-day midnight) happens at save
   *  time so the user-facing semantics match Outlook / Google
   *  ("23. bis 23." = one day). */
  endLocal: string;
};

const EMPTY: FormState = {
  summary: "",
  location: "",
  description: "",
  allDay: false,
  startLocal: defaultStartLocal(),
  endLocal: defaultEndLocal(),
};

export function EventEditor({
  mode,
  commitmentId,
  onClose,
  onSaved,
  onDeleted,
  initialStartAt,
  initialEndAt,
}: Props) {
  const { t } = useTranslation();
  // In create mode with prefilled times: seed the form with those.
  // Otherwise fall back to the "next half hour, +1h" defaults that
  // the empty constant carries.
  const [form, setForm] = useState<FormState>(() => {
    if (mode === "create" && initialStartAt) {
      return {
        ...EMPTY,
        startLocal: rfc3339ToLocalDateTime(initialStartAt),
        endLocal: initialEndAt
          ? rfc3339ToLocalDateTime(initialEndAt)
          : rfc3339ToLocalDateTime(addHour(initialStartAt)),
      };
    }
    return EMPTY;
  });
  const [loaded, setLoaded] = useState<Commitment | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Edit-mode: load existing commitment.
  useEffect(() => {
    if (mode !== "edit" || !commitmentId) return;
    let cancelled = false;
    (async () => {
      try {
        const c = await invoke<Commitment | null>("cal_get", {
          id: commitmentId,
        });
        if (cancelled) return;
        if (!c) {
          setError(t("calendar.editor.notFound"));
          return;
        }
        setLoaded(c);
        const allDay = isAllDayEvent(c);
        setForm({
          summary: c.summary ?? "",
          location: c.location ?? "",
          description: c.description ?? "",
          allDay,
          startLocal: allDay
            ? rfc3339ToLocalDate(c.startAt)
            : rfc3339ToLocalDateTime(c.startAt),
          // All-day events store an *exclusive* end (next midnight) —
          // step back one day for the inclusive UI form. A 1-day event
          // (start = 23.04 00:00, end = 24.04 00:00) renders as 23.→23.
          endLocal: allDay
            ? rfc3339ToLocalDate(addDays(c.endAt, -1))
            : rfc3339ToLocalDateTime(c.endAt),
        });
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [mode, commitmentId, t]);

  const submit = async () => {
    if (busy) return;
    if (loaded?.subscriptionId) {
      // Belt-and-braces: the save button is hidden in this branch, but
      // a stray Enter on a form field would otherwise still fire submit.
      return;
    }
    if (!form.startLocal || !form.endLocal) {
      setError(t("calendar.editor.missingTime"));
      return;
    }
    if (form.endLocal < form.startLocal) {
      setError(t("calendar.editor.endBeforeStart"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      // Two conversion paths share the rest of the draft. All-day:
      // start = picked-date 00:00 local, end = (picked-end + 1d) 00:00
      // local — the +1d turns the user's inclusive end date into the
      // RFC-5545-exclusive next-midnight the storage layer expects.
      const startAt = form.allDay
        ? localDateToRfc3339Midnight(form.startLocal)
        : localDateTimeToRfc3339(form.startLocal);
      const endAt = form.allDay
        ? localDateToRfc3339Midnight(addDaysToDateString(form.endLocal, 1))
        : localDateTimeToRfc3339(form.endLocal);
      const draft: CommitmentDraft = {
        summary: form.summary.trim() || null,
        location: form.location.trim() || null,
        description: form.description.trim() || null,
        startAt,
        endAt,
        originalTzid: loaded?.originalTzid ?? null,
        organizer: loaded?.organizer ?? null,
        attendees: loaded?.attendees ?? [],
      };
      const saved =
        mode === "edit" && commitmentId
          ? await invoke<Commitment>("cal_update", {
              id: commitmentId,
              draft,
            })
          : await invoke<Commitment>("cal_create", { draft });
      onSaved(saved);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async () => {
    if (!commitmentId || busy) return;
    if (!window.confirm(t("calendar.editor.confirmDelete"))) return;
    setBusy(true);
    try {
      // Soft-cancel: backend bumps SEQUENCE and sets STATUS:CANCELLED.
      // The row stays in the table (filtered out of the list view) so a
      // future Phase-2 IMAP-publish can still emit the cancellation
      // envelope with the right counter.
      await invoke<Commitment>("cal_delete", { id: commitmentId });
      onDeleted();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const handleCancelSeries = async () => {
    if (!loaded?.seriesUid || busy) return;
    if (!window.confirm(t("calendar.editor.confirmCancelSeries"))) return;
    setBusy(true);
    try {
      // Hard-deletes all occurrences sharing this series_uid (no IMAP
      // envelope — series rows are excluded from publish, so nothing
      // remote to cancel).
      await invoke<number>("cal_cancel_series", {
        seriesUid: loaded.seriesUid,
      });
      onDeleted();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  const handleExport = async () => {
    if (!commitmentId) return;
    try {
      // Get a default filename first so the dialog can pre-fill it.
      const preview = await invoke<ExportedIcs>("cal_export_to_ics", {
        id: commitmentId,
      });
      const dest = await saveDialog({
        title: t("calendar.editor.exportTitle"),
        defaultPath: preview.filename,
        filters: [{ name: "iCalendar", extensions: ["ics"] }],
      });
      if (!dest) return;
      // Second call writes the file at the chosen path. Two calls because
      // the backend re-renders cheaply and the saveDialog round-trip is
      // user-driven; we don't want to block on the user's choice while
      // holding the rendered content somewhere.
      const exported = await invoke<ExportedIcs>("cal_export_to_ics", {
        id: commitmentId,
        destination: dest,
      });
      if (exported.writtenTo) {
        alert(
          t("calendar.editor.exportSavedTo", { path: exported.writtenTo }),
        );
      }
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.45)" }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="w-full max-w-md rounded-md border p-4 shadow-lg"
        style={{
          background: "var(--bg-elevated, var(--bg-base))",
          borderColor: "var(--border-base)",
        }}
      >
        <h3
          className="mb-3 text-base font-semibold"
          style={{ color: "var(--fg-base)" }}
        >
          {mode === "edit"
            ? t("calendar.editor.editTitle")
            : t("calendar.editor.createTitle")}
        </h3>

        {loaded?.seriesUid && (
          <div
            className="mb-3 rounded border px-2 py-1.5 text-xs"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-soft)",
              color: "var(--fg-muted)",
            }}
          >
            {t("calendar.editor.seriesHint")}
          </div>
        )}

        {loaded?.subscriptionId && (
          <div
            className="mb-3 rounded border px-2 py-1.5 text-xs"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-soft)",
              color: "var(--fg-muted)",
            }}
          >
            {t("calendar.editor.subscriptionHint")}
          </div>
        )}

        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <Field label={t("calendar.editor.summary")}>
            <input
              type="text"
              value={form.summary}
              onChange={(e) =>
                setForm((f) => ({ ...f, summary: e.target.value }))
              }
              className="w-full rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
              autoFocus
            />
          </Field>

          <label className="flex items-center gap-2 text-xs" style={{ color: "var(--fg-muted)" }}>
            <input
              type="checkbox"
              checked={form.allDay}
              onChange={(e) => {
                const allDay = e.target.checked;
                setForm((f) => {
                  // Preserve the calendar day across the toggle. Off→on:
                  // drop the time. On→off: re-attach a sensible default
                  // (09:00 start, 10:00 end) so the user doesn't have
                  // to dial both back from midnight.
                  if (allDay) {
                    return {
                      ...f,
                      allDay: true,
                      startLocal: f.startLocal.slice(0, 10),
                      endLocal: f.endLocal.slice(0, 10),
                    };
                  }
                  const startDate = f.startLocal.slice(0, 10);
                  const endDate = f.endLocal.slice(0, 10);
                  return {
                    ...f,
                    allDay: false,
                    startLocal: `${startDate}T09:00`,
                    endLocal: `${endDate}T10:00`,
                  };
                });
              }}
            />
            <span>{t("calendar.editor.allDay")}</span>
          </label>

          <div className="flex gap-2">
            <Field label={t("calendar.editor.start")} className="flex-1">
              <input
                type={form.allDay ? "date" : "datetime-local"}
                value={form.startLocal}
                onChange={(e) =>
                  setForm((f) => ({ ...f, startLocal: e.target.value }))
                }
                className="w-full rounded border px-2 py-1 text-sm"
                style={{
                  borderColor: "var(--border-soft)",
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                }}
                required
              />
            </Field>
            <Field label={t("calendar.editor.end")} className="flex-1">
              <input
                type={form.allDay ? "date" : "datetime-local"}
                value={form.endLocal}
                onChange={(e) =>
                  setForm((f) => ({ ...f, endLocal: e.target.value }))
                }
                className="w-full rounded border px-2 py-1 text-sm"
                style={{
                  borderColor: "var(--border-soft)",
                  background: "var(--bg-base)",
                  color: "var(--fg-base)",
                }}
                required
              />
            </Field>
          </div>

          <Field label={t("calendar.editor.location")}>
            <input
              type="text"
              value={form.location}
              onChange={(e) =>
                setForm((f) => ({ ...f, location: e.target.value }))
              }
              className="w-full rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
            />
          </Field>

          <Field label={t("calendar.editor.description")}>
            <textarea
              rows={6}
              value={form.description}
              onChange={(e) =>
                setForm((f) => ({ ...f, description: e.target.value }))
              }
              className="w-full rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
                resize: "vertical",
                fontFamily: "inherit",
              }}
            />
          </Field>

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

          <div className="mt-1 flex flex-wrap items-center justify-end gap-2">
            {mode === "edit" && (
              <>
                <button
                  type="button"
                  onClick={() => void handleExport()}
                  className="mr-auto rounded px-3 py-1 text-xs"
                  style={{
                    background: "var(--bg-soft)",
                    color: "var(--fg-base)",
                    border: "1px solid var(--border-soft)",
                  }}
                >
                  {t("calendar.editor.exportIcs")}
                </button>
                {!loaded?.subscriptionId && (
                  <button
                    type="button"
                    onClick={() => void handleDelete()}
                    className="rounded px-3 py-1 text-xs"
                    style={{
                      background: "var(--bg-soft)",
                      color: "var(--fg-error, #c00)",
                      border: "1px solid var(--border-soft)",
                    }}
                    disabled={busy}
                  >
                    {loaded?.seriesUid
                      ? t("calendar.editor.deleteOccurrence")
                      : t("calendar.editor.delete")}
                  </button>
                )}
                {loaded?.seriesUid && (
                  <button
                    type="button"
                    onClick={() => void handleCancelSeries()}
                    className="rounded px-3 py-1 text-xs"
                    style={{
                      background: "var(--bg-soft)",
                      color: "var(--fg-error, #c00)",
                      border: "1px solid var(--border-soft)",
                    }}
                    disabled={busy}
                  >
                    {t("calendar.editor.cancelSeries")}
                  </button>
                )}
              </>
            )}
            <button
              type="button"
              onClick={onClose}
              className="rounded px-3 py-1 text-xs"
              style={{
                background: "var(--bg-soft)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-soft)",
              }}
              disabled={busy}
            >
              {t("calendar.editor.cancel")}
            </button>
            <button
              type="submit"
              disabled={busy}
              className="rounded px-3 py-1 text-xs font-medium"
              style={{
                background: "var(--accent)",
                color: "#fff",
                border: "1px solid var(--border-soft)",
              }}
            >
              {busy
                ? t("calendar.editor.saving")
                : t("calendar.editor.save")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

function Field({
  label,
  className,
  children,
}: {
  label: string;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <label className={`flex flex-col gap-1 ${className ?? ""}`}>
      <span
        className="text-xs"
        style={{ color: "var(--fg-muted)" }}
      >
        {label}
      </span>
      {children}
    </label>
  );
}

// ─── Datetime conversion helpers ─────────────────────────────────────────

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** Convert an HTML5 `datetime-local` input value (timezone-naive) into an
 *  RFC 3339 timestamp with the system's local offset applied. The naive
 *  input is interpreted as wall-clock time in the user's TZ. */
function localDateTimeToRfc3339(local: string): string {
  // The input may be "YYYY-MM-DDTHH:MM" or "YYYY-MM-DDTHH:MM:SS"; JS Date
  // parses both as local.
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

/** Inverse: take an RFC 3339 timestamp and produce a `datetime-local`
 *  string in the user's TZ. Strips the offset — the input element doesn't
 *  show one. */
function rfc3339ToLocalDateTime(rfc: string): string {
  const d = new Date(rfc);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}T${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

/** Date-only twin of `rfc3339ToLocalDateTime`. Format expected by an
 *  `<input type="date">`: `YYYY-MM-DD`. */
function rfc3339ToLocalDate(rfc: string): string {
  const d = new Date(rfc);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/** Convert a date-input string (`YYYY-MM-DD`) to an RFC 3339 timestamp
 *  at local midnight with the system's TZ offset. Mirror of
 *  `localDateTimeToRfc3339` for the all-day case. */
function localDateToRfc3339Midnight(localDate: string): string {
  return localDateTimeToRfc3339(`${localDate}T00:00`);
}

/** Shift a `YYYY-MM-DD` string by `days` calendar days. Negative values
 *  step back. Used to translate between the user-facing inclusive end
 *  date and the RFC-5545-style exclusive end. */
function addDaysToDateString(localDate: string, days: number): string {
  const d = new Date(`${localDate}T00:00:00`);
  d.setDate(d.getDate() + days);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/** Shift an RFC 3339 timestamp by `days` calendar days, preserving the
 *  offset/time-of-day. Used by the load path: stored end is exclusive,
 *  UI form wants inclusive. */
function addDays(rfc: string, days: number): string {
  const d = new Date(rfc);
  d.setDate(d.getDate() + days);
  return d.toISOString();
}

/** Default start = next-rounded-half-hour today (or tomorrow if past 23:30). */
/** RFC 3339 → RFC 3339, +1 hour. Used to derive a default end time
 *  when only `initialStartAt` is supplied. */
function addHour(iso: string): string {
  const d = new Date(iso);
  d.setHours(d.getHours() + 1);
  return d.toISOString();
}

function defaultStartLocal(): string {
  const d = new Date();
  d.setMinutes(d.getMinutes() + 30);
  d.setMinutes(d.getMinutes() < 30 ? 30 : 0, 0, 0);
  if (d.getHours() === 0 && d.getMinutes() === 0) d.setDate(d.getDate() + 1);
  return rfc3339ToLocalDateTime(d.toISOString());
}

function defaultEndLocal(): string {
  const start = new Date(defaultStartLocal());
  start.setHours(start.getHours() + 1);
  return rfc3339ToLocalDateTime(start.toISOString());
}
