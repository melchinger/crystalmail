// Phase 3 calendar UI: when an open mail carries an
// `application/time-protocol+json` attachment, this panel parses the
// envelope, applies it to the local negotiation state machine, and
// renders the current thread state with action buttons appropriate to
// the role (initiator / responder) and current state (requested,
// proposed, confirmed, released).
//
// Side-by-side with `IcsInvitePanel`: that panel handles RFC-5546
// iMIP invitations (interop with Outlook / Google / Zoho); this one
// handles native timeProtocol envelopes (interop with another
// CrystalMail or with timeBank). Both can render in the same Reader.

import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type {
  AccountSummary,
  AttachmentMeta,
  Negotiation,
  NegotiationSlot,
  SlotInput,
} from "../types";

type Props = {
  messageId: string;
  attachments: AttachmentMeta[];
  account: AccountSummary | undefined;
};

const ENVELOPE_MIME_PREFIX = "application/time-protocol+json";

function isEnvelopeAttachment(a: AttachmentMeta): boolean {
  const mime = a.mimeType.toLowerCase().split(";")[0]?.trim() ?? "";
  return mime === ENVELOPE_MIME_PREFIX;
}

export function NegotiationPanel({ messageId, attachments, account }: Props) {
  const { t } = useTranslation();
  const envelopeAttachment = useMemo(
    () => attachments.find(isEnvelopeAttachment),
    [attachments],
  );

  const [neg, setNeg] = useState<Negotiation | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Apply the envelope on mount + whenever the attachment identity
  // changes. The backend is idempotent on `message_id` so re-applying
  // on every mail-open is safe.
  useEffect(() => {
    if (!envelopeAttachment || !account) {
      setNeg(null);
      setError(null);
      return;
    }
    let cancelled = false;
    (async () => {
      try {
        const result = await invoke<Negotiation>(
          "tp_apply_envelope_from_attachment",
          {
            messageId,
            partIdx: envelopeAttachment.partIdx,
            ownEmail: account.address,
          },
        );
        if (!cancelled) {
          setNeg(result);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) {
          setError(String(e));
          setNeg(null);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [messageId, envelopeAttachment?.partIdx, account?.address]);

  const refresh = useCallback(async () => {
    if (!neg) return;
    try {
      const updated = await invoke<Negotiation | null>("tp_get_negotiation", {
        negotiationId: neg.negotiationId,
      });
      if (updated) setNeg(updated);
    } catch (e) {
      console.warn("refresh negotiation failed", e);
    }
  }, [neg]);

  if (!envelopeAttachment) return null;
  if (error) {
    return (
      <div
        className="mx-3 mt-2 rounded-md border px-3 py-2 text-sm"
        style={{
          borderColor: "var(--border-soft)",
          color: "var(--fg-error, #c00)",
        }}
      >
        {t("negotiation.parseError", { error })}
      </div>
    );
  }
  if (!neg || !account) return null;

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
        {t("negotiation.label")} · {t(`negotiation.role.${neg.threadRole}`)} ·{" "}
        {t(`negotiation.state.${neg.state}`)}
      </div>
      <div
        className="mt-0.5 truncate text-base font-medium"
        style={{ color: "var(--fg-base)" }}
      >
        {neg.displaySummary || t("negotiation.untitled")}
      </div>
      <dl
        className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[12px]"
        style={{ color: "var(--fg-muted)" }}
      >
        <dt>{t("negotiation.with")}</dt>
        <dd className="truncate">
          {neg.counterpartyName
            ? `${neg.counterpartyName} <${neg.counterpartyEmail}>`
            : neg.counterpartyEmail}
        </dd>
        {neg.durationIso && (
          <>
            <dt>{t("negotiation.duration")}</dt>
            <dd>{formatDuration(neg.durationIso, t)}</dd>
          </>
        )}
        {neg.constraints?.latest && (
          <>
            <dt>{t("negotiation.latest")}</dt>
            <dd>{formatDateTime(neg.constraints.latest)}</dd>
          </>
        )}
        {neg.constraints?.preferredTime && (
          <>
            <dt>{t("negotiation.preferred")}</dt>
            <dd>{neg.constraints.preferredTime}</dd>
          </>
        )}
      </dl>

      <NegotiationActions
        neg={neg}
        account={account}
        busy={busy}
        setBusy={setBusy}
        onUpdated={(n) => setNeg(n)}
        refresh={refresh}
      />
    </div>
  );
}

function NegotiationActions({
  neg,
  account,
  busy,
  setBusy,
  onUpdated,
  refresh,
}: {
  neg: Negotiation;
  account: AccountSummary;
  busy: boolean;
  setBusy: (b: boolean) => void;
  onUpdated: (n: Negotiation) => void;
  refresh: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const [proposing, setProposing] = useState(false);
  const [proposedSlots, setProposedSlots] = useState<SlotInput[]>([]);

  const sendPropose = async (isCounter: boolean) => {
    if (proposedSlots.length === 0) return;
    setBusy(true);
    try {
      const updated = await invoke<Negotiation>("tp_send_propose_slots", {
        negotiationId: neg.negotiationId,
        slots: proposedSlots,
        accountId: account.id,
        isCounter,
      });
      onUpdated(updated);
      setProposing(false);
      setProposedSlots([]);
    } catch (e) {
      alert(t("negotiation.error.send", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const sendConfirm = async (slotId: string) => {
    setBusy(true);
    try {
      const updated = await invoke<Negotiation>("tp_send_confirm_slot", {
        negotiationId: neg.negotiationId,
        slotId,
        accountId: account.id,
      });
      onUpdated(updated);
    } catch (e) {
      alert(t("negotiation.error.send", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const sendRelease = async (slotId: string) => {
    setBusy(true);
    try {
      const updated = await invoke<Negotiation>("tp_send_release_slot", {
        negotiationId: neg.negotiationId,
        slotId,
        accountId: account.id,
      });
      onUpdated(updated);
    } catch (e) {
      alert(t("negotiation.error.send", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  // Terminal states: read-only display.
  if (neg.state === "confirmed") {
    const confirmed = neg.slots.find((s) => s.status === "confirmed");
    return (
      <div className="mt-2">
        <p className="text-sm" style={{ color: "var(--fg-base)" }}>
          {confirmed
            ? t("negotiation.confirmedSlot", {
                range: formatRange(confirmed),
              })
            : t("negotiation.confirmedNoSlot")}
        </p>
        {neg.confirmedCommitmentId && (
          <p
            className="mt-1 text-xs"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("negotiation.commitmentMaterialized")}
          </p>
        )}
      </div>
    );
  }
  if (neg.state === "released" || neg.state === "expired") {
    return (
      <p
        className="mt-2 text-sm"
        style={{ color: "var(--fg-subtle)" }}
      >
        {t(`negotiation.terminal.${neg.state}`)}
      </p>
    );
  }

  // Non-terminal: render based on whose turn it is.
  // Convention: the side waiting for input from the OTHER party shows
  // "warten auf Antwort", the side that owes input shows action UI.
  const activeSlots = neg.slots.filter((s) => s.status === "active");

  // Responder has just received `request` (state=requested) — they
  // owe slot proposals.
  if (neg.state === "requested" && neg.threadRole === "responder") {
    return (
      <SlotProposeForm
        proposing={proposing || activeSlots.length > 0}
        slots={proposedSlots}
        setSlots={setProposedSlots}
        onCancel={() => {
          setProposing(false);
          setProposedSlots([]);
        }}
        onSubmit={() => void sendPropose(false)}
        onStart={() => setProposing(true)}
        busy={busy}
        ctaKey="negotiation.propose"
      />
    );
  }

  // Initiator looking at proposed slots from the responder.
  if (neg.state === "proposed" && neg.threadRole === "initiator") {
    if (proposing) {
      // Counter-propose flow.
      return (
        <SlotProposeForm
          proposing
          slots={proposedSlots}
          setSlots={setProposedSlots}
          onCancel={() => {
            setProposing(false);
            setProposedSlots([]);
          }}
          onSubmit={() => void sendPropose(true)}
          onStart={() => setProposing(true)}
          busy={busy}
          ctaKey="negotiation.counterPropose"
        />
      );
    }
    return (
      <ProposedSlotsView
        slots={activeSlots}
        onConfirm={(id) => void sendConfirm(id)}
        onRelease={(id) => void sendRelease(id)}
        onCounterPropose={() => setProposing(true)}
        busy={busy}
      />
    );
  }

  // Other transient states (we already proposed, waiting on
  // counterparty; etc.) — just show "waiting".
  return (
    <p
      className="mt-2 text-sm"
      style={{ color: "var(--fg-subtle)" }}
    >
      {t("negotiation.waiting")}
      <button
        type="button"
        onClick={() => void refresh()}
        className="ml-2 text-xs underline"
        disabled={busy}
      >
        {t("negotiation.refresh")}
      </button>
    </p>
  );
}

function SlotProposeForm({
  proposing,
  slots,
  setSlots,
  onStart,
  onCancel,
  onSubmit,
  busy,
  ctaKey,
}: {
  proposing: boolean;
  slots: SlotInput[];
  setSlots: (s: SlotInput[]) => void;
  onStart: () => void;
  onCancel: () => void;
  onSubmit: () => void;
  busy: boolean;
  ctaKey: string;
}) {
  const { t } = useTranslation();
  if (!proposing) {
    return (
      <button
        type="button"
        onClick={onStart}
        className="mt-2 rounded px-3 py-1 text-xs font-medium"
        style={{
          background: "var(--accent)",
          color: "#fff",
          border: "1px solid var(--border-soft)",
        }}
      >
        {t(ctaKey)}
      </button>
    );
  }

  const addSlot = () => {
    const last = slots[slots.length - 1];
    const baseStart = last
      ? new Date(last.endAt)
      : nextHalfHour(new Date());
    const start = new Date(baseStart);
    const end = new Date(start);
    end.setMinutes(end.getMinutes() + 30);
    setSlots([
      ...slots,
      { startAt: start.toISOString(), endAt: end.toISOString() },
    ]);
  };

  const updateSlot = (i: number, patch: Partial<SlotInput>) => {
    setSlots(slots.map((s, idx) => (idx === i ? { ...s, ...patch } : s)));
  };

  const removeSlot = (i: number) => {
    setSlots(slots.filter((_, idx) => idx !== i));
  };

  return (
    <div className="mt-2">
      <ul className="flex flex-col gap-1">
        {slots.map((slot, i) => (
          <li key={i} className="flex items-center gap-2 text-xs">
            <input
              type="datetime-local"
              value={toLocalDateTime(slot.startAt)}
              onChange={(e) =>
                updateSlot(i, {
                  startAt: localDateTimeToIso(e.target.value),
                })
              }
              className="rounded border px-1 py-0.5"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
              }}
            />
            <span style={{ color: "var(--fg-muted)" }}>—</span>
            <input
              type="datetime-local"
              value={toLocalDateTime(slot.endAt)}
              onChange={(e) =>
                updateSlot(i, {
                  endAt: localDateTimeToIso(e.target.value),
                })
              }
              className="rounded border px-1 py-0.5"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
              }}
            />
            <button
              type="button"
              onClick={() => removeSlot(i)}
              className="ml-auto rounded px-2 text-xs"
              style={{ color: "var(--fg-subtle)" }}
            >
              ×
            </button>
          </li>
        ))}
      </ul>
      <div className="mt-2 flex gap-2">
        <button
          type="button"
          onClick={addSlot}
          className="rounded px-2 py-1 text-xs"
          style={{
            background: "var(--bg-soft)",
            border: "1px solid var(--border-soft)",
          }}
        >
          + {t("negotiation.addSlot")}
        </button>
        <button
          type="button"
          onClick={onSubmit}
          disabled={busy || slots.length === 0}
          className="ml-auto rounded px-3 py-1 text-xs font-medium disabled:opacity-50"
          style={{
            background: "var(--accent)",
            color: "#fff",
            border: "1px solid var(--border-soft)",
          }}
        >
          {t(ctaKey)}
        </button>
        <button
          type="button"
          onClick={onCancel}
          disabled={busy}
          className="rounded px-3 py-1 text-xs"
          style={{
            background: "var(--bg-soft)",
            border: "1px solid var(--border-soft)",
          }}
        >
          {t("negotiation.cancel")}
        </button>
      </div>
    </div>
  );
}

function ProposedSlotsView({
  slots,
  onConfirm,
  onRelease,
  onCounterPropose,
  busy,
}: {
  slots: NegotiationSlot[];
  onConfirm: (slotId: string) => void;
  onRelease: (slotId: string) => void;
  onCounterPropose: () => void;
  busy: boolean;
}) {
  const { t } = useTranslation();
  if (slots.length === 0) {
    return (
      <p
        className="mt-2 text-sm"
        style={{ color: "var(--fg-subtle)" }}
      >
        {t("negotiation.noActiveSlots")}
      </p>
    );
  }
  return (
    <div className="mt-2">
      <ul className="flex flex-col gap-1">
        {slots.map((slot) => (
          <li
            key={slot.slotId}
            className="flex items-center gap-2 rounded border px-2 py-1 text-xs"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
            }}
          >
            <span className="flex-1 tabular-nums">{formatRange(slot)}</span>
            <button
              type="button"
              onClick={() => onConfirm(slot.slotId)}
              disabled={busy}
              className="rounded px-2 py-0.5 text-xs font-medium disabled:opacity-50"
              style={{
                background: "var(--accent)",
                color: "#fff",
                border: "1px solid var(--border-soft)",
              }}
            >
              {t("negotiation.confirm")}
            </button>
            <button
              type="button"
              onClick={() => onRelease(slot.slotId)}
              disabled={busy}
              className="rounded px-2 py-0.5 text-xs disabled:opacity-50"
              style={{
                background: "var(--bg-soft)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-soft)",
              }}
            >
              {t("negotiation.release")}
            </button>
          </li>
        ))}
      </ul>
      <button
        type="button"
        onClick={onCounterPropose}
        disabled={busy}
        className="mt-2 rounded px-3 py-1 text-xs"
        style={{
          background: "var(--bg-soft)",
          color: "var(--fg-base)",
          border: "1px solid var(--border-soft)",
        }}
      >
        {t("negotiation.counterProposeStart")}
      </button>
    </div>
  );
}

// ─── Formatting helpers ──────────────────────────────────────────────────

const FMT_DATE = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  day: "2-digit",
  month: "short",
});
const FMT_TIME = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
});

function formatDateTime(iso: string): string {
  const d = new Date(iso);
  return `${FMT_DATE.format(d)} ${FMT_TIME.format(d)}`;
}

function formatRange(slot: NegotiationSlot): string {
  const start = new Date(slot.startAt);
  const end = new Date(slot.endAt);
  const sameDay =
    start.getFullYear() === end.getFullYear() &&
    start.getMonth() === end.getMonth() &&
    start.getDate() === end.getDate();
  if (sameDay) {
    return `${FMT_DATE.format(start)} ${FMT_TIME.format(start)}–${FMT_TIME.format(end)}`;
  }
  return `${FMT_DATE.format(start)} ${FMT_TIME.format(start)} – ${FMT_DATE.format(end)} ${FMT_TIME.format(end)}`;
}

/** Minimal ISO-8601 duration → human renderer. Covers the cases we
 *  emit (`PT15M`, `PT1H`, `PT1H30M`); falls back to the raw string for
 *  anything richer. */
function formatDuration(
  iso: string,
  t: (key: string, params?: Record<string, number>) => string,
): string {
  const m = iso.match(/^PT(?:(\d+)H)?(?:(\d+)M)?$/);
  if (!m) return iso;
  const hours = m[1] ? Number(m[1]) : 0;
  const minutes = m[2] ? Number(m[2]) : 0;
  if (hours && minutes) return t("negotiation.durationHm", { hours, minutes });
  if (hours) return t("negotiation.durationH", { hours });
  if (minutes) return t("negotiation.durationM", { minutes });
  return iso;
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

function toLocalDateTime(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}T${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

function localDateTimeToIso(local: string): string {
  // Treat the input as local wall-clock; new Date does that natively.
  return new Date(local).toISOString();
}

function nextHalfHour(d: Date): Date {
  const out = new Date(d);
  out.setSeconds(0, 0);
  out.setMinutes(out.getMinutes() < 30 ? 30 : 60);
  return out;
}
