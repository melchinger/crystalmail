import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EnvelopeSummary, MessageDetail, PiStreamChunk } from "../types";
import type { ComposeFromPiIntent } from "../App";
import { stripHtmlToText } from "../utils/mailHtml";
import { isAiDisabledError, useAiEnabled } from "../utils/aiState";

/**
 * Bottom-docked pi terminal. Collapsed = thin bar; expanded = chat log
 * + prompt input. Context is assembled fresh for each turn from the
 * currently selected message (if any) or the visible inbox list.
 *
 * Mail references in pi's responses are expected to use the form
 * `cm:msg:<uuid>` — the renderer scans for that pattern and turns them
 * into clickable chips that select the message in the main view.
 */

type ChatMessage =
  | { kind: "user"; text: string }
  | {
      kind: "assistant";
      text: string;
      streaming?: boolean;
      /**
       * Snapshot of the selected-message id at the moment this turn
       * was fired. Drives which "übernehmen als …" buttons the bubble
       * offers: single mail → new/reply/forward; folder context →
       * only "new mail".
       */
      contextMessageId?: string;
    }
  | { kind: "system"; text: string };

type Props = {
  expanded: boolean;
  onToggle: () => void;
  /** Currently selected envelope id — context = that mail's details. */
  selectedMessageId?: string;
  /** Fallback context when nothing is selected = the visible inbox list. */
  inbox: EnvelopeSummary[];
  activeFolderLabel: string;
  onSelectMessage: (id: string) => void;
  /** Hand pi's response text to App.tsx which builds the right draft. */
  onComposeFromPi: (
    intent: ComposeFromPiIntent,
    body: string,
    contextMessageId?: string,
  ) => void;
};

export function PiTerminal({
  expanded,
  onToggle,
  selectedMessageId,
  inbox,
  activeFolderLabel,
  onSelectMessage,
  onComposeFromPi,
}: Props) {
  const { t } = useTranslation();
  const [chat, setChat] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  // Master AI flag: when off the chat input is disabled and a hint
  // sits in place of the placeholder. We still let users *read* prior
  // chat history — only sending is gated.
  const [aiEnabled] = useAiEnabled();

  // Subscribe to the `chat-stream` events emitted by pi_rpc. Appends deltas
  // to the last assistant message; creates one if missing.
  //
  // Cancellation dance: `listen()` is async. In React StrictMode the effect
  // mounts twice in dev — without a cancellation flag the first cleanup
  // runs before the first subscription resolves, which leaves two
  // listeners attached and every delta gets applied twice (symptom:
  // "und und betrifftrifft die die …"). We track a `cancelled` flag so
  // a subscription that resolves after unmount is torn down immediately.
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    (async () => {
      const fn = await listen<PiStreamChunk>("chat-stream", (e) => {
        const { content, done } = e.payload;
        setChat((prev) => {
          if (done) {
            return prev.map((m, i) =>
              i === prev.length - 1 && m.kind === "assistant"
                ? { ...m, streaming: false }
                : m,
            );
          }
          if (!content) return prev;
          const last = prev[prev.length - 1];
          if (last && last.kind === "assistant" && last.streaming) {
            return [
              ...prev.slice(0, -1),
              { ...last, text: last.text + content },
            ];
          }
          return [
            ...prev,
            { kind: "assistant", text: content, streaming: true },
          ];
        });
      });
      if (cancelled) {
        fn();
      } else {
        unlisten = fn;
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // Auto-scroll when new content arrives.
  useEffect(() => {
    if (!expanded) return;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [chat, expanded]);

  const submit = async () => {
    const q = input.trim();
    if (!q || sending) return;
    if (!aiEnabled) {
      // Defensive: the input is already disabled when AI is off, but
      // a quick toggle race could still get here. Surface the same
      // friendly notice users see for stale clicks.
      setChat((prev) => [
        ...prev,
        { kind: "system", text: t("piTerminal.aiOff") },
      ]);
      return;
    }
    setInput("");
    setSending(true);

    // Assemble context + user question into a single prompt.
    let context = "";
    try {
      context = await buildContext({
        selectedMessageId,
        inbox,
        folderLabel: activeFolderLabel,
      });
    } catch (e) {
      setChat((prev) => [
        ...prev,
        { kind: "system", text: `Kontext konnte nicht gebaut werden: ${e}` },
      ]);
    }

    const fullPrompt = [
      context,
      "",
      "### Anweisungen",
      "Antworte auf deutsch (es sei denn der Nutzer schreibt englisch).",
      "Wenn du auf eine konkrete Mail verweist, verwende exakt das Format",
      "`cm:msg:<UUID>` (z. B. cm:msg:12345678-1234-…) — der Client rendert das als anklickbaren Link.",
      "",
      "### Frage",
      q,
    ].join("\n");

    // Snapshot the current context at send-time so the answer bubble
    // keeps the right "übernehmen als …" options even if the user
    // navigates to a different mail while pi is still thinking.
    const ctxId = selectedMessageId;
    setChat((prev) => [
      ...prev,
      { kind: "user", text: q },
      {
        kind: "assistant",
        text: "",
        streaming: true,
        contextMessageId: ctxId,
      },
    ]);

    try {
      await invoke("pi_ask", { message: fullPrompt });
    } catch (e) {
      // Mark the still-streaming placeholder as "done" so a partial
      // response doesn't render an endless caret ▌, then append the
      // system error below it. The kill-switch sentinel gets a
      // friendlier message than the raw `pi-Fehler: ai_disabled`.
      const text = isAiDisabledError(e)
        ? t("piTerminal.aiOff")
        : `pi-Fehler: ${String(e)} (Einstellungen → KI prüfen)`;
      setChat((prev) => [
        ...prev.map((m, i) =>
          i === prev.length - 1 && m.kind === "assistant" && m.streaming
            ? { ...m, streaming: false }
            : m,
        ),
        { kind: "system", text },
      ]);
    } finally {
      setSending(false);
    }
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit();
    }
  };

  const contextSummary = useMemo(() => {
    if (selectedMessageId) return t("pi.contextMessage");
    return t("pi.contextFolder", {
      folder: activeFolderLabel,
      count: inbox.length,
    });
  }, [selectedMessageId, activeFolderLabel, inbox.length, t]);

  return (
    <div
      className="flex shrink-0 flex-col border-t"
      style={{
        borderColor: "var(--border-base)",
        background: "var(--bg-panel)",
        height: expanded ? "40vh" : "2rem",
        transition: "height 180ms",
      }}
    >
      {/* Header bar — always visible, click to toggle. */}
      <button
        type="button"
        onClick={onToggle}
        className="flex h-8 shrink-0 items-center justify-between gap-2 px-3 text-xs"
        style={{
          color: "var(--fg-muted)",
          background: "var(--bg-base)",
          borderBottom: expanded
            ? "1px solid var(--border-soft)"
            : "1px solid transparent",
        }}
      >
        <span className="inline-flex items-center gap-2">
          <span
            aria-hidden
            className="inline-block"
            style={{
              transform: expanded ? "rotate(90deg)" : "rotate(0deg)",
              transition: "transform 150ms",
            }}
          >
            ▸
          </span>
          <span style={{ fontWeight: 600 }}>π</span>
          <span>{t("pi.title")}</span>
          {sending && (
            <span aria-hidden style={{ color: "var(--fg-subtle)" }}>
              …
            </span>
          )}
        </span>
        <span className="truncate" style={{ color: "var(--fg-subtle)" }}>
          {expanded ? contextSummary : t("pi.expandHint")}
        </span>
      </button>

      {expanded && (
        <>
          <div
            ref={scrollRef}
            className="min-h-0 flex-1 overflow-y-auto px-4 py-3 text-sm"
          >
            {chat.length === 0 ? (
              <p className="text-[13px]" style={{ color: "var(--fg-muted)" }}>
                {t("pi.emptyHint")}
              </p>
            ) : (
              <ul className="flex flex-col gap-3">
                {chat.map((m, i) => (
                  <li key={i}>
                    <ChatBubble
                      msg={m}
                      onSelectMessage={onSelectMessage}
                      onComposeFromPi={onComposeFromPi}
                    />
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div
            className="flex shrink-0 gap-2 border-t px-3 py-2"
            style={{ borderColor: "var(--border-soft)" }}
          >
            <textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={onKeyDown}
              rows={2}
              placeholder={
                aiEnabled
                  ? t("pi.inputPlaceholder")
                  : t("piTerminal.disabledPlaceholder")
              }
              disabled={!aiEnabled}
              className="flex-1 resize-none rounded-md px-2.5 py-1.5 text-sm outline-none disabled:cursor-not-allowed disabled:opacity-60"
              style={{
                background: "var(--bg-base)",
                color: "var(--fg-base)",
                border: "1px solid var(--border-base)",
              }}
            />
            <button
              type="button"
              onClick={() => void submit()}
              disabled={!aiEnabled || sending || !input.trim()}
              className="rounded-md px-3 py-1 text-xs font-medium disabled:opacity-50"
              style={{ background: "var(--accent)", color: "white" }}
            >
              {sending ? t("pi.sending") : t("pi.send")}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

// ─── chat bubble rendering ────────────────────────────────────────────────

function ChatBubble({
  msg,
  onSelectMessage,
  onComposeFromPi,
}: {
  msg: ChatMessage;
  onSelectMessage: (id: string) => void;
  onComposeFromPi: (
    intent: ComposeFromPiIntent,
    body: string,
    contextMessageId?: string,
  ) => void;
}) {
  const { t } = useTranslation();
  if (msg.kind === "user") {
    return (
      <div
        className="ml-auto max-w-[85%] rounded-md px-3 py-2 text-sm"
        style={{
          background: "var(--bg-selected)",
          color: "var(--fg-base)",
          width: "fit-content",
        }}
      >
        <pre className="m-0 whitespace-pre-wrap font-sans text-sm">
          {msg.text}
        </pre>
      </div>
    );
  }
  if (msg.kind === "system") {
    return (
      <div
        className="rounded-md px-3 py-2 text-xs"
        style={{
          background: "rgba(248,113,113,0.08)",
          color: "#ef4444",
          border: "1px solid rgba(248,113,113,0.2)",
        }}
      >
        {msg.text}
      </div>
    );
  }
  // assistant
  const showActions = !msg.streaming && msg.text.trim().length > 0;
  const hasContext = !!msg.contextMessageId;
  return (
    <div
      className="max-w-[95%] text-sm"
      style={{ color: "var(--fg-base)" }}
    >
      <AssistantText
        text={msg.text}
        streaming={msg.streaming}
        onSelectMessage={onSelectMessage}
      />
      {showActions && (
        <div className="mt-2 flex flex-wrap gap-1.5">
          <ActionBadge
            label={t("pi.useAsNewMail")}
            onClick={() =>
              onComposeFromPi("new", msg.text, msg.contextMessageId)
            }
          />
          {hasContext && (
            <>
              <ActionBadge
                label={t("pi.useAsReply")}
                onClick={() =>
                  onComposeFromPi("reply", msg.text, msg.contextMessageId)
                }
              />
              <ActionBadge
                label={t("pi.useAsForward")}
                onClick={() =>
                  onComposeFromPi("forward", msg.text, msg.contextMessageId)
                }
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Small pill-shaped action button shown below a completed assistant
 * bubble. Style picked to match the other "chip"-like elements in the
 * app (mail-link chips, account-filter chips) so it doesn't look like
 * a heavyweight form button.
 */
function ActionBadge({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] transition-colors"
      style={{
        borderColor: "var(--border-base)",
        color: "var(--accent)",
        background: "var(--bg-base)",
      }}
      onMouseEnter={(e) => {
        e.currentTarget.style.background = "var(--bg-hover)";
      }}
      onMouseLeave={(e) => {
        e.currentTarget.style.background = "var(--bg-base)";
      }}
    >
      {label}
    </button>
  );
}

/// Render assistant text with `cm:msg:<uuid>` references turned into
/// clickable chips. Plain text is whitespace-preserving (pre-wrap).
function AssistantText({
  text,
  streaming,
  onSelectMessage,
}: {
  text: string;
  streaming?: boolean;
  onSelectMessage: (id: string) => void;
}) {
  const nodes = useMemo(() => renderMailLinks(text, onSelectMessage), [text, onSelectMessage]);
  return (
    <div>
      <div className="whitespace-pre-wrap leading-relaxed">{nodes}</div>
      {streaming && (
        <span aria-hidden style={{ color: "var(--fg-subtle)" }}>
          ▌
        </span>
      )}
    </div>
  );
}

const MAIL_LINK_RE =
  /cm:msg:([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})/g;

function renderMailLinks(
  text: string,
  onSelectMessage: (id: string) => void,
): React.ReactNode[] {
  const out: React.ReactNode[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  MAIL_LINK_RE.lastIndex = 0;
  let keyCounter = 0;
  while ((match = MAIL_LINK_RE.exec(text)) !== null) {
    if (match.index > lastIndex) {
      out.push(
        <span key={`t${keyCounter++}`}>{text.slice(lastIndex, match.index)}</span>,
      );
    }
    const id = match[1];
    out.push(
      <button
        key={`l${keyCounter++}`}
        type="button"
        onClick={() => onSelectMessage(id)}
        className="inline-flex items-center gap-1 rounded border px-1.5 py-0.5 font-mono text-[11px]"
        style={{
          borderColor: "var(--border-base)",
          color: "var(--accent)",
          background: "var(--bg-base)",
          verticalAlign: "baseline",
        }}
        title={id}
      >
        ✉ {id.slice(0, 8)}
      </button>,
    );
    lastIndex = match.index + match[0].length;
  }
  if (lastIndex < text.length) {
    out.push(<span key={`t${keyCounter++}`}>{text.slice(lastIndex)}</span>);
  }
  return out;
}

// ─── context assembly ─────────────────────────────────────────────────────

async function buildContext({
  selectedMessageId,
  inbox,
  folderLabel,
}: {
  selectedMessageId?: string;
  inbox: EnvelopeSummary[];
  folderLabel: string;
}): Promise<string> {
  if (selectedMessageId) {
    const detail = await invoke<MessageDetail>("open_message", {
      messageId: selectedMessageId,
    });
    const env = detail.envelope;
    const body =
      (detail.plainText?.trim() ||
        (detail.htmlText ? stripHtmlToText(detail.htmlText) : "")) ??
      "";
    const truncated = body.length > 6000 ? body.slice(0, 6000) + "\n[…]" : body;
    return [
      "### Kontext: eine Mail",
      `ID: cm:msg:${env.id}`,
      `Von: ${env.from
        .map((a) => (a.name ? `${a.name} <${a.email}>` : a.email))
        .join(", ")}`,
      `An: ${env.to
        .map((a) => (a.name ? `${a.name} <${a.email}>` : a.email))
        .join(", ")}`,
      `Betreff: ${env.subject}`,
      `Datum: ${new Date(env.date).toISOString()}`,
      `Ordner: ${env.folderName}`,
      detail.attachments.length > 0
        ? `Anhänge: ${detail.attachments.map((a) => a.filename).join(", ")}`
        : "",
      "",
      "Nachrichtentext:",
      truncated || "(leer)",
    ]
      .filter(Boolean)
      .join("\n");
  }

  // Folder context: metadata table of up to 80 most recent envelopes.
  const head = inbox.slice(0, 80);
  const lines = head.map((m) => {
    const flags = [
      m.seen ? "" : "UNREAD",
      m.answered ? "ANS" : "",
      m.forwarded ? "FWD" : "",
      m.flagged ? "★" : "",
    ]
      .filter(Boolean)
      .join(",");
    const date = new Date(m.date).toISOString().slice(0, 16).replace("T", " ");
    return `- cm:msg:${m.id}  [${date}]  ${flags ? `(${flags}) ` : ""}${m.fromFirst} — ${m.subject}`;
  });
  return [
    `### Kontext: Ordner "${folderLabel}"`,
    `Insgesamt ${inbox.length} sichtbare Mails; ${Math.min(head.length, 80)} werden gelistet (neueste zuerst):`,
    "",
    ...lines,
  ].join("\n");
}
