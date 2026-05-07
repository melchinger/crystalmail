// Phase 3.3: initiate-from-scratch dialog. Lets the user kick off a
// new timeProtocol negotiation by choosing recipient + duration +
// optional constraints. Backend mints the `negotiation_id` and sends
// the `request` envelope; the resulting Negotiation appears in the
// counterparty's Reader (or in our own threads later, once we add a
// negotiations list view in 3.5+).

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { AccountSummary, Negotiation } from "../types";

type Props = {
  onClose: () => void;
  onSent: (neg: Negotiation) => void;
};

const DURATION_PRESETS: { iso: string; labelKey: string }[] = [
  { iso: "PT15M", labelKey: "negotiation.start.duration15" },
  { iso: "PT30M", labelKey: "negotiation.start.duration30" },
  { iso: "PT45M", labelKey: "negotiation.start.duration45" },
  { iso: "PT1H", labelKey: "negotiation.start.duration60" },
  { iso: "PT1H30M", labelKey: "negotiation.start.duration90" },
  { iso: "PT2H", labelKey: "negotiation.start.duration120" },
];

export function NegotiationStartDialog({ onClose, onSent }: Props) {
  const { t } = useTranslation();
  const [accounts, setAccounts] = useState<AccountSummary[]>([]);
  const [accountId, setAccountId] = useState<string>("");
  const [toEmail, setToEmail] = useState("");
  const [duration, setDuration] = useState("PT45M");
  const [summary, setSummary] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [latest, setLatest] = useState("");
  const [preferredTime, setPreferredTime] = useState("");
  const [minimumNotice, setMinimumNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load accounts + the calendar-config-account as the default sender.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await invoke<AccountSummary[]>("list_accounts");
        if (cancelled) return;
        setAccounts(list);
        // Default to the calendar-account if configured, else first.
        try {
          const cfg = await invoke<{ accountId: string | null }>(
            "cal_get_config",
          );
          const preferred = cfg?.accountId
            ? list.find((a) => a.id === cfg.accountId)
            : null;
          setAccountId(preferred?.id ?? list[0]?.id ?? "");
        } catch {
          setAccountId(list[0]?.id ?? "");
        }
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!accountId) {
      setError(t("negotiation.start.errorNoAccount"));
      return;
    }
    if (!toEmail.trim()) {
      setError(t("negotiation.start.errorNoRecipient"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const neg = await invoke<Negotiation>("tp_send_initial_request", {
        accountId,
        toEmail: toEmail.trim(),
        duration,
        latest: latest ? new Date(latest).toISOString() : null,
        preferredTime: preferredTime.trim() || null,
        minimumNotice: minimumNotice.trim() || null,
        summary: summary.trim() || null,
      });
      onSent(neg);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
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
          {t("negotiation.start.title")}
        </h3>

        <form className="flex flex-col gap-3" onSubmit={submit}>
          <Field label={t("negotiation.start.account")}>
            <select
              value={accountId}
              onChange={(e) => setAccountId(e.target.value)}
              className="w-full rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
              required
            >
              {accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.address}
                </option>
              ))}
            </select>
          </Field>

          <Field label={t("negotiation.start.to")}>
            <input
              type="email"
              value={toEmail}
              onChange={(e) => setToEmail(e.target.value)}
              placeholder="alice@example.com"
              className="w-full rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
              required
              autoFocus
            />
          </Field>

          <Field label={t("negotiation.start.duration")}>
            <select
              value={duration}
              onChange={(e) => setDuration(e.target.value)}
              className="w-full rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
            >
              {DURATION_PRESETS.map((p) => (
                <option key={p.iso} value={p.iso}>
                  {t(p.labelKey)}
                </option>
              ))}
            </select>
          </Field>

          <Field label={t("negotiation.start.summary")}>
            <input
              type="text"
              value={summary}
              onChange={(e) => setSummary(e.target.value)}
              placeholder={t("negotiation.start.summaryPlaceholder")}
              className="w-full rounded border px-2 py-1 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-base)",
              }}
            />
          </Field>

          <button
            type="button"
            onClick={() => setShowAdvanced((v) => !v)}
            className="self-start text-xs underline"
            style={{ color: "var(--fg-subtle)" }}
          >
            {showAdvanced
              ? t("negotiation.start.hideAdvanced")
              : t("negotiation.start.showAdvanced")}
          </button>

          {showAdvanced && (
            <div
              className="flex flex-col gap-2 rounded border px-3 py-2"
              style={{ borderColor: "var(--border-soft)" }}
            >
              <Field label={t("negotiation.start.latest")}>
                <input
                  type="datetime-local"
                  value={latest}
                  onChange={(e) => setLatest(e.target.value)}
                  className="w-full rounded border px-2 py-1 text-sm"
                  style={{
                    borderColor: "var(--border-soft)",
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                  }}
                />
              </Field>
              <Field label={t("negotiation.start.preferred")}>
                <input
                  type="text"
                  value={preferredTime}
                  onChange={(e) => setPreferredTime(e.target.value)}
                  placeholder={t("negotiation.start.preferredPlaceholder")}
                  className="w-full rounded border px-2 py-1 text-sm"
                  style={{
                    borderColor: "var(--border-soft)",
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                  }}
                />
              </Field>
              <Field label={t("negotiation.start.minimumNotice")}>
                <input
                  type="text"
                  value={minimumNotice}
                  onChange={(e) => setMinimumNotice(e.target.value)}
                  placeholder="PT2H"
                  className="w-full rounded border px-2 py-1 text-sm font-mono"
                  style={{
                    borderColor: "var(--border-soft)",
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                  }}
                />
                <span
                  className="mt-1 text-xs"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("negotiation.start.minimumNoticeHint")}
                </span>
              </Field>
            </div>
          )}

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

          <div className="mt-1 flex items-center justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              disabled={busy}
              className="rounded px-3 py-1 text-xs"
              style={{
                background: "var(--bg-soft)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-soft)",
              }}
            >
              {t("negotiation.start.cancel")}
            </button>
            <button
              type="submit"
              disabled={busy || !accountId}
              className="rounded px-3 py-1 text-xs font-medium disabled:opacity-50"
              style={{
                background: "var(--accent)",
                color: "#fff",
                border: "1px solid var(--border-soft)",
              }}
            >
              {busy
                ? t("negotiation.start.sending")
                : t("negotiation.start.send")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs" style={{ color: "var(--fg-muted)" }}>
        {label}
      </span>
      {children}
    </label>
  );
}
