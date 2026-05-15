import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { AddressAutocomplete } from "./AddressAutocomplete";
import { RichEditor, type RichEditorHandle } from "./RichEditor";
import {
  plainToHtml,
  sanitizeFragment,
  wrapAsHtmlDocument,
} from "../utils/mailHtml";
import type {
  AccountSummary,
  ComposeAttachment,
  ComposeDraft,
} from "../types";

/** Snapshot of the composed mail handed to the caller. The undo-send
 *  pipeline ships this through a 5-second buffer; on cancel, Compose re-
 *  opens with these fields plus `bodyHtml` so the editor restores byte-
 *  identical state. */
export type ComposeSendSnapshot = {
  accountId: string;
  identityKey: string;
  /** Resolved alias-or-account `from` identity, or `null` to default to
   *  the account's primary identity. */
  from: { email: string; fromName: string } | null;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
  bodyHtml: string;
  attachments: ComposeAttachment[];
  inReplyToHeader?: string;
  references?: string[];
  parentMessageId?: string;
  parentMode?: "answered" | "forwarded";
  /** Wenn gesetzt, ist das ein Edit eines bestehenden Server-Drafts.
   *  Caller löscht nach erfolgreichem Send/Save-Draft die Original-
   *  Mail (Best-Effort), damit nicht zwei Versionen rumliegen. */
  replacesDraftMessageId?: string;
};

type Props = {
  draft: ComposeDraft;
  accounts: AccountSummary[];
  onClose: () => void;
  /** Called when the user clicks Send. The parent owns the actual SMTP
   *  invocation — typically wraps it in a 5-second undo buffer. */
  onSendRequest: (snapshot: ComposeSendSnapshot) => void;
  /** Called when the user clicks "Save as Draft". The parent fires the
   *  `save_draft` Tauri command. Compose itself doesn't await — closes
   *  immediately, parent surfaces success/failure as a status message. */
  onSaveDraft: (snapshot: ComposeSendSnapshot) => void;
};

type Mode = "new" | "reply" | "forward";

export function Compose({
  draft,
  accounts,
  onClose,
  onSendRequest,
  onSaveDraft,
}: Props) {
  const { t } = useTranslation();

  // An "identity" is a (accountId, email, fromName) tuple. Each account
  // contributes its primary identity plus one per alias. The Compose
  // dropdown is keyed by `${accountId}::${email}` so aliases that share an
  // address with their parent account are still selectable unambiguously.
  const identities = useMemo(() => {
    return accounts.flatMap((a) => {
      const base = {
        key: `${a.id}::${a.address.toLowerCase()}`,
        accountId: a.id,
        email: a.address,
        fromName: a.fromName,
        color: a.color,
        displayName: a.displayName,
        isAlias: false as const,
      };
      const aliasEntries = a.aliases.map((al) => ({
        key: `${a.id}::${al.email.toLowerCase()}`,
        accountId: a.id,
        email: al.email,
        fromName: al.fromName,
        color: a.color,
        displayName: a.displayName,
        isAlias: true as const,
      }));
      return [base, ...aliasEntries];
    });
  }, [accounts]);

  const defaultIdentityKey = useMemo(() => {
    // Undo-send pfad: User hatte explizit eine Identity gewählt
    // (Account oder Alias). Wir setzen sie unverändert wieder ein.
    if (draft.identityKey) return draft.identityKey;
    const defAccount = draft.accountId ?? accounts[0]?.id;
    if (!defAccount) return undefined;
    const defAcc = accounts.find((a) => a.id === defAccount);
    return defAcc ? `${defAcc.id}::${defAcc.address.toLowerCase()}` : undefined;
  }, [draft.identityKey, draft.accountId, accounts]);

  const [identityKey, setIdentityKey] = useState<string | undefined>(
    defaultIdentityKey,
  );
  const selectedIdentity = useMemo(
    () => identities.find((i) => i.key === identityKey),
    [identities, identityKey],
  );
  const accountId = selectedIdentity?.accountId;
  const selectedAccount = useMemo(
    () => accounts.find((a) => a.id === accountId),
    [accounts, accountId],
  );
  const [to, setTo] = useState(draft.to);
  const [cc, setCc] = useState(draft.cc);
  const [bcc, setBcc] = useState(draft.bcc);
  const [subject, setSubject] = useState(draft.subject);
  const [showCc, setShowCc] = useState(!!draft.cc);
  const [showBcc, setShowBcc] = useState(!!draft.bcc);
  const [attachments, setAttachments] = useState<ComposeAttachment[]>(
    draft.attachments ?? [],
  );

  const editorRef = useRef<RichEditorHandle>(null);
  const toRef = useRef<HTMLInputElement>(null);
  const ccRef = useRef<HTMLInputElement>(null);
  const bccRef = useRef<HTMLInputElement>(null);
  const subjectRef = useRef<HTMLInputElement>(null);
  const formRef = useRef<HTMLFormElement>(null);
  // Build the editor's initial HTML.
  //
  //   * Undo-send-roundtrip path: caller pre-built the body HTML and
  //     passes it in `draft.bodyHtml`. We use it verbatim so the user
  //     gets back exactly what they had on screen when they clicked
  //     Send (signature, formatting, attachments, all intact).
  //   * Normal path: user's start text (empty for reply/forward), the
  //     account signature, then the sanitized quoted block. Mirrors
  //     Apple Mail / Outlook ordering.
  //
  // The signature here uses the *initial* account's sig. The
  // `replaceSignature` effect below keeps it in sync if the user
  // switches the From-account afterwards.
  const initialHtml = useMemo(() => {
    if (draft.bodyHtml && draft.bodyHtml.length > 0) {
      return draft.bodyHtml;
    }
    const userStart = draft.body
      ? `<div>${plainToHtml(draft.body)}</div>`
      : "<div><br></div>";

    const firstAccount = accounts.find((a) => a.id === defaultIdentityKey?.split("::")[0]);
    const sigHtml = firstAccount?.signatureHtml
      ? `<div><br></div><div class="cm-signature">${sanitizeFragment(firstAccount.signatureHtml)}</div>`
      : "";

    const quote = draft.quotedHtml ? sanitizeFragment(draft.quotedHtml) : "";
    const quoteBlock = quote ? `<div><br></div>${quote}` : "";

    return `${userStart}${sigHtml}${quoteBlock}`;
    // Only needs to be computed once at open — the editor is uncontrolled.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!identityKey && identities.length > 0) setIdentityKey(identities[0].key);
  }, [identities, identityKey]);

  // Signatur-Sync beim Account-Wechsel. Beim ersten Render hat der Editor
  // bereits die Initial-Signatur via `initialHtml` reingerendert; wir
  // skippen also den allerersten Lauf. Bei jedem späteren Identity-Wechsel
  // wird die alte `.cm-signature`-Box durch die neue ersetzt — User-Body
  // dazwischen bleibt unangetastet. Wenn `draft.bodyHtml` gesetzt war
  // (Undo-Send-Roundtrip), springt der Effect ebenfalls erst beim NÄCHSTEN
  // Wechsel an — der eingefrorene Body kommt 1:1 vom letzten Send-Versuch
  // mit der zugehörigen Signatur, da darf nichts ge-flipped werden.
  const sigSyncSkipFirstRef = useRef(true);
  useEffect(() => {
    if (sigSyncSkipFirstRef.current) {
      sigSyncSkipFirstRef.current = false;
      return;
    }
    const sig = selectedAccount?.signatureHtml
      ? sanitizeFragment(selectedAccount.signatureHtml)
      : null;
    editorRef.current?.replaceSignature(sig);
  }, [selectedAccount]);

  const mode: Mode = useMemo(() => {
    if (draft.parentMode === "answered") return "reply";
    if (draft.parentMode === "forwarded") return "forward";
    if (/^fwd?:/i.test(draft.subject)) return "forward";
    return "new";
  }, [draft]);

  // Mount-time focus. Runs once; `mode` is derived from `draft` which is
  // fixed for the lifetime of the dialog.
  //   - reply/forward → caret at the *start* of the editor body so the user
  //     types above signature + quote (matches Apple Mail / Outlook).
  //   - new mail     → To-Feld first. Selbst wenn die Adresse vorbefüllt
  //     ist (Compose-from-Reader-Header etc.), öffnet das Autocomplete-
  //     Dropdown automatisch sobald Focus auf dem Input liegt → User
  //     kann sofort Empfänger ergänzen oder mit Tab weiter zu Subject.
  useEffect(() => {
    const raf = requestAnimationFrame(() => {
      if (mode === "reply" || mode === "forward") {
        editorRef.current?.focusStart();
      } else {
        toRef.current?.focus();
        // Caret ans Ende falls schon was drinsteht (vorbefüllt aus
        // einem Compose-from-context-Pfad).
        const el = toRef.current;
        if (el) el.setSelectionRange(el.value.length, el.value.length);
      }
    });
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const title =
    mode === "reply"
      ? t("compose.replyTitle")
      : mode === "forward"
        ? t("compose.forwardTitle")
        : t("compose.title");

  const canSend = !!accountId && to.trim().length > 0;
  /** Save-Draft hat nur eine Account-Vorbedingung — keine Empfänger nötig.
   *  Die Server-APPEND akzeptiert auch eine halb-fertige Mail, das ist
   *  ja gerade der Sinn von Drafts. */
  const canSaveDraft = !!accountId;

  const addAttachments = async () => {
    try {
      const picked = await openDialog({
        multiple: true,
        title: t("attachments.add"),
      });
      if (!picked) return;
      const paths = Array.isArray(picked) ? picked : [picked];
      const next: ComposeAttachment[] = [];
      for (const path of paths) {
        // Use Rust to fetch size + a sane default filename. For MVP, derive
        // from the string path; lettre will re-read the file at send time
        // anyway so we don't need to load bytes here.
        next.push({
          clientId: crypto.randomUUID(),
          path,
          filename: basename(path),
          sizeBytes: 0,
        });
      }
      setAttachments((cur) => [...cur, ...next]);
    } catch (e) {
      setError(String(e));
    }
  };

  const removeAttachment = (clientId: string) => {
    setAttachments((cur) => cur.filter((a) => a.clientId !== clientId));
  };

  /** Called by RichEditor when the user pastes an image from the clipboard.
   *  Persists the bytes to a temp file via Tauri, registers an inline
   *  attachment (so the recipient sees the picture embedded and the
   *  attachment list still shows it), and returns the CID + a blob: URL
   *  the editor uses for immediate in-place preview. */
  const handlePasteImage = async (file: File) => {
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      // Pass the byte buffer as plain `Array<number>` — Tauri's IPC
      // bridge marshals it as a JSON array of integers into `Vec<u8>`
      // on the Rust side. For images this is fine (typical screenshot
      // is sub-megabyte); we already cap at 32 MB in the Rust command.
      const saved = await invoke<{
        path: string;
        filename: string;
        mimeType: string;
        sizeBytes: number;
      }>("save_clipboard_image", {
        bytes: Array.from(bytes),
        mimeType: file.type || "image/png",
      });
      // `@` suffix mirrors RFC 2392 convention so the cid: ref reads as
      // a proper Message-ID — Outlook used to choke on bare uuids.
      const contentId = `${crypto.randomUUID().replace(/-/g, "")}@crystalmail`;
      const previewUrl = URL.createObjectURL(file);
      setAttachments((cur) => [
        ...cur,
        {
          clientId: crypto.randomUUID(),
          path: saved.path,
          filename: saved.filename,
          sizeBytes: saved.sizeBytes,
          mimeType: saved.mimeType,
          isInline: true,
          contentId,
        },
      ]);
      return { contentId, previewUrl };
    } catch (e) {
      setError(String(e));
      return null;
    }
  };

  /** Frieren des aktuellen Form-State in einen Snapshot ein. Wird sowohl
   *  vom Send- als auch vom Save-Draft-Pfad benutzt. Body ist sowohl als
   *  raw HTML-fragment (für den Undo-Roundtrip) als auch als wrapped
   *  HTML-Document (für SMTP / IMAP) enthalten. */
  const buildSnapshot = (): ComposeSendSnapshot | null => {
    if (!accountId) return null;
    const rawHtml = editorRef.current?.getHtml() ?? "";
    const plain = editorRef.current?.getText() ?? "";
    const htmlBody = wrapAsHtmlDocument(sanitizeFragment(rawHtml));
    const fromOverride =
      selectedIdentity &&
      selectedAccount &&
      (selectedIdentity.isAlias ||
        selectedIdentity.email.toLowerCase() !==
          selectedAccount.address.toLowerCase())
        ? {
            email: selectedIdentity.email,
            fromName: selectedIdentity.fromName,
          }
        : null;
    return {
      accountId,
      identityKey: identityKey ?? `${accountId}::${selectedAccount?.address.toLowerCase() ?? ""}`,
      from: fromOverride,
      to,
      cc,
      bcc,
      subject,
      body: plain,
      bodyHtml: htmlBody,
      attachments,
      inReplyToHeader: draft.inReplyToHeader,
      references: draft.references,
      parentMessageId: draft.parentMessageId,
      parentMode: draft.parentMode,
      replacesDraftMessageId: draft.replacesDraftMessageId,
    };
  };

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const snap = buildSnapshot();
    if (!snap) return;
    setError(null);
    // Optimistic-Send: Dialog sofort zu, Parent übernimmt das eigentliche
    // SMTP-Roundtrip in einem 5s-Undo-Buffer. Ein Fehler kommt später als
    // Toast zurück + die Mail liegt dann im Drafts-Ordner.
    onSendRequest(snap);
    onClose();
  };

  const onSaveDraftClick = () => {
    const snap = buildSnapshot();
    if (!snap) return;
    setError(null);
    onSaveDraft(snap);
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center px-4"
      style={{ background: "rgba(0,0,0,0.45)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <form
        ref={formRef}
        onSubmit={submit}
        onKeyDown={(e) => {
          // Ctrl+Enter (or ⌘+Enter on macOS) sends from anywhere inside the
          // form — subject input, address fields, and the contentEditable
          // editor. Plain Enter in the editor still inserts a new paragraph.
          if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
            e.preventDefault();
            if (canSend) formRef.current?.requestSubmit();
          }
        }}
        className="flex max-h-[92vh] w-full max-w-2xl flex-col rounded-xl border shadow-xl"
        style={{
          background: "var(--bg-panel)",
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <header
          className="flex items-center justify-between gap-2 border-b px-5 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <h2 className="text-base font-semibold">{title}</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("compose.cancel")}
            className="rounded-md px-2 py-1 text-sm"
            style={{ color: "var(--fg-muted)" }}
          >
            ✕
          </button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-4">
          {accounts.length === 0 ? (
            <p className="text-sm" style={{ color: "var(--fg-muted)" }}>
              {t("compose.noAccounts")}
            </p>
          ) : (
            <>
              <LabeledRow label={t("compose.from")}>
                <select
                  value={identityKey ?? ""}
                  onChange={(e) => setIdentityKey(e.target.value)}
                  className="w-full rounded-md px-2 py-1.5 text-sm"
                  style={{
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                    border: "1px solid var(--border-base)",
                  }}
                  title={t("compose.accountPickHint")}
                >
                  {identities.map((i) => (
                    <option key={i.key} value={i.key}>
                      {i.fromName || i.displayName} · {i.email}
                      {i.isAlias ? ` · ${t("compose.alias")}` : ""}
                    </option>
                  ))}
                </select>
              </LabeledRow>

              <LabeledRow
                label={t("compose.to")}
                trailing={
                  <div className="flex gap-2 text-[11px]" style={{ color: "var(--fg-subtle)" }}>
                    {!showCc && (
                      <button
                        type="button"
                        tabIndex={-1}
                        onClick={() => setShowCc(true)}
                      >
                        {t("compose.addCc")}
                      </button>
                    )}
                    {!showBcc && (
                      <button
                        type="button"
                        tabIndex={-1}
                        onClick={() => setShowBcc(true)}
                      >
                        {t("compose.addBcc")}
                      </button>
                    )}
                  </div>
                }
              >
                {/* Tab-Order-Anker für "neue Mail": To → Subject → Body.
                    Wenn Cc/Bcc sichtbar sind, fließt der Tab natürlich
                    durch Cc → Bcc → Subject (Document-Order). Ohne Cc/Bcc
                    leitet der onKeyDown Tab direkt zum Subject. */}
                <AddressInput
                  innerRef={toRef}
                  value={to}
                  onChange={setTo}
                  placeholder={t("compose.toPlaceholder")}
                  onKeyDown={(e) => {
                    if (e.key !== "Tab" || e.shiftKey) return;
                    if (showCc || showBcc) return; // natural flow → Cc/Bcc
                    e.preventDefault();
                    subjectRef.current?.focus();
                    subjectRef.current?.setSelectionRange(
                      subject.length,
                      subject.length,
                    );
                  }}
                />
              </LabeledRow>

              {showCc && (
                <LabeledRow label={t("compose.cc")}>
                  <AddressInput
                    innerRef={ccRef}
                    value={cc}
                    onChange={setCc}
                    onKeyDown={(e) => {
                      if (e.key !== "Tab" || e.shiftKey) return;
                      if (showBcc) return;
                      e.preventDefault();
                      subjectRef.current?.focus();
                    }}
                  />
                </LabeledRow>
              )}

              {showBcc && (
                <LabeledRow label={t("compose.bcc")}>
                  <AddressInput
                    innerRef={bccRef}
                    value={bcc}
                    onChange={setBcc}
                    onKeyDown={(e) => {
                      if (e.key !== "Tab" || e.shiftKey) return;
                      e.preventDefault();
                      subjectRef.current?.focus();
                    }}
                  />
                </LabeledRow>
              )}

              <LabeledRow label={t("compose.subject")}>
                <input
                  ref={subjectRef}
                  value={subject}
                  onChange={(e) => setSubject(e.target.value)}
                  onKeyDown={(e) => {
                    // Subject → Body: Tab-Stop überspringt die
                    // Attachments-"+-Anhängen"-Pille direkt darunter
                    // (die per tabIndex=-1 gar nicht erst Tab-fokussierbar
                    // ist), und springt in den Editor-Body. Caret an
                    // den Anfang, damit der User über der Signatur
                    // landet (passt zum Reply/Forward-Verhalten).
                    if (e.key === "Tab" && !e.shiftKey) {
                      e.preventDefault();
                      editorRef.current?.focusStart();
                    }
                  }}
                  className="w-full rounded-md px-2.5 py-1.5 text-sm"
                  style={{
                    background: "var(--bg-base)",
                    color: "var(--fg-base)",
                    border: "1px solid var(--border-base)",
                  }}
                />
              </LabeledRow>

              <AttachmentList
                items={attachments}
                onAdd={addAttachments}
                onRemove={removeAttachment}
              />

              <div className="mt-3">
                <RichEditor
                  ref={editorRef}
                  initialHtml={initialHtml}
                  placeholder={
                    mode === "reply"
                      ? t("compose.replyPlaceholder")
                      : mode === "forward"
                        ? t("compose.forwardPlaceholder")
                        : t("compose.bodyPlaceholder")
                  }
                  minHeight={260}
                  onPasteImage={handlePasteImage}
                />
              </div>
            </>
          )}

          {error && (
            <div
              className="mt-3 rounded-md px-3 py-2 text-xs"
              style={{
                background: "rgba(248,113,113,0.12)",
                color: "#ef4444",
                border: "1px solid rgba(248,113,113,0.25)",
              }}
            >
              {error}
            </div>
          )}
        </div>

        <footer
          className="flex items-center justify-end gap-2 border-t px-5 py-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm"
            style={{ color: "var(--fg-muted)" }}
          >
            {t("compose.cancel")}
          </button>
          {/* Als Entwurf speichern: kein Empfänger nötig, IMAP APPEND zum
              Drafts-Ordner. Schließt den Dialog optimistisch — Erfolg/
              Fehler kommen als Status-Toast vom Parent zurück. */}
          <button
            type="button"
            onClick={onSaveDraftClick}
            disabled={!canSaveDraft}
            className="rounded-md border px-3 py-1.5 text-sm disabled:opacity-50"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
            }}
            title={t("compose.saveDraftHint")}
          >
            {t("compose.saveDraft")}
          </button>
          <button
            type="submit"
            disabled={!canSend}
            title={t("compose.sendHotkeyHint")}
            className="rounded-md px-4 py-1.5 text-sm font-medium disabled:opacity-50"
            style={{ background: "var(--accent)", color: "white" }}
          >
            {t("compose.send")}
          </button>
        </footer>
      </form>
    </div>
  );
}

function AttachmentList({
  items,
  onAdd,
  onRemove,
}: {
  items: ComposeAttachment[];
  onAdd: () => void;
  onRemove: (clientId: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="mb-2 mt-3 flex flex-wrap items-center gap-1.5">
      <button
        type="button"
        // Aus dem Tab-Flow: User soll Subject → Body direkt durchtabben.
        // Mit der Maus voll erreichbar, kein A11y-Verlust.
        tabIndex={-1}
        onClick={onAdd}
        className="inline-flex items-center gap-1 rounded-md border px-2 py-1 text-[11px]"
        style={{
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
          background: "transparent",
        }}
      >
        <span aria-hidden>📎</span> {t("attachments.add")}
      </button>
      {items.map((a) => (
        <span
          key={a.clientId}
          className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px]"
          style={{
            borderColor: "var(--border-base)",
            color: "var(--fg-base)",
          }}
        >
          <span className="max-w-[16rem] truncate">{a.filename}</span>
          {a.isInline && (
            <span
              className="rounded px-1 text-[9px] uppercase tracking-wide"
              title={t("attachments.inlineHint", {
                defaultValue: "Wird im Body eingebettet (cid:)",
              })}
              style={{
                background: "var(--bg-hover)",
                color: "var(--fg-subtle)",
              }}
            >
              inline
            </span>
          )}
          <button
            type="button"
            tabIndex={-1}
            onClick={() => onRemove(a.clientId)}
            aria-label={t("attachments.remove")}
            className="ml-1 rounded-full px-1 text-[10px]"
            style={{ color: "var(--fg-subtle)" }}
          >
            ✕
          </button>
        </span>
      ))}
    </div>
  );
}

function AddressInput({
  innerRef,
  value,
  onChange,
  placeholder,
  onKeyDown,
}: {
  innerRef?: React.Ref<HTMLInputElement>;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  onKeyDown?: (e: React.KeyboardEvent<HTMLInputElement>) => void;
}) {
  // Lokaler Ref damit das Autocomplete-Dropdown den Anchor messen
  // kann. Wenn der Caller einen eigenen Ref übergibt, mergen wir
  // beide via Callback-Ref (rare aber clean).
  const localRef = useRef<HTMLInputElement | null>(null);
  const setRef = (el: HTMLInputElement | null) => {
    localRef.current = el;
    if (typeof innerRef === "function") innerRef(el);
    else if (innerRef && typeof innerRef === "object")
      (innerRef as React.MutableRefObject<HTMLInputElement | null>).current = el;
  };
  // "Hat der Input gerade Focus?" — nur dann zeigen wir das Dropdown.
  // Sonst würde der Picker auch nach Tab-out noch sichtbar bleiben.
  const [focused, setFocused] = useState(false);

  return (
    <>
      <input
        ref={setRef}
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={onKeyDown}
        onFocus={() => setFocused(true)}
        // Delay damit ein Klick auf einen Dropdown-Eintrag noch
        // durchkommt bevor wir das Dropdown ausblenden — der
        // mousedown-handler im AddressAutocomplete preventDefault'et
        // den Focus-Loss, aber wir setzen `focused` State sicherheits-
        // halber mit einem rAF-Tick Verzögerung.
        onBlur={() => {
          window.setTimeout(() => setFocused(false), 100);
        }}
        placeholder={placeholder}
        className="w-full rounded-md px-2.5 py-1.5 text-sm"
        style={{
          background: "var(--bg-base)",
          color: "var(--fg-base)",
          border: "1px solid var(--border-base)",
        }}
        autoComplete="off"
      />
      {focused && (
        <AddressAutocomplete
          anchorRef={localRef}
          value={value}
          onPick={(formatted) => onChange(formatted)}
        />
      )}
    </>
  );
}

function LabeledRow({
  label,
  trailing,
  children,
}: {
  label: string;
  trailing?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="mb-2 grid grid-cols-[60px_1fr] items-start gap-2">
      <div className="pt-1.5 text-xs" style={{ color: "var(--fg-subtle)" }}>
        {label}
      </div>
      <div className="flex items-center gap-2">
        <div className="flex-1">{children}</div>
        {trailing}
      </div>
    </div>
  );
}

/** Comma-or-whitespace tolerant split for the To/Cc/Bcc input strings.
 *  Exported so the App-level send pipeline (which wraps Compose's snapshot
 *  in the undo-send buffer) can use the same parsing rules. */
export function splitAddresses(v: string): string[] {
  return v
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

function basename(path: string): string {
  const m = path.split(/[\\/]/).filter((p) => p.length > 0);
  return m[m.length - 1] ?? path;
}
