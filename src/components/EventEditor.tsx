// Phase 1 calendar editor — modal dialog for creating/editing a stored
// commitment. Datetime inputs are HTML5 `datetime-local` (timezone-naive
// in the input), converted to RFC 3339 with the system's local offset on
// save. The resulting backend timestamps embed the offset so the row
// round-trips back to the same wall-clock display next to the editor.
//
// Attendees: editable list of email (+ optional display name). The
// organizer is *not* a UI field — it's stamped to the sending account's
// address when "Einladung versenden" is clicked. Recurrence and a
// freeform organizer-picker remain out of scope (Phase 3+).

import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import type {
  AccountSummary,
  AddressCompletion,
  Commitment,
  CommitmentAttendee,
  CommitmentDraft,
  ComposeAttachment,
  ComposeDraft,
  ExportedIcs,
  InvitationRequestDraft,
} from "../types";
import { AddressAutocomplete } from "./AddressAutocomplete";
import {
  isAllDayEvent,
  localDateTimeToRfc3339,
} from "../utils/calendarEvent";

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
  /** Pre-fill the attendee list in `create` mode. Used by the
   *  ContactDetail "Termin planen"-Button: opens the editor with the
   *  contact already on the invite list. Ignored in `edit` mode. */
  initialAttendees?: CommitmentAttendee[] | null;
  /** Pre-fill summary/location/description text-fields in `create`
   *  mode. Used by the "Termin aus Mail" pi-extraction path so the
   *  EventEditor opens with the LLM-derived draft ready for review.
   *  All ignored in `edit` mode (loaded commitment wins). */
  initialSummary?: string | null;
  initialLocation?: string | null;
  initialDescription?: string | null;
  /** Available accounts — drives the sender picker for "Einladung
   *  versenden". Required for the invite path; absent means the
   *  button stays disabled. */
  accounts: AccountSummary[];
  /** Open Compose with the prepared REQUEST draft. Threaded down
   *  from App via CalendarView. */
  onCompose: (draft: ComposeDraft) => void;
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
  /** Editable attendee list. PARTSTAT is preserved across edits but the
   *  UI shows it read-only (you can't fake a reply for someone else).
   *  Empty list = no invitation flow available. */
  attendees: CommitmentAttendee[];
};

const EMPTY: FormState = {
  summary: "",
  location: "",
  description: "",
  allDay: false,
  startLocal: defaultStartLocal(),
  endLocal: defaultEndLocal(),
  attendees: [],
};

/** Loose email check — enough to keep typo-only entries out of the
 *  outgoing iMIP. The backend treats whatever lands as a literal mailto:
 *  URI, so we'd rather reject `bob` than silently send `mailto:bob`. */
function looksLikeEmail(s: string): boolean {
  const trimmed = s.trim();
  if (!trimmed) return false;
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(trimmed);
}

export function EventEditor({
  mode,
  commitmentId,
  onClose,
  onSaved,
  onDeleted,
  initialStartAt,
  initialEndAt,
  initialAttendees,
  initialSummary,
  initialLocation,
  initialDescription,
  accounts,
  onCompose,
}: Props) {
  const { t } = useTranslation();
  // Sender for "Einladung versenden". Defaults to the first account
  // (matches the rest of the UI's single-account-bias). Multi-account
  // users get a dropdown next to the button.
  const [senderAccountId, setSenderAccountId] = useState<string>(
    () => accounts[0]?.id ?? "",
  );
  useEffect(() => {
    // Re-seed if accounts arrive after first render (App loads them
    // async) or change underneath us.
    if (!senderAccountId && accounts[0]) setSenderAccountId(accounts[0].id);
  }, [accounts, senderAccountId]);
  // Inline-add row for new attendees. Kept outside FormState because
  // it's transient UI state, not persisted.
  const [newAttendeeEmail, setNewAttendeeEmail] = useState("");
  const [newAttendeeName, setNewAttendeeName] = useState("");
  // In create mode with prefilled times: seed the form with those.
  // Otherwise fall back to the "next half hour, +1h" defaults that
  // the empty constant carries.
  const [form, setForm] = useState<FormState>(() => {
    // Both initial-* props only apply in create-mode; in edit-mode the
    // form is hydrated from the loaded commitment by the effect below.
    if (mode !== "create") return EMPTY;
    const seedAttendees = initialAttendees ?? [];
    const seedSummary = initialSummary ?? "";
    const seedLocation = initialLocation ?? "";
    const seedDescription = initialDescription ?? "";
    if (initialStartAt) {
      return {
        ...EMPTY,
        summary: seedSummary,
        location: seedLocation,
        description: seedDescription,
        startLocal: rfc3339ToLocalDateTime(initialStartAt),
        endLocal: initialEndAt
          ? rfc3339ToLocalDateTime(initialEndAt)
          : rfc3339ToLocalDateTime(addHour(initialStartAt)),
        attendees: seedAttendees,
      };
    }
    return {
      ...EMPTY,
      summary: seedSummary,
      location: seedLocation,
      description: seedDescription,
      attendees: seedAttendees,
    };
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
          attendees: c.attendees ?? [],
        });
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [mode, commitmentId, t]);

  /** Shared time validator. Returns a translation key on failure, null
   *  when the form is acceptable for save. Centralised so the submit
   *  path and the auto-save-before-invite path enforce the exact same
   *  rule — diverging would let the user dodge validation by triggering
   *  the invitation flow on an invalid form. */
  const validateTimes = (): string | null => {
    if (!form.startLocal || !form.endLocal) {
      return "calendar.editor.missingTime";
    }
    if (form.allDay) {
      // All-day: end is *inclusive* in the UI form, so start == end is
      // a legitimate 1-day event. Only flag the strictly-backwards case.
      if (form.endLocal < form.startLocal) {
        return "calendar.editor.endBeforeStart";
      }
      return null;
    }
    // Non-all-day: require end >= start + 5min. Catches both the
    // strictly-backwards case ("Ende vor Beginn") and the
    // zero-or-near-zero-duration mistakes — RFC 5545 allows 0-minute
    // events but no calendar UX I've seen makes sense for one.
    const startMs = Date.parse(form.startLocal);
    const endMs = Date.parse(form.endLocal);
    if (Number.isNaN(startMs) || Number.isNaN(endMs)) {
      return "calendar.editor.missingTime";
    }
    if (endMs - startMs < 5 * 60 * 1000) {
      return endMs < startMs
        ? "calendar.editor.endBeforeStart"
        : "calendar.editor.endTooSoon";
    }
    return null;
  };

  const submit = async () => {
    if (busy) return;
    if (loaded?.subscriptionId) {
      // Belt-and-braces: the save button is hidden in this branch, but
      // a stray Enter on a form field would otherwise still fire submit.
      return;
    }
    const validationKey = validateTimes();
    if (validationKey) {
      setError(t(validationKey));
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
        attendees: form.attendees,
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

  const addAttendee = () => {
    const email = newAttendeeEmail.trim();
    if (!looksLikeEmail(email)) {
      setError(t("calendar.editor.attendees.invalidEmail"));
      return;
    }
    // Dedupe by lower-cased email — preserves whatever case the user
    // first typed for display, drops the second click on the same
    // address.
    if (
      form.attendees.some((a) =>
        a.email.toLowerCase() === email.toLowerCase(),
      )
    ) {
      setError(t("calendar.editor.attendees.duplicate"));
      return;
    }
    const displayName = newAttendeeName.trim();
    setForm((f) => ({
      ...f,
      attendees: [
        ...f.attendees,
        {
          email,
          displayName: displayName || null,
          partstat: null,
        },
      ],
    }));
    setNewAttendeeEmail("");
    setNewAttendeeName("");
    setError(null);
  };

  const removeAttendee = (email: string) => {
    setForm((f) => ({
      ...f,
      attendees: f.attendees.filter((a) => a.email !== email),
    }));
  };

  /** Autocomplete-pick path: insert the chosen contact straight into
   *  the list (no Hinzufügen-Klick needed) and clear the input row.
   *  Mirrors how Compose's autocomplete works — picking ist die
   *  Bestätigung. Dedupe is silent; if the contact's already on the
   *  list the inputs just reset. */
  const addAttendeeFromCompletion = (c: AddressCompletion) => {
    const email = c.email.trim();
    if (!email) return;
    if (
      form.attendees.some((a) => a.email.toLowerCase() === email.toLowerCase())
    ) {
      setNewAttendeeEmail("");
      setNewAttendeeName("");
      return;
    }
    // Prefer the user-curated display name from the address book over
    // the historical "displayName" we saw in past mail headers — same
    // priority Compose's `formatAddress` uses.
    const rawName = (c.contactDisplayName ?? c.displayName ?? "").trim();
    // Suppress the name when it's just the email local-part (no
    // signal), matching Compose's behavior.
    const localPart = email.split("@")[0]?.toLowerCase() ?? "";
    const displayName =
      rawName && rawName.toLowerCase() !== localPart ? rawName : null;
    setForm((f) => ({
      ...f,
      attendees: [
        ...f.attendees,
        { email, displayName, partstat: null },
      ],
    }));
    setNewAttendeeEmail("");
    setNewAttendeeName("");
    setError(null);
  };

  const senderAccount = useMemo(
    () => accounts.find((a) => a.id === senderAccountId),
    [accounts, senderAccountId],
  );

  // Sendable: must be a persisted event with at least one *non-self*
  // attendee and a configured sender. We mirror the backend's "drop
  // organizer from recipients" rule for the disabled-state hint so the
  // button doesn't promise something the backend will refuse.
  const sendableAttendees = useMemo(() => {
    if (!senderAccount) return form.attendees;
    return form.attendees.filter(
      (a) => a.email.toLowerCase() !== senderAccount.address.toLowerCase(),
    );
  }, [form.attendees, senderAccount]);

  const canSendInvitation =
    mode === "edit" &&
    !!commitmentId &&
    !loaded?.subscriptionId &&
    sendableAttendees.length > 0 &&
    !!senderAccount &&
    !busy;

  const handleSendInvitation = async () => {
    if (!canSendInvitation || !commitmentId || !senderAccount) return;
    // Auto-save pending edits before invoking the backend — the
    // user expects the invitation to reflect what's on screen, not
    // the last saved version. If save fails, abort: surfacing the
    // form-error is more useful than sending a stale ICS.
    if (loaded && hasUnsavedChanges(form, loaded)) {
      const ok = await persistDraft();
      if (!ok) return;
    }
    setBusy(true);
    setError(null);
    try {
      const draft = await invoke<InvitationRequestDraft>(
        "cal_build_invitation_request",
        { id: commitmentId, accountId: senderAccount.id },
      );
      // Reflect the SEQUENCE-bumped row in our local state so a
      // subsequent save doesn't fight it.
      setLoaded(draft.commitment);
      onCompose(buildInvitationComposeDraft(draft, senderAccount, t));
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  /** Persist the current form via `cal_update` / `cal_create`. Returns
   *  true on success. Same code path as `submit` but factored out so
   *  `handleSendInvitation` can reuse it without duplicating the
   *  RFC3339-conversion + draft assembly logic.
   *  TODO if this grows: extract a shared `buildDraft(form)` helper. */
  const persistDraft = async (): Promise<boolean> => {
    const validationKey = validateTimes();
    if (validationKey) {
      setError(t(validationKey));
      return false;
    }
    try {
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
        attendees: form.attendees,
      };
      const saved =
        mode === "edit" && commitmentId
          ? await invoke<Commitment>("cal_update", { id: commitmentId, draft })
          : await invoke<Commitment>("cal_create", { draft });
      setLoaded(saved);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
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

          <AttendeesSection
            attendees={form.attendees}
            newEmail={newAttendeeEmail}
            newName={newAttendeeName}
            onChangeNewEmail={setNewAttendeeEmail}
            onChangeNewName={setNewAttendeeName}
            onAdd={addAttendee}
            onPickFromAutocomplete={addAttendeeFromCompletion}
            onRemove={removeAttendee}
            disabled={!!loaded?.subscriptionId}
          />

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
                {!loaded?.subscriptionId && form.attendees.length > 0 && (
                  <>
                    {accounts.length > 1 && (
                      <select
                        value={senderAccountId}
                        onChange={(e) => setSenderAccountId(e.target.value)}
                        className="rounded border px-2 py-1 text-xs"
                        style={{
                          borderColor: "var(--border-soft)",
                          background: "var(--bg-base)",
                          color: "var(--fg-base)",
                        }}
                        title={t("calendar.editor.sendInvitation.fromAccount")}
                      >
                        {accounts.map((a) => (
                          <option key={a.id} value={a.id}>
                            {a.address}
                          </option>
                        ))}
                      </select>
                    )}
                    <button
                      type="button"
                      onClick={() => void handleSendInvitation()}
                      className="rounded px-3 py-1 text-xs font-medium"
                      style={{
                        background: "var(--accent)",
                        color: "#fff",
                        border: "1px solid var(--border-soft)",
                        opacity: canSendInvitation ? 1 : 0.5,
                      }}
                      disabled={!canSendInvitation}
                      title={
                        sendableAttendees.length === 0
                          ? t("calendar.editor.sendInvitation.onlySelf")
                          : t("calendar.editor.sendInvitation.tooltip")
                      }
                    >
                      {t("calendar.editor.sendInvitation.label")}
                    </button>
                  </>
                )}
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

function AttendeesSection({
  attendees,
  newEmail,
  newName,
  onChangeNewEmail,
  onChangeNewName,
  onAdd,
  onPickFromAutocomplete,
  onRemove,
  disabled,
}: {
  attendees: CommitmentAttendee[];
  newEmail: string;
  newName: string;
  onChangeNewEmail: (s: string) => void;
  onChangeNewName: (s: string) => void;
  onAdd: () => void;
  /** Autocomplete-Pick: ein gewählter Kontakt landet direkt in der
   *  Liste (kein extra Klick auf "Hinzufügen"), Inputs werden geleert. */
  onPickFromAutocomplete: (c: AddressCompletion) => void;
  onRemove: (email: string) => void;
  disabled: boolean;
}) {
  const { t } = useTranslation();
  // Ref-Anker für das Autocomplete-Dropdown. AddressAutocomplete misst
  // die BoundingBox dieses Inputs und positioniert das Dropdown
  // viewport-fixed darunter — funktioniert also auch im Modal.
  const emailInputRef = useRef<HTMLInputElement>(null);
  const handleAddKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    // Enter inside either input field triggers add — but only here,
    // not the outer form's submit. The form's onSubmit is what catches
    // the natural Enter, so we preventDefault to keep this confined.
    // Wenn das Autocomplete-Dropdown aktiv ist und Enter zum Picken
    // genutzt wird, fängt AddressAutocomplete den Event in der Capture-
    // Phase ab — wir kommen hier dann gar nicht mehr an.
    if (e.key === "Enter") {
      e.preventDefault();
      onAdd();
    }
  };
  return (
    <fieldset
      className="flex flex-col gap-1 rounded border px-2 py-2"
      style={{ borderColor: "var(--border-soft)" }}
      disabled={disabled}
    >
      <legend
        className="px-1 text-xs"
        style={{ color: "var(--fg-muted)" }}
      >
        {t("calendar.editor.attendees.label")}
      </legend>
      {attendees.length === 0 && (
        <p className="text-xs" style={{ color: "var(--fg-subtle)" }}>
          {t("calendar.editor.attendees.empty")}
        </p>
      )}
      {attendees.length > 0 && (
        <ul className="flex flex-col gap-1">
          {attendees.map((a) => (
            <li
              key={a.email}
              className="flex items-center justify-between gap-2 rounded px-2 py-1 text-xs"
              style={{ background: "var(--bg-soft)" }}
            >
              <span className="min-w-0 flex-1 truncate">
                {a.displayName ? (
                  <>
                    <span style={{ color: "var(--fg-base)" }}>
                      {a.displayName}
                    </span>{" "}
                    <span style={{ color: "var(--fg-muted)" }}>
                      &lt;{a.email}&gt;
                    </span>
                  </>
                ) : (
                  <span style={{ color: "var(--fg-base)" }}>{a.email}</span>
                )}
                {a.partstat && (
                  <PartstatBadge partstat={a.partstat} />
                )}
              </span>
              <button
                type="button"
                onClick={() => onRemove(a.email)}
                className="rounded px-2 py-0.5 text-[11px]"
                style={{
                  background: "transparent",
                  color: "var(--fg-error, #c00)",
                  border: "1px solid var(--border-soft)",
                }}
                aria-label={t("calendar.editor.attendees.remove")}
              >
                {t("calendar.editor.attendees.remove")}
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="mt-1 flex gap-2">
        <input
          ref={emailInputRef}
          type="email"
          value={newEmail}
          placeholder={t("calendar.editor.attendees.emailPlaceholder")}
          onChange={(e) => onChangeNewEmail(e.target.value)}
          onKeyDown={handleAddKey}
          className="flex-1 rounded border px-2 py-1 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-base)",
          }}
          autoComplete="off"
        />
        <input
          type="text"
          value={newName}
          placeholder={t("calendar.editor.attendees.namePlaceholder")}
          onChange={(e) => onChangeNewName(e.target.value)}
          onKeyDown={handleAddKey}
          className="flex-1 rounded border px-2 py-1 text-xs"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-base)",
          }}
        />
        <AddressAutocomplete
          anchorRef={emailInputRef}
          value={newEmail}
          mode="single"
          onPickContact={onPickFromAutocomplete}
        />
        <button
          type="button"
          onClick={onAdd}
          className="rounded px-3 py-1 text-xs"
          style={{
            background: "var(--bg-soft)",
            color: "var(--fg-base)",
            border: "1px solid var(--border-soft)",
          }}
        >
          {t("calendar.editor.attendees.add")}
        </button>
      </div>
    </fieldset>
  );
}

/** RFC 5545 PARTSTAT → human badge. Tints by tone so an "Abgesagt" is
 *  visually distinguishable from "Zugesagt" without reading the label.
 *  Unknown codes fall through as raw-uppercase + neutral color. */
function PartstatBadge({ partstat }: { partstat: string }) {
  const { t } = useTranslation();
  const code = partstat.toUpperCase();
  const labelKey = `calendar.editor.attendees.partstat.${code}`;
  const fallbackKey = "calendar.editor.attendees.partstat.UNKNOWN";
  // i18next returns the key unchanged when missing — use that as our
  // "did we have a translation" probe so we can fall back gracefully.
  const translated = t(labelKey);
  const label =
    translated === labelKey ? t(fallbackKey, { code }) : translated;
  const tones: Record<string, { fg: string; bg: string }> = {
    ACCEPTED: { fg: "#1e6a30", bg: "rgba(46,160,67,0.12)" },
    DECLINED: { fg: "#9a2424", bg: "rgba(207,34,46,0.10)" },
    TENTATIVE: { fg: "#8a6800", bg: "rgba(218,164,30,0.12)" },
    "NEEDS-ACTION": { fg: "var(--fg-muted)", bg: "var(--bg-base)" },
  };
  const tone = tones[code] ?? {
    fg: "var(--fg-muted)",
    bg: "var(--bg-base)",
  };
  return (
    <span
      className="ml-2 rounded px-1.5 py-0.5 text-[10px]"
      style={{
        color: tone.fg,
        background: tone.bg,
        border: "1px solid var(--border-soft)",
      }}
      title={t("calendar.editor.attendees.partstatTooltip")}
    >
      {label}
    </span>
  );
}

/** Detect form mutations not yet persisted. Used by the
 *  send-invitation handler to auto-save before invoking the backend
 *  (otherwise the outgoing ICS reflects the *last save*, not the
 *  visible form). Cheap shallow compare — we don't need diff-precision,
 *  only the "anything changed at all" signal. */
function hasUnsavedChanges(form: FormState, loaded: Commitment): boolean {
  const allDay = isAllDayEvent(loaded);
  const loadedStartLocal = allDay
    ? rfc3339ToLocalDate(loaded.startAt)
    : rfc3339ToLocalDateTime(loaded.startAt);
  const loadedEndLocal = allDay
    ? rfc3339ToLocalDate(addDays(loaded.endAt, -1))
    : rfc3339ToLocalDateTime(loaded.endAt);
  if (form.summary !== (loaded.summary ?? "")) return true;
  if (form.location !== (loaded.location ?? "")) return true;
  if (form.description !== (loaded.description ?? "")) return true;
  if (form.allDay !== allDay) return true;
  if (form.startLocal !== loadedStartLocal) return true;
  if (form.endLocal !== loadedEndLocal) return true;
  if (form.attendees.length !== (loaded.attendees?.length ?? 0)) return true;
  const sorted = [...form.attendees]
    .map((a) => `${a.email.toLowerCase()}|${a.displayName ?? ""}`)
    .sort();
  const sortedLoaded = (loaded.attendees ?? [])
    .map((a) => `${a.email.toLowerCase()}|${a.displayName ?? ""}`)
    .sort();
  return sorted.some((s, i) => s !== sortedLoaded[i]);
}

/** Build a ComposeDraft from a backend-prepared REQUEST ICS. Mirror of
 *  `buildIcsReplyComposeDraft` in IcsInvitePanel — same iMIP Content-Type
 *  trick so the SMTP path's multipart/alternative branch fires. */
function buildInvitationComposeDraft(
  draft: InvitationRequestDraft,
  account: AccountSummary,
  t: (key: string, vars?: Record<string, string>) => string,
): ComposeDraft {
  const summary = draft.eventSummary ?? t("calendar.editor.attendees.untitled");
  const subject = t("calendar.editor.sendInvitation.subject", { summary });
  const body = t("calendar.editor.sendInvitation.body", { summary });
  const attachment: ComposeAttachment = {
    clientId: `ics-request-${Date.now()}`,
    path: draft.attachmentPath,
    filename: draft.attachmentFilename,
    sizeBytes: draft.attachmentSizeBytes,
    mimeType: "text/calendar; method=REQUEST; charset=utf-8",
  };
  const toAddrs = draft.recipients
    .map((r) =>
      r.displayName ? `${r.displayName} <${r.email}>` : r.email,
    )
    .join(", ");
  return {
    accountId: account.id,
    to: toAddrs,
    cc: "",
    bcc: "",
    subject,
    body,
    attachments: [attachment],
  };
}

// ─── Datetime conversion helpers ─────────────────────────────────────────
//
// `localDateTimeToRfc3339` lives in `utils/calendarEvent.ts` — shared
// with the Reader's "Termin aus Mail"-Pfad so both produce identical
// timestamps. The rest below is editor-local form ↔ display glue.

function pad2(n: number): string {
  return String(n).padStart(2, "0");
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
