import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { TagEditor } from "./TagEditor";
import type {
  ComposeDraft,
  ContactDetail as ContactDetailType,
  ContactForm,
  EnvelopeSummary,
} from "../types";

type Props = {
  contactId: string | undefined;
  /** Bei "create new" ist `contactId` undefined — dann zeigen wir
   *  einen leeren Form-State im Editor-Modus. */
  newMode?: boolean;
  onSaved: (id: string) => void;
  onDeleted: () => void;
  onCancel: () => void;
  /** Klick auf "Mail schreiben" → öffnet Compose mit primary_email
   *  vorbefüllt. */
  onCompose: (draft: ComposeDraft) => void;
  /** Klick auf eine Mail-Zeile in der "Letzte Mails"-Liste — Caller
   *  springt aus dem Kontakte-Mode zurück in die Mail-Ansicht und
   *  öffnet den Envelope. */
  onOpenMessage: (messageId: string) => void;
};

const EMPTY_FORM: ContactForm = {
  displayName: "",
  organization: null,
  jobTitle: null,
  phone: null,
  mobile: null,
  street: null,
  zip: null,
  city: null,
  country: null,
  website: null,
  notes: "",
  pinned: false,
};

function formFromDetail(d: ContactDetailType): ContactForm {
  return {
    displayName: d.displayName,
    organization: d.organization,
    jobTitle: d.jobTitle,
    phone: d.phone,
    mobile: d.mobile,
    street: d.street,
    zip: d.zip,
    city: d.city,
    country: d.country,
    website: d.website,
    notes: d.notes,
    pinned: d.pinned,
  };
}

export function ContactDetail({
  contactId,
  newMode,
  onSaved,
  onDeleted,
  onCancel,
  onCompose,
  onOpenMessage,
}: Props) {
  const { t } = useTranslation();
  const [detail, setDetail] = useState<ContactDetailType | null>(null);
  const [form, setForm] = useState<ContactForm>(EMPTY_FORM);
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [messages, setMessages] = useState<EnvelopeSummary[]>([]);
  const [extracting, setExtracting] = useState(false);
  // E-Mail-Adresse-Hinzufügen-Modal — ersetzt das hässliche
  // window.prompt mit einem app-stiligen Dialog.
  const [addEmailOpen, setAddEmailOpen] = useState(false);

  // ── Load on contact change ─────────────────────────────────────
  const load = useCallback(async () => {
    if (newMode) {
      setDetail(null);
      setForm(EMPTY_FORM);
      setEditing(true);
      setMessages([]);
      return;
    }
    if (!contactId) {
      setDetail(null);
      setMessages([]);
      return;
    }
    setError(null);
    try {
      const d = await invoke<ContactDetailType>("get_contact", { contactId });
      setDetail(d);
      setForm(formFromDetail(d));
      setEditing(false);
      const m = await invoke<EnvelopeSummary[]>("list_messages_for_contact", {
        contactId,
        limit: 50,
        offset: 0,
      });
      setMessages(m);
    } catch (e) {
      setError(String(e));
    }
  }, [contactId, newMode]);

  useEffect(() => {
    void load();
  }, [load]);

  const patch = (p: Partial<ContactForm>) => setForm((f) => ({ ...f, ...p }));

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      if (newMode || !detail) {
        const created = await invoke<ContactDetailType>("create_contact", {
          form,
          initialEmail: null,
        });
        onSaved(created.id);
      } else {
        const updated = await invoke<ContactDetailType>("update_contact", {
          contactId: detail.id,
          form,
        });
        setDetail(updated);
        setForm(formFromDetail(updated));
        setEditing(false);
        onSaved(updated.id);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const del = async () => {
    if (!detail) return;
    if (!window.confirm(t("contacts.confirmDelete", { name: detail.displayName })))
      return;
    try {
      await invoke("delete_contact", { contactId: detail.id });
      onDeleted();
    } catch (e) {
      setError(String(e));
    }
  };

  /** Öffnet den eigenen Modal-Dialog statt eines `window.prompt` —
   *  letzteres rendert nativ vom Webview-Host und sieht in der App
   *  wie ein Fremdkörper aus ("localhost:14210 enthält…"). */
  const openAddEmail = () => {
    if (!detail) return;
    setAddEmailOpen(true);
  };

  /** Wird vom Modal nach Validierung gerufen. */
  const submitAddEmail = async (raw: string) => {
    if (!detail) return;
    const email = raw.trim();
    if (!email) return;
    try {
      const updated = await invoke<ContactDetailType>("add_contact_email", {
        contactId: detail.id,
        email,
        isPrimary: detail.emails.length === 0,
      });
      setDetail(updated);
      setAddEmailOpen(false);
    } catch (e) {
      setError(String(e));
    }
  };

  const removeEmail = async (email: string) => {
    if (!detail) return;
    if (!window.confirm(t("contacts.confirmRemoveEmail", { email }))) return;
    try {
      const updated = await invoke<ContactDetailType>("remove_contact_email", {
        contactId: detail.id,
        email,
      });
      setDetail(updated);
    } catch (e) {
      setError(String(e));
    }
  };

  const setPrimary = async (email: string) => {
    if (!detail) return;
    try {
      const updated = await invoke<ContactDetailType>(
        "set_primary_contact_email",
        { contactId: detail.id, email },
      );
      setDetail(updated);
    } catch (e) {
      setError(String(e));
    }
  };

  const onTagsChange = async (tagIds: string[]) => {
    if (!detail) return;
    try {
      const updated = await invoke<ContactDetailType>("set_contact_tags", {
        contactId: detail.id,
        tagIds,
      });
      setDetail(updated);
    } catch (e) {
      setError(String(e));
    }
  };

  const composeMail = () => {
    if (!detail) return;
    const primary = detail.emails.find((e) => e.isPrimary) ?? detail.emails[0];
    if (!primary) {
      setError(t("contacts.noEmailToSendTo"));
      return;
    }
    const formatted = detail.displayName
      ? `${detail.displayName} <${primary.email}>`
      : primary.email;
    onCompose({
      to: `${formatted}, `,
      cc: "",
      bcc: "",
      subject: "",
      body: "",
    });
  };

  /** Manueller Re-Extract-Trigger für extracted-origin contacts mit
   *  neuerer Mail seit der letzten Extraktion. Greifen über
   *  list_messages, der dort vorhandene jüngste Eintrag wird als
   *  Anchor genommen. */
  const reExtract = async () => {
    if (!detail || messages.length === 0) return;
    setExtracting(true);
    setError(null);
    try {
      const newest = messages[0];
      await invoke("extract_contact_from_message", {
        messageId: newest.id,
      });
      // Nach Re-Extract DB neu lesen — Backend hat ggf. die Felder
      // aktualisiert.
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setExtracting(false);
    }
  };

  if (!newMode && !detail && !error) {
    return (
      <div
        className="flex h-full items-center justify-center px-6 text-sm"
        style={{ color: "var(--fg-subtle)" }}
      >
        {t("contacts.noSelection")}
      </div>
    );
  }

  return (
    <div
      className="flex h-full flex-col"
      style={{ background: "var(--bg-panel)" }}
    >
      <header
        className="flex items-center justify-between gap-2 border-b px-4 py-2"
        style={{ borderColor: "var(--border-base)" }}
      >
        <h2 className="truncate text-sm font-semibold">
          {newMode
            ? t("contacts.newTitle")
            : detail?.displayName ?? t("contacts.unnamed")}
        </h2>
        <div className="flex shrink-0 gap-2">
          {!editing && detail && (
            <>
              <button
                type="button"
                onClick={composeMail}
                className="rounded-md border px-2 py-0.5 text-xs"
                style={{
                  borderColor: "var(--border-base)",
                  color: "var(--fg-base)",
                }}
              >
                {t("contacts.composeMail")}
              </button>
              <button
                type="button"
                onClick={() => setEditing(true)}
                className="rounded-md border px-2 py-0.5 text-xs"
                style={{
                  borderColor: "var(--border-base)",
                  color: "var(--fg-base)",
                }}
              >
                {t("contacts.edit")}
              </button>
              {detail.origin === "extracted" && (
                <button
                  type="button"
                  onClick={reExtract}
                  disabled={extracting || messages.length === 0}
                  className="rounded-md border px-2 py-0.5 text-xs disabled:opacity-50"
                  style={{
                    borderColor: "var(--border-base)",
                    color: "var(--fg-muted)",
                  }}
                  title={t("contacts.reExtractHint")}
                >
                  {extracting ? t("contacts.extracting") : t("contacts.reExtract")}
                </button>
              )}
              <button
                type="button"
                onClick={del}
                className="rounded-md px-2 py-0.5 text-xs"
                style={{ color: "#ef4444" }}
              >
                {t("contacts.delete")}
              </button>
            </>
          )}
          {editing && (
            <>
              <button
                type="button"
                onClick={() => {
                  if (newMode) {
                    onCancel();
                  } else if (detail) {
                    setForm(formFromDetail(detail));
                    setEditing(false);
                  }
                }}
                className="rounded-md px-2 py-0.5 text-xs"
                style={{ color: "var(--fg-muted)" }}
              >
                {t("contacts.cancel")}
              </button>
              <button
                type="button"
                onClick={save}
                disabled={saving || !form.displayName.trim()}
                className="rounded-md px-3 py-0.5 text-xs font-medium disabled:opacity-50"
                style={{ background: "var(--accent)", color: "white" }}
              >
                {saving ? t("contacts.saving") : t("contacts.save")}
              </button>
            </>
          )}
        </div>
      </header>

      {error && (
        <div
          className="px-4 py-2 text-xs"
          style={{
            background: "rgba(248,113,113,0.12)",
            color: "#ef4444",
          }}
        >
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto px-4 py-3">
        {editing ? (
          <ContactFormFields form={form} onChange={patch} />
        ) : detail ? (
          <ContactReadOnly detail={detail} />
        ) : null}

        {detail && !editing && (
          <>
            <Section title={t("contacts.tagsSection")}>
              <TagEditor tags={detail.tags} onChange={onTagsChange} />
            </Section>

            <Section title={t("contacts.emailsSection")}>
              <ul className="flex flex-col gap-1">
                {detail.emails.map((e) => (
                  <li
                    key={e.id}
                    className="flex items-center gap-2 text-sm"
                  >
                    <span
                      className="flex-1 truncate"
                      style={{ color: "var(--fg-base)" }}
                    >
                      {e.email}
                    </span>
                    {e.isPrimary && (
                      <span
                        className="rounded px-1 text-[10px]"
                        style={{
                          background: "var(--bg-hover)",
                          color: "var(--accent)",
                        }}
                      >
                        {t("contacts.primary")}
                      </span>
                    )}
                    {!e.isPrimary && (
                      <button
                        type="button"
                        onClick={() => setPrimary(e.email)}
                        className="text-[10px] underline"
                        style={{ color: "var(--fg-muted)" }}
                      >
                        {t("contacts.makePrimary")}
                      </button>
                    )}
                    <button
                      type="button"
                      onClick={() => removeEmail(e.email)}
                      className="text-[10px]"
                      style={{ color: "var(--fg-subtle)" }}
                      aria-label={t("contacts.removeEmail")}
                    >
                      ✕
                    </button>
                  </li>
                ))}
              </ul>
              <button
                type="button"
                onClick={openAddEmail}
                className="mt-2 self-start rounded-md border px-2 py-0.5 text-xs"
                style={{
                  borderColor: "var(--border-base)",
                  color: "var(--fg-base)",
                }}
              >
                + {t("contacts.addEmail")}
              </button>
            </Section>

            {messages.length > 0 && (
              <Section title={t("contacts.recentMessages")}>
                <ul className="flex flex-col">
                  {messages.slice(0, 20).map((m) => (
                    <li
                      key={m.id}
                      onClick={() => onOpenMessage(m.id)}
                      className="flex cursor-pointer items-center justify-between gap-2 rounded px-2 py-1 text-xs transition-colors"
                      onMouseEnter={(e) =>
                        (e.currentTarget.style.background =
                          "var(--bg-hover)")
                      }
                      onMouseLeave={(e) =>
                        (e.currentTarget.style.background = "transparent")
                      }
                      title={t("contacts.openMessage")}
                    >
                      <div className="flex min-w-0 flex-1 items-center gap-1.5">
                        {!m.seen && (
                          <span
                            aria-hidden
                            className="inline-block h-1.5 w-1.5 shrink-0 rounded-full"
                            style={{ background: "var(--accent)" }}
                          />
                        )}
                        <span
                          className="min-w-0 flex-1 truncate"
                          style={{
                            color: "var(--fg-base)",
                            fontWeight: m.seen ? 400 : 500,
                          }}
                        >
                          {m.subject || t("inbox.noSubject")}
                        </span>
                      </div>
                      <span
                        className="shrink-0"
                        style={{ color: "var(--fg-subtle)" }}
                      >
                        {new Date(m.date).toLocaleDateString()}
                      </span>
                    </li>
                  ))}
                </ul>
              </Section>
            )}
          </>
        )}
      </div>
      {addEmailOpen && (
        <AddEmailDialog
          onCancel={() => setAddEmailOpen(false)}
          onSubmit={(v) => void submitAddEmail(v)}
        />
      )}
    </div>
  );
}

/** App-stiliger Ersatz für `window.prompt`. Validiert grobschlächtig auf
 *  `local@host`-Form (mindestens ein @ mit was davor und was danach,
 *  kein Leerzeichen) — der Server validiert sauber, der Frontend-Check
 *  fängt nur Tippfehler ab. */
function AddEmailDialog({
  onCancel,
  onSubmit,
}: {
  onCancel: () => void;
  onSubmit: (email: string) => void;
}) {
  const { t } = useTranslation();
  const [value, setValue] = useState("");
  const [touched, setTouched] = useState(false);
  const trimmed = value.trim();
  const looksValid = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(trimmed);
  const showError = touched && !looksValid && trimmed.length > 0;

  const submit = () => {
    setTouched(true);
    if (!looksValid) return;
    onSubmit(trimmed);
  };

  return (
    <div
      className="fixed inset-0 z-[62] flex items-start justify-center overflow-y-auto px-4 py-[20vh]"
      style={{ background: "rgba(0,0,0,0.55)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div
        role="dialog"
        className="flex w-full max-w-md flex-col overflow-hidden rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
          if (e.key === "Escape") onCancel();
        }}
      >
        <header
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <h2 className="text-sm font-semibold">
            {t("contacts.addEmailTitle")}
          </h2>
          <button
            type="button"
            onClick={onCancel}
            className="text-xs"
            style={{ color: "var(--fg-muted)" }}
          >
            ✕
          </button>
        </header>
        <div className="flex flex-col gap-2 px-4 py-4">
          <p className="text-xs" style={{ color: "var(--fg-muted)" }}>
            {t("contacts.addEmailPrompt")}
          </p>
          <input
            type="email"
            autoFocus
            value={value}
            onChange={(e) => setValue(e.target.value)}
            placeholder={t("contacts.addEmailPlaceholder")}
            className="rounded-md border px-2 py-1.5 text-sm"
            style={{
              borderColor: showError ? "#ef4444" : "var(--border-base)",
              background: "var(--bg-base)",
              color: "var(--fg-base)",
            }}
          />
          {showError && (
            <span className="text-[11px]" style={{ color: "#ef4444" }}>
              {t("contacts.addEmailInvalid")}
            </span>
          )}
        </div>
        <footer
          className="flex items-center justify-end gap-2 border-t px-4 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <button
            type="button"
            onClick={onCancel}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-muted)",
            }}
          >
            {t("contacts.cancel")}
          </button>
          <button
            type="button"
            onClick={submit}
            disabled={!looksValid}
            className="rounded-md border px-3 py-1 text-xs"
            style={{
              borderColor: "var(--border-base)",
              background: looksValid ? "var(--accent)" : "var(--bg-base)",
              color: looksValid ? "var(--bg-panel)" : "var(--fg-muted)",
              opacity: looksValid ? 1 : 0.6,
            }}
          >
            {t("contacts.addEmail")}
          </button>
        </footer>
      </div>
    </div>
  );
}

function ContactReadOnly({ detail }: { detail: ContactDetailType }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-1 text-sm">
      {detail.organization && (
        <Field label={t("contacts.organization")}>{detail.organization}</Field>
      )}
      {detail.jobTitle && (
        <Field label={t("contacts.jobTitle")}>{detail.jobTitle}</Field>
      )}
      {detail.phone && <Field label={t("contacts.phone")}>{detail.phone}</Field>}
      {detail.mobile && (
        <Field label={t("contacts.mobile")}>{detail.mobile}</Field>
      )}
      {(detail.street || detail.zip || detail.city || detail.country) && (
        <Field label={t("contacts.address")}>
          {[detail.street, [detail.zip, detail.city].filter(Boolean).join(" "), detail.country]
            .filter((s) => s && s.length > 0)
            .join(", ")}
        </Field>
      )}
      {detail.website && (
        <Field label={t("contacts.website")}>{detail.website}</Field>
      )}
      {detail.notes && (
        <Field label={t("contacts.notes")}>
          <span style={{ whiteSpace: "pre-wrap" }}>{detail.notes}</span>
        </Field>
      )}
      {detail.origin === "extracted" && (
        <p
          className="mt-2 text-[11px]"
          style={{ color: "var(--fg-subtle)" }}
        >
          {t("contacts.extractedNotice")}
        </p>
      )}
    </div>
  );
}

function ContactFormFields({
  form,
  onChange,
}: {
  form: ContactForm;
  onChange: (p: Partial<ContactForm>) => void;
}) {
  const { t } = useTranslation();
  const setStr = (key: keyof ContactForm) => (v: string) =>
    onChange({ [key]: v.length > 0 ? v : null } as Partial<ContactForm>);
  return (
    <div className="flex flex-col gap-2 text-sm">
      <Input
        required
        label={t("contacts.displayName")}
        value={form.displayName}
        onChange={(v) => onChange({ displayName: v })}
      />
      <div className="grid grid-cols-2 gap-2">
        <Input
          label={t("contacts.organization")}
          value={form.organization ?? ""}
          onChange={setStr("organization")}
        />
        <Input
          label={t("contacts.jobTitle")}
          value={form.jobTitle ?? ""}
          onChange={setStr("jobTitle")}
        />
      </div>
      <div className="grid grid-cols-2 gap-2">
        <Input
          label={t("contacts.phone")}
          value={form.phone ?? ""}
          onChange={setStr("phone")}
        />
        <Input
          label={t("contacts.mobile")}
          value={form.mobile ?? ""}
          onChange={setStr("mobile")}
        />
      </div>
      <Input
        label={t("contacts.street")}
        value={form.street ?? ""}
        onChange={setStr("street")}
      />
      <div className="grid grid-cols-[1fr_2fr] gap-2">
        <Input
          label={t("contacts.zip")}
          value={form.zip ?? ""}
          onChange={setStr("zip")}
        />
        <Input
          label={t("contacts.city")}
          value={form.city ?? ""}
          onChange={setStr("city")}
        />
      </div>
      <Input
        label={t("contacts.country")}
        value={form.country ?? ""}
        onChange={setStr("country")}
      />
      <Input
        label={t("contacts.website")}
        value={form.website ?? ""}
        onChange={setStr("website")}
      />
      <label className="block">
        <span
          className="mb-1 block text-xs"
          style={{ color: "var(--fg-muted)" }}
        >
          {t("contacts.notes")}
        </span>
        <textarea
          value={form.notes}
          onChange={(e) => onChange({ notes: e.target.value })}
          rows={4}
          className="w-full rounded-md px-2 py-1 text-sm outline-none"
          style={{
            background: "var(--bg-base)",
            color: "var(--fg-base)",
            border: "1px solid var(--border-base)",
          }}
        />
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={form.pinned}
          onChange={(e) => onChange({ pinned: e.target.checked })}
        />
        <span>{t("contacts.pinToggle")}</span>
      </label>
    </div>
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
    <section className="mt-4 flex flex-col gap-1">
      <div
        className="text-[11px] uppercase tracking-[0.15em]"
        style={{ color: "var(--fg-subtle)" }}
      >
        {title}
      </div>
      {children}
    </section>
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
    <div className="grid grid-cols-[110px_1fr] gap-2">
      <span className="text-[11px]" style={{ color: "var(--fg-subtle)" }}>
        {label}
      </span>
      <span style={{ color: "var(--fg-base)" }}>{children}</span>
    </div>
  );
}

function Input({
  label,
  value,
  onChange,
  required,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  required?: boolean;
}) {
  return (
    <label className="block">
      <span
        className="mb-1 block text-xs"
        style={{ color: "var(--fg-muted)" }}
      >
        {label}
        {required ? " *" : ""}
      </span>
      <input
        value={value}
        required={required}
        onChange={(e) => onChange(e.target.value)}
        className="w-full rounded-md px-2 py-1 text-sm outline-none"
        style={{
          background: "var(--bg-base)",
          color: "var(--fg-base)",
          border: "1px solid var(--border-base)",
        }}
      />
    </label>
  );
}
