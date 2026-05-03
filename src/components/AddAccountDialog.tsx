import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { RichEditor, type RichEditorHandle } from "./RichEditor";
import { stripHtmlToText } from "../utils/mailHtml";
import type {
  AccountSummary,
  AliasForm,
  DiscoveredFolders,
  NewAccountForm,
  SyncMode,
  UpdateAccountForm,
  VerboseReport,
} from "../types";

const htmlToPlain = (s: string) => stripHtmlToText(s);

type Props = {
  /** If set, dialog runs in edit mode pre-filled from this account. */
  initial?: AccountSummary;
  onSaved: (a: AccountSummary) => void;
  onDeleted?: (id: string) => void;
  onClose: () => void;
};

const DEFAULT_COLORS = [
  "#60a5fa",
  "#a78bfa",
  "#34d399",
  "#fbbf24",
  "#f472b6",
  "#f87171",
  "#22d3ee",
  "#facc15",
];

/** Lokaler Form-State: NewAccountForm-Shape plus zusätzliche Felder die
 *  in beiden Modi bearbeitbar sind (`syncMode`, `serverStoresSent`).
 *  Beim Submit wird je nach Modus das richtige Backend-Format gepackt. */
type FormState = Omit<NewAccountForm, "serverStoresSent" | "syncMode"> & {
  syncMode: SyncMode;
  serverStoresSent: boolean;
};

function initialFrom(a?: AccountSummary): FormState {
  if (!a) {
    return {
      displayName: "",
      address: "",
      fromName: "",
      color: DEFAULT_COLORS[0],
      signature: null,
      signatureHtml: null,
      imapHost: "",
      imapPort: 993,
      imapTls: true, // 993 = implicit
      smtpHost: "",
      smtpPort: 587,
      smtpTls: false, // 587 = STARTTLS (industry default)
      archiveFolder: "Archive",
      sentFolder: "Sent",
      draftsFolder: "Drafts",
      trashFolder: "Trash",
      spamFolder: "Spam",
      archiveOnReply: false,
      prefetchDays: 2,
      // Default-Werte fürs neue Konto. Beim Submit wird `serverStoresSent`
      // bewusst NICHT mitgeschickt (sondern als `null` Override), damit das
      // Backend per Probe-Mail entscheidet.
      syncMode: "idle",
      serverStoresSent: false,
      aliases: [],
      password: "",
    };
  }
  return {
    displayName: a.displayName,
    address: a.address,
    fromName: a.fromName,
    color: a.color,
    signature: a.signature,
    signatureHtml: a.signatureHtml,
    imapHost: a.imapHost,
    imapPort: a.imapPort,
    imapTls: a.imapTls,
    smtpHost: a.smtpHost,
    smtpPort: a.smtpPort,
    smtpTls: a.smtpTls,
    archiveFolder: a.archiveFolder,
    sentFolder: a.sentFolder,
    draftsFolder: a.draftsFolder,
    trashFolder: a.trashFolder,
    spamFolder: a.spamFolder,
    archiveOnReply: a.archiveOnReply,
    prefetchDays: a.prefetchDays,
    syncMode: a.syncMode,
    serverStoresSent: a.serverStoresSent,
    aliases: a.aliases.map((al) => ({
      email: al.email,
      fromName: al.fromName,
    })),
    password: "",
  };
}

export function AddAccountDialog({ initial, onSaved, onDeleted, onClose }: Props) {
  const { t } = useTranslation();
  const isEdit = !!initial;

  const [form, setForm] = useState<FormState>(() => initialFrom(initial));
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [report, setReport] = useState<VerboseReport | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [discovering, setDiscovering] = useState(false);
  const [discoveryInfo, setDiscoveryInfo] = useState<string | null>(null);
  const signatureRef = useRef<RichEditorHandle>(null);
  // Signature edit mode: "rich" uses the WYSIWYG editor, "html" exposes
  // raw HTML source for paste-from-clipboard workflows. We sync on every
  // mode switch so nothing the user typed gets lost.
  const [sigMode, setSigMode] = useState<"rich" | "html">("rich");
  const [sigHtmlSource, setSigHtmlSource] = useState<string>(
    initial?.signatureHtml ?? "",
  );

  const derivedDefaults = useMemo(() => {
    const at = form.address.indexOf("@");
    if (at < 0 || at === form.address.length - 1) return null;
    const domain = form.address.slice(at + 1).trim();
    if (!domain) return null;
    return { imapHost: `imap.${domain}`, smtpHost: `smtp.${domain}` };
  }, [form.address]);

  const patch = (p: Partial<FormState>) => setForm((f) => ({ ...f, ...p }));

  const maybeAutofill = () => {
    if (!derivedDefaults || isEdit) return;
    patch({
      imapHost: form.imapHost || derivedDefaults.imapHost,
      smtpHost: form.smtpHost || derivedDefaults.smtpHost,
      fromName: form.fromName || form.displayName,
    });
  };

  // Well-known ports map directly to a TLS variant. Nudging the flag when
  // the user changes the port catches the most common misconfig: 587 with
  // implicit TLS (should be STARTTLS) → lettre otherwise reports a cryptic
  // "corrupt message of type InvalidContentType" from the server.
  const tlsForPort = (port: number): boolean | null => {
    if (port === 465 || port === 993 || port === 995) return true;
    if (port === 587 || port === 143 || port === 110 || port === 25) return false;
    return null;
  };
  const onImapPortBlur = () => {
    const inferred = tlsForPort(form.imapPort);
    if (inferred !== null && inferred !== form.imapTls) patch({ imapTls: inferred });
  };
  const onSmtpPortBlur = () => {
    const inferred = tlsForPort(form.smtpPort);
    if (inferred !== null && inferred !== form.smtpTls) patch({ smtpTls: inferred });
  };

  const runVerboseTest = async () => {
    setTesting(true);
    setReport(null);
    setSaveError(null);
    try {
      // In edit mode an empty password means "use the stored one" — but the
      // test command takes a plaintext arg. We can't reach into the keyring
      // from the frontend, so the UI requires a fresh password for the test
      // when editing. This matches SparkMail's behavior.
      if (!form.password) {
        setReport({
          ok: false,
          totalMs: 0,
          steps: [
            {
              elapsedMs: 0,
              kind: "err",
              message:
                "Zum Testen bitte Passwort eingeben (wird dann auch gespeichert).",
            },
          ],
        });
        return;
      }
      const r = await invoke<VerboseReport>("test_imap_verbose", {
        host: form.imapHost,
        port: form.imapPort,
        user: form.address,
        password: form.password,
      });
      setReport(r);
    } catch (e) {
      setReport({
        ok: false,
        totalMs: 0,
        steps: [{ elapsedMs: 0, kind: "err", message: String(e) }],
      });
    } finally {
      setTesting(false);
    }
  };

  const save = async (skipTest: boolean) => {
    setSaving(true);
    setSaveError(null);
    try {
      // Pull latest HTML out of whichever editor mode the user is in:
      // in rich mode it's the live contentEditable DOM, in html mode it's
      // the raw source textarea state.
      const sigHtml =
        sigMode === "rich"
          ? signatureRef.current?.getHtml() ?? ""
          : sigHtmlSource;
      const sigPlain =
        sigMode === "rich"
          ? signatureRef.current?.getText() ?? ""
          : htmlToPlain(sigHtmlSource);
      const snapshot = {
        ...form,
        signatureHtml: sigHtml.trim().length > 0 ? sigHtml : null,
        signature: sigPlain.trim().length > 0 ? sigPlain : null,
      };
      if (isEdit && initial) {
        const payload: UpdateAccountForm = {
          id: initial.id,
          displayName: snapshot.displayName,
          address: snapshot.address,
          fromName: snapshot.fromName,
          color: snapshot.color,
          signature: snapshot.signature,
          signatureHtml: snapshot.signatureHtml,
          imapHost: snapshot.imapHost,
          imapPort: snapshot.imapPort,
          imapTls: snapshot.imapTls,
          smtpHost: snapshot.smtpHost,
          smtpPort: snapshot.smtpPort,
          smtpTls: snapshot.smtpTls,
          archiveFolder: snapshot.archiveFolder,
          sentFolder: snapshot.sentFolder,
          draftsFolder: snapshot.draftsFolder,
          trashFolder: snapshot.trashFolder,
          spamFolder: snapshot.spamFolder,
          archiveOnReply: snapshot.archiveOnReply,
          prefetchDays: snapshot.prefetchDays,
          syncMode: snapshot.syncMode,
          serverStoresSent: snapshot.serverStoresSent,
          aliases: snapshot.aliases,
          password: snapshot.password || null,
          skipTest,
        };
        const saved = await invoke<AccountSummary>("update_account", {
          form: payload,
        });
        onSaved(saved);
      } else {
        // Beim Anlegen schicken wir KEINEN expliziten `serverStoresSent`-
        // Wert mit (= null/undefined Override) → Backend führt nach dem
        // Login-Test eine Probe-Mail durch und ermittelt das Verhalten
        // automatisch. Der lokale Form-State `serverStoresSent` ist dabei
        // irrelevant; das Backend antwortet mit dem ermittelten Wert,
        // den der User danach in den Settings ändern kann.
        const newPayload: NewAccountForm = {
          displayName: snapshot.displayName,
          address: snapshot.address,
          fromName: snapshot.fromName,
          color: snapshot.color,
          signature: snapshot.signature,
          signatureHtml: snapshot.signatureHtml,
          imapHost: snapshot.imapHost,
          imapPort: snapshot.imapPort,
          imapTls: snapshot.imapTls,
          smtpHost: snapshot.smtpHost,
          smtpPort: snapshot.smtpPort,
          smtpTls: snapshot.smtpTls,
          archiveFolder: snapshot.archiveFolder,
          sentFolder: snapshot.sentFolder,
          draftsFolder: snapshot.draftsFolder,
          trashFolder: snapshot.trashFolder,
          spamFolder: snapshot.spamFolder,
          archiveOnReply: snapshot.archiveOnReply,
          prefetchDays: snapshot.prefetchDays,
          syncMode: snapshot.syncMode,
          // serverStoresSent bewusst weggelassen → Backend probt
          aliases: snapshot.aliases,
          password: snapshot.password,
          skipTest,
        };
        const saved = await invoke<AccountSummary>("add_account", {
          form: newPayload,
        });
        onSaved(saved);
      }
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const discoverFolders = async () => {
    setDiscovering(true);
    setDiscoveryInfo(null);
    setSaveError(null);
    try {
      const result = await invoke<DiscoveredFolders>("discover_folders", {
        host: form.imapHost,
        port: form.imapPort,
        user: form.address,
        password: form.password,
        accountId: initial?.id ?? null,
      });
      const next: Partial<FormState> = {};
      if (result.archive) next.archiveFolder = result.archive;
      if (result.sent) next.sentFolder = result.sent;
      if (result.drafts) next.draftsFolder = result.drafts;
      if (result.trash) next.trashFolder = result.trash;
      if (result.spam) next.spamFolder = result.spam;
      patch(next);
      const hits = [
        result.archive && "Archiv",
        result.sent && "Gesendet",
        result.drafts && "Entwürfe",
        result.trash && "Papierkorb",
        result.spam && "Spam",
      ].filter(Boolean) as string[];
      setDiscoveryInfo(
        hits.length > 0
          ? t("accounts.discoveryFound", {
              hits: hits.join(", "),
              total: result.all.length,
            })
          : t("accounts.discoveryNone", { total: result.all.length }),
      );
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setDiscovering(false);
    }
  };

  const addAlias = () =>
    patch({ aliases: [...form.aliases, { email: "", fromName: "" }] });
  const removeAlias = (idx: number) =>
    patch({ aliases: form.aliases.filter((_, i) => i !== idx) });
  const patchAlias = (idx: number, p: Partial<AliasForm>) =>
    patch({
      aliases: form.aliases.map((a, i) => (i === idx ? { ...a, ...p } : a)),
    });

  const del = async () => {
    if (!initial || !onDeleted) return;
    const ok = window.confirm(
      t("accounts.deleteConfirm", { name: initial.displayName }),
    );
    if (!ok) return;
    setDeleting(true);
    setSaveError(null);
    try {
      await invoke("delete_account", { id: initial.id });
      onDeleted(initial.id);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setDeleting(false);
    }
  };

  const onSubmit = (ev: React.FormEvent) => {
    ev.preventDefault();
    void save(false);
  };

  const passwordRequired = !isEdit;
  const passwordHint = isEdit
    ? t("accounts.passwordEditHint")
    : t("accounts.passwordHint");

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center px-4"
      style={{ background: "rgba(0,0,0,0.45)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <form
        onSubmit={onSubmit}
        className="max-h-[90vh] w-full max-w-lg overflow-y-auto rounded-xl border p-5 shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <h2 className="mb-4 text-lg font-semibold">
          {isEdit ? t("accounts.editDialogTitle") : t("accounts.dialogTitle")}
        </h2>

        <Section title={t("accounts.identity")}>
          <Field label={t("accounts.displayName")} hint={t("accounts.displayNameHint")}>
            <input
              required
              value={form.displayName}
              onChange={(e) => patch({ displayName: e.target.value })}
              className={inputCls}
            />
          </Field>
          <Row>
            <Field label={t("accounts.address")}>
              <input
                required
                type="email"
                value={form.address}
                onChange={(e) => patch({ address: e.target.value })}
                onBlur={maybeAutofill}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.fromName")} hint={t("accounts.fromNameHint")}>
              <input
                required
                value={form.fromName}
                onChange={(e) => patch({ fromName: e.target.value })}
                className={inputCls}
              />
            </Field>
          </Row>
          <Field label={t("accounts.color")}>
            <div className="flex flex-wrap gap-2 pt-1">
              {DEFAULT_COLORS.map((c) => (
                <button
                  key={c}
                  type="button"
                  onClick={() => patch({ color: c })}
                  aria-label={c}
                  className="h-6 w-6 rounded-full transition-transform hover:scale-110"
                  style={{
                    background: c,
                    outline: form.color === c ? "2px solid var(--fg-base)" : "none",
                    outlineOffset: "2px",
                  }}
                />
              ))}
            </div>
          </Field>
        </Section>

        <Section title={t("accounts.server")}>
          <div className="grid grid-cols-[1fr_90px_130px] gap-3">
            <Field label={t("accounts.imapHost")}>
              <input
                required
                value={form.imapHost}
                onChange={(e) => patch({ imapHost: e.target.value })}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.imapPort")}>
              <input
                required
                type="number"
                value={form.imapPort}
                onChange={(e) => patch({ imapPort: Number(e.target.value) })}
                onBlur={onImapPortBlur}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.tls")}>
              <TlsSelect
                value={form.imapTls}
                onChange={(v) => patch({ imapTls: v })}
              />
            </Field>
          </div>

          <div className="grid grid-cols-[1fr_90px_130px] gap-3">
            <Field label={t("accounts.smtpHost")}>
              <input
                required
                value={form.smtpHost}
                onChange={(e) => patch({ smtpHost: e.target.value })}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.smtpPort")}>
              <input
                required
                type="number"
                value={form.smtpPort}
                onChange={(e) => patch({ smtpPort: Number(e.target.value) })}
                onBlur={onSmtpPortBlur}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.tls")}>
              <TlsSelect
                value={form.smtpTls}
                onChange={(v) => patch({ smtpTls: v })}
              />
            </Field>
          </div>
          <Field label={t("accounts.password")} hint={passwordHint}>
            <input
              required={passwordRequired}
              type="password"
              autoComplete="new-password"
              value={form.password}
              onChange={(e) => patch({ password: e.target.value })}
              className={inputCls}
            />
          </Field>
        </Section>

        <Section title={t("accounts.foldersSection")}>
          <div className="flex items-center justify-between gap-2">
            <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
              {t("accounts.foldersHint")}
            </p>
            <button
              type="button"
              onClick={discoverFolders}
              disabled={discovering || !form.imapHost || (!form.password && !initial)}
              className="rounded-md border px-2 py-1 text-[11px] disabled:opacity-50"
              style={{
                borderColor: "var(--border-base)",
                color: "var(--fg-base)",
              }}
            >
              {discovering
                ? t("accounts.discoveryRunning")
                : t("accounts.discoveryButton")}
            </button>
          </div>
          {discoveryInfo && (
            <p className="text-[11px]" style={{ color: "var(--fg-muted)" }}>
              {discoveryInfo}
            </p>
          )}
          <div className="grid grid-cols-2 gap-3">
            <Field label={t("accounts.archiveFolder")}>
              <input
                required
                value={form.archiveFolder}
                onChange={(e) => patch({ archiveFolder: e.target.value })}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.sentFolder")}>
              <input
                required
                value={form.sentFolder}
                onChange={(e) => patch({ sentFolder: e.target.value })}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.draftsFolder")}>
              <input
                required
                value={form.draftsFolder}
                onChange={(e) => patch({ draftsFolder: e.target.value })}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.trashFolder")}>
              <input
                required
                value={form.trashFolder}
                onChange={(e) => patch({ trashFolder: e.target.value })}
                className={inputCls}
              />
            </Field>
            <Field label={t("accounts.spamFolder")}>
              <input
                required
                value={form.spamFolder}
                onChange={(e) => patch({ spamFolder: e.target.value })}
                className={inputCls}
              />
            </Field>
          </div>

          {/* Workflow toggle lives in the same section as the archive-folder
              config — this setting is only meaningful in relation to that
              folder. */}
          <label
            className="mt-1 flex items-start gap-2 rounded-md border px-3 py-2 text-sm"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
            }}
          >
            <input
              type="checkbox"
              checked={form.archiveOnReply}
              onChange={(e) => patch({ archiveOnReply: e.target.checked })}
              className="mt-0.5"
            />
            <span className="flex flex-col">
              <span style={{ color: "var(--fg-base)" }}>
                {t("accounts.archiveOnReply")}
              </span>
              <span
                className="text-[11px]"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("accounts.archiveOnReplyHint")}
              </span>
            </span>
          </label>

          {/* Server-Auto-Save-Toggle. Im Edit-Modus zeigen wir den
              Wert wie er von der Probe-Mail gesetzt wurde — der User
              kann ihn manuell überschreiben falls Probe falsch lag.
              Im Anlage-Modus blenden wir das aus: dort übernimmt der
              Backend-Probe-Pfad das automatisch. */}
          {isEdit ? (
            <label
              className="mt-1 flex items-start gap-2 rounded-md border px-3 py-2 text-sm"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
              }}
            >
              <input
                type="checkbox"
                checked={form.serverStoresSent}
                onChange={(e) => patch({ serverStoresSent: e.target.checked })}
                className="mt-0.5"
              />
              <span className="flex flex-col">
                <span style={{ color: "var(--fg-base)" }}>
                  {t("accounts.serverStoresSent")}
                </span>
                <span
                  className="text-[11px]"
                  style={{ color: "var(--fg-subtle)" }}
                >
                  {t("accounts.serverStoresSentHint")}
                </span>
              </span>
            </label>
          ) : (
            <p
              className="mt-1 text-[11px] rounded-md border px-3 py-2"
              style={{
                borderColor: "var(--border-soft)",
                background: "var(--bg-base)",
                color: "var(--fg-subtle)",
              }}
            >
              {t("accounts.serverStoresSentProbeNotice")}
            </p>
          )}

          {/* Prefetch window. Numeric field rather than a checkbox because
              "how aggressive" is the interesting dimension — 0 disables,
              high values trade bandwidth for responsiveness. */}
          <div
            className="mt-1 flex items-start gap-3 rounded-md border px-3 py-2 text-sm"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
            }}
          >
            <div className="flex flex-col">
              <span style={{ color: "var(--fg-base)" }}>
                {t("accounts.prefetchDays")}
              </span>
              <span
                className="text-[11px]"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("accounts.prefetchDaysHint")}
              </span>
            </div>
            <input
              type="number"
              min={0}
              max={365}
              value={form.prefetchDays}
              onChange={(e) =>
                patch({
                  prefetchDays: Math.max(
                    0,
                    Math.min(365, Number(e.target.value) || 0),
                  ),
                })
              }
              className="ml-auto w-20 rounded-md px-2 py-1 text-right text-sm outline-none"
              style={{
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-base)",
              }}
            />
          </div>

          {/* Sync-Modus pro Konto. Default `idle` deckt 95% der Provider ab —
              Push-Sofortbenachrichtigung über IMAP-IDLE. Wenn ein Server
              IDLE nicht zuverlässig liefert (selten, aber kommt vor),
              kann der User hier auf reines Polling oder den
              Belt-and-Suspenders-Modus IDLE+Polling wechseln. */}
          <div
            className="mt-1 flex items-start gap-3 rounded-md border px-3 py-2 text-sm"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
            }}
          >
            <div className="flex flex-1 flex-col">
              <span style={{ color: "var(--fg-base)" }}>
                {t("accounts.syncMode")}
              </span>
              <span
                className="text-[11px]"
                style={{ color: "var(--fg-subtle)" }}
              >
                {t("accounts.syncModeHint")}
              </span>
            </div>
            <select
              value={form.syncMode}
              onChange={(e) =>
                patch({ syncMode: e.target.value as SyncMode })
              }
              className="ml-auto rounded-md px-2 py-1 text-sm outline-none"
              style={{
                background: "var(--bg-panel)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-base)",
              }}
            >
              <option value="idle">{t("accounts.syncModeIdle")}</option>
              <option value="polling">
                {t("accounts.syncModePolling")}
              </option>
              <option value="idle_and_polling">
                {t("accounts.syncModeIdleAndPolling")}
              </option>
            </select>
          </div>
        </Section>

        <Section title={t("accounts.aliasesSection")}>
          <p className="-mt-1 text-[11px]" style={{ color: "var(--fg-subtle)" }}>
            {t("accounts.aliasesHint")}
          </p>
          {form.aliases.length === 0 ? (
            <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
              {t("accounts.aliasesEmpty")}
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {form.aliases.map((al, idx) => (
                <li key={idx} className="grid grid-cols-[1fr_1fr_auto] gap-2">
                  <input
                    type="email"
                    placeholder={t("accounts.aliasEmailPlaceholder")}
                    value={al.email}
                    onChange={(e) => patchAlias(idx, { email: e.target.value })}
                    className={inputCls}
                  />
                  <input
                    placeholder={t("accounts.aliasFromNamePlaceholder")}
                    value={al.fromName}
                    onChange={(e) =>
                      patchAlias(idx, { fromName: e.target.value })
                    }
                    className={inputCls}
                  />
                  <button
                    type="button"
                    onClick={() => removeAlias(idx)}
                    aria-label={t("accounts.aliasRemove")}
                    className="rounded-md border px-2 text-sm"
                    style={{
                      borderColor: "var(--border-base)",
                      color: "var(--fg-muted)",
                    }}
                  >
                    ✕
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            onClick={addAlias}
            className="self-start rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
          >
            {t("accounts.aliasAdd")}
          </button>
        </Section>

        <Section title={t("accounts.signatureSection")}>
          <div className="flex items-center justify-between gap-2">
            <p className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
              {sigMode === "rich"
                ? t("accounts.signatureHintRich")
                : t("accounts.signatureHintHtml")}
            </p>
            <div
              className="inline-flex rounded-md border p-0.5 text-[11px]"
              style={{ borderColor: "var(--border-base)" }}
            >
              <button
                type="button"
                onClick={() => {
                  if (sigMode === "html") {
                    // Switching back to rich: seed the editor with the raw
                    // HTML the user just typed. RichEditor is uncontrolled
                    // so we nudge it via key — remount by bumping the key.
                    setSigMode("rich");
                  }
                }}
                className="rounded px-2 py-0.5"
                style={{
                  background:
                    sigMode === "rich" ? "var(--bg-selected)" : "transparent",
                  color:
                    sigMode === "rich" ? "var(--accent)" : "var(--fg-muted)",
                }}
              >
                {t("accounts.signatureModeRich")}
              </button>
              <button
                type="button"
                onClick={() => {
                  if (sigMode === "rich") {
                    // Capture the WYSIWYG contents into the raw source
                    // before we stop rendering the rich editor.
                    const current = signatureRef.current?.getHtml() ?? "";
                    setSigHtmlSource(current);
                    setSigMode("html");
                  }
                }}
                className="rounded px-2 py-0.5"
                style={{
                  background:
                    sigMode === "html" ? "var(--bg-selected)" : "transparent",
                  color:
                    sigMode === "html" ? "var(--accent)" : "var(--fg-muted)",
                }}
              >
                {t("accounts.signatureModeHtml")}
              </button>
            </div>
          </div>

          {sigMode === "rich" ? (
            <RichEditor
              // Remount when the HTML source changes externally so a paste
              // in HTML mode followed by switching back to rich actually
              // picks up the new content.
              key={sigHtmlSource.length}
              ref={signatureRef}
              initialHtml={sigHtmlSource || form.signatureHtml || ""}
              placeholder={t("accounts.signaturePlaceholder")}
              minHeight={140}
            />
          ) : (
            <textarea
              value={sigHtmlSource}
              onChange={(e) => setSigHtmlSource(e.target.value)}
              rows={8}
              spellCheck={false}
              placeholder="<div style=…>Ihre HTML-Signatur</div>"
              className="w-full resize-vertical rounded-md px-3 py-2 font-mono text-[12px] outline-none"
              style={{
                background: "var(--bg-base)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-base)",
                minHeight: "160px",
              }}
            />
          )}
        </Section>

        {report && <VerboseLog report={report} t={t} />}

        {saveError && (
          <div
            className="mb-3 rounded-md px-3 py-2 text-xs"
            style={{
              background: "rgba(248,113,113,0.12)",
              color: "#ef4444",
              border: "1px solid rgba(248,113,113,0.25)",
            }}
          >
            {saveError}
          </div>
        )}

        <div className="mt-4 flex flex-wrap items-center justify-between gap-2">
          <div className="flex gap-2">
            <button
              type="button"
              onClick={onClose}
              className="rounded-md px-3 py-1.5 text-sm"
              style={{ color: "var(--fg-muted)" }}
            >
              {t("accounts.cancel")}
            </button>
            {isEdit && onDeleted && (
              <button
                type="button"
                onClick={del}
                disabled={deleting}
                className="rounded-md px-3 py-1.5 text-sm disabled:opacity-50"
                style={{ color: "#ef4444" }}
              >
                {deleting ? t("accounts.saving") : t("accounts.delete")}
              </button>
            )}
          </div>
          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              onClick={runVerboseTest}
              disabled={testing || !form.imapHost || !form.password}
              className="rounded-md border px-3 py-1.5 text-sm disabled:opacity-50"
              style={{
                borderColor: "var(--border-base)",
                color: "var(--fg-base)",
              }}
            >
              {testing ? t("accounts.testing") : t("accounts.testConnection")}
            </button>
            <button
              type="button"
              onClick={() => void save(true)}
              disabled={saving}
              className="rounded-md border px-3 py-1.5 text-sm disabled:opacity-50"
              style={{
                borderColor: "var(--border-base)",
                color: "var(--fg-muted)",
              }}
            >
              {t("accounts.saveDraft")}
            </button>
            <button
              type="submit"
              disabled={saving}
              className="rounded-md px-3 py-1.5 text-sm font-medium disabled:opacity-50"
              style={{ background: "var(--accent)", color: "white" }}
            >
              {saving
                ? // Beim Anlegen läuft zusätzlich eine Test-Mail-Probe → eigener
                  // Label-Text damit der User weiß warum es ~10s dauert.
                  isEdit
                  ? t("accounts.saving")
                  : t("accounts.savingWithProbe")
                : t("accounts.save")}
            </button>
          </div>
        </div>
      </form>
    </div>
  );
}

function VerboseLog({
  report,
  t,
}: {
  report: VerboseReport;
  t: (k: string, o?: Record<string, unknown>) => string;
}) {
  const summary = report.ok
    ? t("accounts.testPassed", { ms: report.totalMs })
    : t("accounts.testFailed", { ms: report.totalMs });
  const summaryColor = report.ok ? "#10b981" : "#ef4444";

  return (
    <div
      className="mb-3 rounded-md border px-3 py-2 text-xs font-mono"
      style={{
        background: "var(--bg-base)",
        borderColor: "var(--border-base)",
        color: "var(--fg-base)",
      }}
    >
      <div style={{ color: summaryColor, fontWeight: 600 }} className="mb-1">
        {summary}
      </div>
      <ul className="flex flex-col gap-0.5">
        {report.steps.map((s, i) => (
          <li key={i} className="flex gap-2 leading-tight">
            <span
              className="w-10 shrink-0 text-right"
              style={{ color: "var(--fg-subtle)" }}
            >
              {s.elapsedMs}ms
            </span>
            <span
              className="w-3 shrink-0 text-center"
              style={{
                color:
                  s.kind === "ok"
                    ? "#10b981"
                    : s.kind === "err"
                      ? "#ef4444"
                      : "var(--fg-muted)",
              }}
            >
              {s.kind === "ok" ? "✓" : s.kind === "err" ? "✗" : "•"}
            </span>
            <span
              className="min-w-0 break-words"
              style={{ color: "var(--fg-base)" }}
            >
              {s.message}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

const inputCls =
  "w-full rounded-md px-2.5 py-1.5 text-sm outline-none focus:ring-2";

function TlsSelect({
  value,
  onChange,
}: {
  value: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <select
      value={value ? "implicit" : "starttls"}
      onChange={(e) => onChange(e.target.value === "implicit")}
      className="w-full rounded-md px-2 py-1.5 text-sm"
      style={{
        background: "var(--bg-base)",
        color: "var(--fg-base)",
        border: "1px solid var(--border-base)",
      }}
    >
      <option value="implicit">Implicit TLS</option>
      <option value="starttls">STARTTLS</option>
    </select>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mb-4">
      <div
        className="mb-2 text-[11px] uppercase tracking-[0.15em]"
        style={{ color: "var(--fg-subtle)" }}
      >
        {title}
      </div>
      <div className="flex flex-col gap-3">{children}</div>
    </section>
  );
}

function Row({ children }: { children: React.ReactNode }) {
  return <div className="grid grid-cols-[1fr_auto] gap-3">{children}</div>;
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block">
      <span
        className="mb-1 block text-xs"
        style={{ color: "var(--fg-muted)" }}
      >
        {label}
      </span>
      <div className="[&>input]:bg-[var(--bg-base)] [&>input]:text-[var(--fg-base)] [&>input]:border [&>input]:border-[var(--border-base)]">
        {children}
      </div>
      {hint && (
        <span
          className="mt-1 block text-[11px]"
          style={{ color: "var(--fg-subtle)" }}
        >
          {hint}
        </span>
      )}
    </label>
  );
}
