// Calendar UI for mails carrying a `text/calendar` attachment. Three flavors:
//   - METHOD:REQUEST (or attendees, no method) — invitation we received.
//     Three response buttons (Accept/Tentative/Decline) — clicking builds
//     an RFC 5546 REPLY ICS, drops it into a Compose draft pre-addressed
//     to the organizer.
//   - METHOD:REPLY — someone we invited responded. We auto-apply the
//     PARTSTAT to the matching local commitment on mount and show a
//     "Bob hat zugesagt zu X" status line.
//   - METHOD:CANCEL — not handled here (future).

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type {
  AccountSummary,
  AttachmentMeta,
  ComposeAttachment,
  ComposeDraft,
  InvitationReplyDraft,
  InvitationResponse,
  ParsedIcsEvent,
} from "../types";

/** Backend `cal_apply_invitation_reply` result. Mirror of the Rust
 *  `InvitationReplyApplied`. */
type InvitationReplyApplied = {
  uid: string;
  outcome: "applied" | "noMatchingCommitment" | "noMatchingAttendee";
  responderPartstat: string | null;
  responderEmail: string | null;
  responderDisplayName: string | null;
  commitment: unknown | null;
};

type Props = {
  messageId: string;
  attachments: AttachmentMeta[];
  account: AccountSummary | undefined;
  onCompose: (draft: ComposeDraft) => void;
};

/** Recognized as iCalendar if the MIME is `text/calendar` / `application/ics`
 *  (case-insensitive, ignoring parameters like `; method=REQUEST`) or the
 *  filename ends in `.ics`. Outlook's TNEF-wrapped `winmail.dat` does NOT
 *  match — those don't carry plain VCALENDAR text. */
function isIcsAttachment(a: AttachmentMeta): boolean {
  const mime = a.mimeType.toLowerCase().split(";")[0]?.trim() ?? "";
  if (mime === "text/calendar" || mime === "application/ics") return true;
  return a.filename.toLowerCase().endsWith(".ics");
}

export function IcsInvitePanel({
  messageId,
  attachments,
  account,
  onCompose,
}: Props) {
  const { t } = useTranslation();
  const ics = attachments.find(isIcsAttachment);

  const [event, setEvent] = useState<ParsedIcsEvent | null>(null);
  const [busy, setBusy] = useState(false);
  // REPLY branch only: filled by the auto-apply effect below. Null until
  // the apply finishes (or stays null when this isn't a REPLY).
  const [replyResult, setReplyResult] = useState<InvitationReplyApplied | null>(
    null,
  );
  // Tracks the (messageId, partIdx) pair that was last parsed. Without it,
  // navigating from mail A → mail B → mail A could leave a stale event from
  // A's first parse if B's parse is in flight when we re-mount.
  const liveRef = useRef<{ messageId: string; partIdx: number } | null>(null);

  useEffect(() => {
    if (!ics) {
      setEvent(null);
      setReplyResult(null);
      liveRef.current = null;
      return;
    }
    const target = { messageId, partIdx: ics.partIdx };
    liveRef.current = target;
    setEvent(null);
    setReplyResult(null);
    void (async () => {
      try {
        const parsed = await invoke<ParsedIcsEvent | null>(
          "ics_parse_attachment",
          { messageId, partIdx: ics.partIdx },
        );
        if (
          liveRef.current?.messageId !== target.messageId ||
          liveRef.current?.partIdx !== target.partIdx
        ) {
          return;
        }
        setEvent(parsed);
        // REPLY auto-apply: when the inbound ICS announces it's a REPLY,
        // push the PARTSTAT into the matching local commitment. This is
        // idempotent (re-opening the mail just writes the same value),
        // and the backend silently skips when the UID doesn't resolve —
        // so we can fire-and-await without risking weird state.
        const isReply =
          parsed?.method?.toUpperCase() === "REPLY";
        if (isReply) {
          try {
            const applied = await invoke<InvitationReplyApplied>(
              "cal_apply_invitation_reply",
              { messageId, partIdx: ics.partIdx },
            );
            if (
              liveRef.current?.messageId === target.messageId &&
              liveRef.current?.partIdx === target.partIdx
            ) {
              setReplyResult(applied);
            }
          } catch (err) {
            // Apply failures don't block the panel — we still want to
            // show "what is this mail" metadata. Logs go to dev console.
            console.warn("apply reply failed", err);
          }
        }
      } catch (err) {
        // Malformed ICS isn't a user-facing error — best-effort
        // recognition. Logging keeps the dev console useful without
        // surfacing a banner the user can't act on.
        console.warn("ics parse failed", err);
        if (
          liveRef.current?.messageId === target.messageId &&
          liveRef.current?.partIdx === target.partIdx
        ) {
          setEvent(null);
        }
      }
    })();
  }, [messageId, ics?.partIdx]);

  if (!ics || !event) return null;
  const isReply = event.method?.toUpperCase() === "REPLY";
  // Calendar publications without attendees can't be replied to, and
  // REPLY mails are responses *to* us — neither shows the response
  // buttons. We still render event metadata for both.
  const canRespond =
    !isReply && event.isInvitation && event.organizer !== null;

  const handleRespond = async (response: InvitationResponse) => {
    if (!ics || !account || busy) return;
    setBusy(true);
    try {
      const reply = await invoke<InvitationReplyDraft>(
        "ics_build_invitation_reply",
        {
          messageId,
          partIdx: ics.partIdx,
          response,
          attendeeEmail: account.address,
          attendeeName: account.fromName || null,
        },
      );
      // Phase 1: also stash the event in the local calendar so the user
      // sees it under "Heute / Diese Woche" without having to keep the
      // mail around. We pass our PARTSTAT in so the stored row reflects
      // the choice we just made. This is best-effort — a calendar-write
      // failure must not block the response mail; the user already
      // decided to send it. Errors land in the dev console for triage.
      try {
        await invoke("cal_import_ics_attachment", {
          messageId,
          partIdx: ics.partIdx,
          myEmail: account.address,
          myPartstat: response,
        });
      } catch (err) {
        console.warn("calendar import on respond failed", err);
      }
      onCompose(buildIcsReplyComposeDraft(reply, account, t));
    } catch (err) {
      console.error("ics reply build failed", err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="mx-3 mt-2 rounded-md border px-3 py-2 text-sm"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-base)",
      }}
    >
      <div
        className="text-[11px] uppercase tracking-wide"
        style={{ color: "var(--fg-subtle)" }}
      >
        {isReply
          ? t("calendar.invitation.replyLabel")
          : t("calendar.invitation.label")}
      </div>
      <div
        className="mt-0.5 truncate text-base font-medium"
        style={{ color: "var(--fg-base)" }}
      >
        {event.summary || t("calendar.invitation.untitled")}
      </div>

      <dl
        className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[12px]"
        style={{ color: "var(--fg-muted)" }}
      >
        {event.dtstart && (
          <>
            <dt>{t("calendar.invitation.when")}</dt>
            <dd>{formatIcsRange(event.dtstart, event.dtend)}</dd>
          </>
        )}
        {event.location && (
          <>
            <dt>{t("calendar.invitation.where")}</dt>
            <dd className="truncate">{event.location}</dd>
          </>
        )}
        {event.organizer && (
          <>
            <dt>{t("calendar.invitation.organizer")}</dt>
            <dd className="truncate">
              {event.organizer.displayName
                ? `${event.organizer.displayName} <${event.organizer.email}>`
                : event.organizer.email}
            </dd>
          </>
        )}
      </dl>

      {canRespond && (
        <div className="mt-2 flex gap-2">
          <ResponseButton
            label={t("calendar.invitation.accept")}
            tone="accept"
            onClick={() => handleRespond("accepted")}
            disabled={busy || !account}
          />
          <ResponseButton
            label={t("calendar.invitation.tentative")}
            tone="tentative"
            onClick={() => handleRespond("tentative")}
            disabled={busy || !account}
          />
          <ResponseButton
            label={t("calendar.invitation.decline")}
            tone="decline"
            onClick={() => handleRespond("declined")}
            disabled={busy || !account}
          />
        </div>
      )}

      {isReply && (
        <ReplyStatus result={replyResult} eventSummary={event.summary} />
      )}
    </div>
  );
}

function ResponseButton({
  label,
  tone,
  onClick,
  disabled,
}: {
  label: string;
  tone: "accept" | "tentative" | "decline";
  onClick: () => void;
  disabled: boolean;
}) {
  const palette: Record<typeof tone, { bg: string; fg: string }> = {
    accept: { bg: "var(--accent)", fg: "#fff" },
    tentative: { bg: "var(--bg-soft)", fg: "var(--fg-base)" },
    decline: { bg: "var(--bg-soft)", fg: "var(--fg-base)" },
  };
  const colors = palette[tone];
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="rounded px-3 py-1 text-xs font-medium disabled:opacity-50"
      style={{
        background: colors.bg,
        color: colors.fg,
        border: "1px solid var(--border-soft)",
      }}
    >
      {label}
    </button>
  );
}

/** Status line for incoming REPLY mails. Shows "Bob hat zugesagt zu X"
 *  (or declined/tentative) once the backend's apply call returns. While
 *  the apply is still in flight, renders a subtle "wird gespeichert …"
 *  placeholder so the user knows something happened. */
function ReplyStatus({
  result,
  eventSummary,
}: {
  result: InvitationReplyApplied | null;
  eventSummary: string | null;
}) {
  const { t } = useTranslation();
  // In-flight: backend hasn't reported back yet.
  if (!result) {
    return (
      <div
        className="mt-2 text-xs"
        style={{ color: "var(--fg-subtle)" }}
      >
        {t("calendar.invitation.reply.applying")}
      </div>
    );
  }
  const summary = eventSummary ?? t("calendar.invitation.untitled");
  const who =
    result.responderDisplayName ?? result.responderEmail ?? t("calendar.invitation.reply.someone");
  const partstat = (result.responderPartstat ?? "").toUpperCase();
  // Map PARTSTAT → human verb. NEEDS-ACTION on a REPLY is weird but
  // legal (responder is signalling "I see this, will decide later").
  const verbKey =
    partstat === "ACCEPTED"
      ? "calendar.invitation.reply.verbAccepted"
      : partstat === "DECLINED"
        ? "calendar.invitation.reply.verbDeclined"
        : partstat === "TENTATIVE"
          ? "calendar.invitation.reply.verbTentative"
          : "calendar.invitation.reply.verbResponded";
  const headline = t(verbKey, { who, summary });

  const tone =
    partstat === "ACCEPTED"
      ? "var(--fg-success, #2a8a3e)"
      : partstat === "DECLINED"
        ? "var(--fg-error, #c00)"
        : "var(--fg-muted)";

  let note: string | null = null;
  if (result.outcome === "noMatchingCommitment") {
    note = t("calendar.invitation.reply.noLocalEvent");
  } else if (result.outcome === "noMatchingAttendee") {
    note = t("calendar.invitation.reply.unknownResponder");
  }

  return (
    <div className="mt-2 flex flex-col gap-0.5 text-xs">
      <div style={{ color: tone, fontWeight: 500 }}>{headline}</div>
      {note && (
        <div style={{ color: "var(--fg-subtle)" }}>{note}</div>
      )}
    </div>
  );
}

/** Build a ComposeDraft from a backend-prepared REPLY ICS. The body text is
 *  short and human-friendly; the attached `text/calendar` is what calendar
 *  servers actually parse. */
function buildIcsReplyComposeDraft(
  reply: InvitationReplyDraft,
  account: AccountSummary,
  t: (key: string, vars?: Record<string, string>) => string,
): ComposeDraft {
  const subjectKey = `calendar.invitation.subject.${reply.response}`;
  const bodyKey = `calendar.invitation.body.${reply.response}`;
  const summary = reply.eventSummary ?? t("calendar.invitation.untitled");
  const subject = t(subjectKey, { summary });
  const body = t(bodyKey, { summary });
  // iMIP RFC 6047: the `method=REPLY` parameter is what tells receiving
  // calendar servers (Zoho, Outlook, Google) to auto-process the message as
  // an invitation response instead of displaying it as a plain mail with an
  // .ics attachment. The backend SMTP path detects this Content-Type and
  // routes the ICS into a multipart/alternative body part rather than
  // wrapping it as a multipart/mixed attachment.
  const attachment: ComposeAttachment = {
    clientId: `ics-reply-${Date.now()}`,
    path: reply.attachmentPath,
    filename: reply.attachmentFilename,
    sizeBytes: reply.attachmentSizeBytes,
    mimeType: "text/calendar; method=REPLY; charset=utf-8",
  };
  const toAddr = reply.recipientDisplayName
    ? `${reply.recipientDisplayName} <${reply.recipientEmail}>`
    : reply.recipientEmail;
  return {
    accountId: account.id,
    to: toAddr,
    cc: "",
    bcc: "",
    subject,
    body,
    attachments: [attachment],
  };
}

// ─── Date formatting ───────────────────────────────────────────────────────
//
// Phase 0 keeps this minimal: we recognize the four common DTSTART shapes
// and render with the user's locale. Anything else falls back to the raw
// string so the user at least sees *something* coherent.
//
// The formats:
//   `20260423T090000Z`              → UTC, Date-aware
//   `20260423T090000`               → floating, treat as local
//   `TZID=Europe/Berlin:20260423T090000` → strip TZID, treat as local (the
//                                          right thing only when the user's
//                                          local TZ matches; v1+ will need
//                                          a real TZ resolver — see Phase 1
//                                          decision in the calendar memory)
//   `20260423`                      → date-only, all-day

const ICS_TIME_RE = /^(?:TZID=[^:]+:)?(\d{4})(\d{2})(\d{2})(?:T(\d{2})(\d{2})(\d{2})(Z)?)?$/;

function parseIcsTime(raw: string): { date: Date; allDay: boolean } | null {
  const m = raw.match(ICS_TIME_RE);
  if (!m) return null;
  const [, y, mo, d, h, mi, s, z] = m;
  const yi = Number(y);
  const moi = Number(mo) - 1;
  const di = Number(d);
  if (!h) {
    // All-day: anchor at noon local to dodge the DST midnight shift hazards.
    return { date: new Date(yi, moi, di, 12), allDay: true };
  }
  const hi = Number(h);
  const mii = Number(mi);
  const si = Number(s);
  if (z) {
    return { date: new Date(Date.UTC(yi, moi, di, hi, mii, si)), allDay: false };
  }
  return { date: new Date(yi, moi, di, hi, mii, si), allDay: false };
}

function formatIcsRange(dtstart: string, dtend: string | null): string {
  const start = parseIcsTime(dtstart);
  if (!start) return dtstart;
  const fmtDate = new Intl.DateTimeFormat(undefined, {
    weekday: "short",
    day: "2-digit",
    month: "short",
    year: "numeric",
  });
  if (start.allDay) {
    return fmtDate.format(start.date);
  }
  const fmtTime = new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
  const startStr = `${fmtDate.format(start.date)}, ${fmtTime.format(start.date)}`;
  if (!dtend) return startStr;
  const end = parseIcsTime(dtend);
  if (!end) return startStr;
  // Same calendar day → render the second timestamp as time only.
  const sameDay =
    start.date.getFullYear() === end.date.getFullYear() &&
    start.date.getMonth() === end.date.getMonth() &&
    start.date.getDate() === end.date.getDate();
  if (sameDay) {
    return `${startStr}–${fmtTime.format(end.date)}`;
  }
  return `${startStr} – ${fmtDate.format(end.date)}, ${fmtTime.format(end.date)}`;
}
