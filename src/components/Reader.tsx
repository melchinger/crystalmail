import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { HOTKEY_EVENTS } from "../hooks/useHotkeys";
import { MoveToDialog } from "./MoveToDialog";
import { WorkflowPicker } from "./WorkflowPicker";
import {
  WorkflowPromptDialog,
  collectPromptParams,
} from "./WorkflowPromptDialog";
import { WorkflowResultDialog } from "./WorkflowResultDialog";
import { decodeImapFolderName } from "../utils/imapFolderName";
import {
  TRUSTED_SENDERS_CHANGED,
  addTrustedDomain,
  addTrustedSender,
  extractDomain,
  removeTrustedDomain,
  removeTrustedSender,
  trustReasonFor,
  type TrustReason,
} from "../utils/trustedSenders";
import {
  escapeHtml,
  extractHtmlBody,
  plainToHtml,
  sanitizeFragment,
  stripHtmlToText,
} from "../utils/mailHtml";
import type {
  Address,
  AccountSummary,
  AttachmentMeta,
  ComposeDraft,
  ContactLookup,
  EnvelopeDetail,
  ExtractionResult,
  FlagChanges,
  Flags,
  MessageDetail,
  WorkflowRunResult,
} from "../types";

/** Lokales Lade-Modell für den Header-Person-Icon-Status. Mehr States
 *  als das Backend-Pendant, weil wir hier auch "noch nicht gefragt"
 *  und "ist eine eigene Adresse" tracken müssen. */
type ContactLookupState =
  | { kind: "loading" }
  | { kind: "self" }
  | { kind: "contact"; contactId: string; displayName: string }
  | { kind: "history_only" }
  | { kind: "unknown" };

type Props = {
  selectedId?: string;
  accounts: AccountSummary[];
  onCompose: (draft: ComposeDraft) => void;
  onFlagsChanged: (id: string, flags: Flags) => void;
  /**
   * Request handlers — App owns the optimistic UI pop + background invoke
   * + rollback-on-failure. Reader only fires the intent; it doesn't wait
   * for any IMAP round-trip to finish before the view advances.
   */
  onArchiveRequest: (id: string) => void;
  onDeleteRequest: (id: string) => void;
  onMoveRequest: (id: string, folder: string) => void;
  onMarkSpamRequest: (id: string) => void;
  onSpamCandidateRequest: (id: string) => void;
  /** Klick auf das Person-Icon im Mail-Header → springt in die
   *  Kontakte-View und öffnet den Detail-Inspector. */
  onShowContact: (contactId: string) => void;
};

export function Reader({
  selectedId,
  accounts,
  onCompose,
  onFlagsChanged,
  onArchiveRequest,
  onDeleteRequest,
  onMoveRequest,
  onMarkSpamRequest,
  onSpamCandidateRequest,
  onShowContact,
}: Props) {
  const { t } = useTranslation();
  const [state, setState] = useState<
    | { kind: "idle" }
    | { kind: "loading" }
    | { kind: "ready"; detail: MessageDetail }
    | { kind: "error"; message: string }
  >({ kind: "idle" });

  // Auto-mark-seen timer: a short delay before setting \Seen so a quick
  // scroll-past doesn't pollute the read state.
  const autoSeenTimer = useRef<number | null>(null);


  useEffect(() => {
    if (!selectedId) {
      setState({ kind: "idle" });
      return;
    }
    let cancelled = false;
    setState({ kind: "loading" });
    (async () => {
      try {
        const detail = await invoke<MessageDetail>("open_message", {
          messageId: selectedId,
        });
        if (cancelled) return;
        setState({ kind: "ready", detail });

        if (!detail.envelope.seen) {
          if (autoSeenTimer.current) window.clearTimeout(autoSeenTimer.current);
          autoSeenTimer.current = window.setTimeout(() => {
            if (cancelled) return;
            // Optimistic: mark seen in the local Reader state and
            // propagate to App (which adjusts the sidebar counter)
            // before the IMAP STORE round-trip. Keeps the badge in
            // step with the visible state of the open mail.
            const env = detail.envelope;
            const optimisticFlags: Flags = {
              seen: true,
              answered: env.answered,
              flagged: env.flagged,
              forwarded: env.forwarded,
              junk: env.junk,
              draft: false,
              deleted: false,
            };
            setState((s) =>
              s.kind === "ready" && s.detail.envelope.id === env.id
                ? mergeFlags(s, optimisticFlags)
                : s,
            );
            onFlagsChanged(env.id, optimisticFlags);

            // Reconcile with the authoritative server response —
            // happy path is a no-op since the flags match.
            void applyFlagChange(env.id, { seen: true }).then((flags) => {
              if (cancelled || !flags) return;
              setState((s) =>
                s.kind === "ready" && s.detail.envelope.id === env.id
                  ? mergeFlags(s, flags)
                  : s,
              );
              onFlagsChanged(env.id, flags);
            });
          }, 1200);
        }
      } catch (e) {
        // "cancelled" is the sentinel we get when the user archived /
        // deleted / moved the mail while its body was still loading.
        // Not an error the user needs to see — the view has already
        // advanced to whatever replaced it.
        if (cancelled) return;
        const msg = String(e);
        if (msg.includes("cancelled")) {
          setState({ kind: "idle" });
          return;
        }
        setState({ kind: "error", message: msg });
      }
    })();
    return () => {
      cancelled = true;
      if (autoSeenTimer.current) {
        window.clearTimeout(autoSeenTimer.current);
        autoSeenTimer.current = null;
      }
    };
    // onFlagsChanged intentionally not in deps — it's a stable reference-via-useCallback on parent
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  if (state.kind === "idle") {
    return (
      <EmptyFrame>
        <div className="max-w-sm px-6 text-center">
          {/* App logo as the idle-state anchor — same asset we ship as
              the Tauri app icon and use in the splash screen. Slightly
              dimmed so it reads as decoration, not as a call-to-action. */}
          <img
            src="/crystalmail-logo.png"
            alt=""
            aria-hidden
            className="mx-auto mb-4 h-20 w-20"
            style={{ opacity: 0.55 }}
          />
          <p className="text-sm" style={{ color: "var(--fg-muted)" }}>
            {t("reader.noSelection")}
          </p>
        </div>
      </EmptyFrame>
    );
  }

  if (state.kind === "loading") {
    return (
      <EmptyFrame>
        <p className="text-sm" style={{ color: "var(--fg-muted)" }}>
          {t("reader.loading")}
        </p>
      </EmptyFrame>
    );
  }

  if (state.kind === "error") {
    return (
      <ErrorView
        message={state.message}
        selectedId={selectedId}
        onArchiveRequest={onArchiveRequest}
        onDeleteRequest={onDeleteRequest}
      />
    );
  }

  return (
    <MessageView
      detail={state.detail}
      accounts={accounts}
      onCompose={onCompose}
      onLocalFlagsUpdate={(flags) => {
        setState((s) =>
          s.kind === "ready" ? mergeFlags(s, flags) : s,
        );
        onFlagsChanged(state.detail.envelope.id, flags);
      }}
      onArchiveRequest={onArchiveRequest}
      onDeleteRequest={onDeleteRequest}
      onMoveRequest={onMoveRequest}
      onMarkSpamRequest={onMarkSpamRequest}
      onSpamCandidateRequest={onSpamCandidateRequest}
      onShowContact={onShowContact}
    />
  );
}

function mergeFlags(
  s: { kind: "ready"; detail: MessageDetail },
  flags: Flags,
): { kind: "ready"; detail: MessageDetail } {
  return {
    kind: "ready",
    detail: {
      ...s.detail,
      envelope: {
        ...s.detail.envelope,
        seen: flags.seen,
        answered: flags.answered,
        flagged: flags.flagged,
        forwarded: flags.forwarded,
      },
    },
  };
}

async function applyFlagChange(
  messageId: string,
  changes: FlagChanges,
): Promise<Flags | null> {
  try {
    return await invoke<Flags>("set_message_flags", {
      messageId,
      changes,
    });
  } catch (e) {
    console.error("set_message_flags failed:", e);
    return null;
  }
}

function EmptyFrame({ children }: { children: React.ReactNode }) {
  return (
    <section
      className="flex flex-1 items-center justify-center"
      style={{ background: "var(--bg-panel)" }}
    >
      {children}
    </section>
  );
}

/**
 * Error state for the Reader: shows whatever string the backend returned,
 * plus delete/archive buttons that work even when the body never loaded.
 *
 * The MessageView (the "happy" Reader body) owns the global hotkey
 * listeners for delete/archive/move — so without this component, hitting
 * Delete on a ghost mail (body fetch failed) does literally nothing,
 * because no MessageView is mounted to receive the event. ErrorView
 * fills that gap: it registers minimal hotkey listeners (delete +
 * archive) and renders matching buttons so the user can clean up
 * either via keyboard or mouse.
 *
 * The buttons go through the same `onDeleteRequest` / `onArchiveRequest`
 * the parent (App.tsx) passes down — those run the optimistic-removal
 * pipeline, which means the row pops out of the inbox immediately.
 * Backend-side, `message_ops` does a UID-existence pre-check and
 * succeeds with local-only cleanup when the UID is gone server-side.
 */
function ErrorView({
  message,
  selectedId,
  onArchiveRequest,
  onDeleteRequest,
}: {
  message: string;
  selectedId?: string;
  onArchiveRequest: (id: string) => void;
  onDeleteRequest: (id: string) => void;
}) {
  const { t } = useTranslation();

  useEffect(() => {
    if (!selectedId) return;
    const onDelete = () => onDeleteRequest(selectedId);
    const onArchive = () => onArchiveRequest(selectedId);
    window.addEventListener(HOTKEY_EVENTS.delete, onDelete);
    window.addEventListener(HOTKEY_EVENTS.archive, onArchive);
    return () => {
      window.removeEventListener(HOTKEY_EVENTS.delete, onDelete);
      window.removeEventListener(HOTKEY_EVENTS.archive, onArchive);
    };
  }, [selectedId, onDeleteRequest, onArchiveRequest]);

  return (
    <section
      className="flex flex-1 flex-col items-center justify-center px-6"
      style={{ background: "var(--bg-panel)" }}
    >
      <p
        className="max-w-md text-center text-sm"
        style={{ color: "#ef4444" }}
      >
        {message}
      </p>
      {selectedId && (
        <div className="mt-4 flex items-center gap-2">
          <button
            type="button"
            onClick={() => onDeleteRequest(selectedId)}
            className="rounded-md border px-3 py-1.5 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
              background: "var(--bg-base)",
            }}
            title={t("reader.errorDeleteHint")}
          >
            {t("reader.errorDelete")}
          </button>
          <button
            type="button"
            onClick={() => onArchiveRequest(selectedId)}
            className="rounded-md border px-3 py-1.5 text-xs"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
              background: "var(--bg-base)",
            }}
            title={t("reader.errorArchiveHint")}
          >
            {t("reader.errorArchive")}
          </button>
        </div>
      )}
    </section>
  );
}

function MessageView({
  detail,
  accounts,
  onCompose,
  onLocalFlagsUpdate,
  onArchiveRequest,
  onDeleteRequest,
  onMoveRequest,
  onMarkSpamRequest,
  onSpamCandidateRequest,
  onShowContact,
}: {
  detail: MessageDetail;
  accounts: AccountSummary[];
  onCompose: (draft: ComposeDraft) => void;
  onLocalFlagsUpdate: (flags: Flags) => void;
  onArchiveRequest: (id: string) => void;
  onDeleteRequest: (id: string) => void;
  onMoveRequest: (id: string, folder: string) => void;
  onMarkSpamRequest: (id: string) => void;
  onSpamCandidateRequest: (id: string) => void;
  onShowContact: (contactId: string) => void;
}) {
  const { t } = useTranslation();
  const [showDetails, setShowDetails] = useState(false);
  const [movePickerOpen, setMovePickerOpen] = useState(false);
  const [workflowPickerOpen, setWorkflowPickerOpen] = useState(false);
  // Last workflow run result (per message) — stays up until the user
  // dismisses it so they can read stdout/stderr for a failed step.
  // `applying` guards against double-trigger when a user hammers the
  // hotkey; we don't disable the picker, just ignore re-triggers.
  const [workflowResult, setWorkflowResult] = useState<{
    result: WorkflowRunResult;
    name: string;
  } | null>(null);
  const [workflowApplying, setWorkflowApplying] = useState(false);
  // Wenn der gewählte Workflow Prompt-Source-Params hat, blendet
  // dieser State den Pre-Apply-Dialog ein. `null` = kein Dialog
  // offen; sonst trägt er das Workflow-Objekt für die Anzeige.
  const [pendingWorkflow, setPendingWorkflow] = useState<
    import("../types").Workflow | null
  >(null);
  // Lookup-Status der From-Adresse fürs Contact-Icon im Header.
  const [fromLookup, setFromLookup] = useState<ContactLookupState>({
    kind: "loading",
  });
  const [extractingContact, setExtractingContact] = useState(false);
  const [extractionToast, setExtractionToast] = useState<string | null>(null);
  const env = detail.envelope;
  const account = accounts.find((a) => a.id === env.accountId);

  // Contact-Lookup-Effekt: bei jedem Mail-Wechsel (env.id) holen wir
  // den Status und entscheiden ob's ein bekannter Kontakt, eine reine
  // History-Adresse oder unbekannt ist. Eigene Adressen kurzschließen
  // wir client-side um den DB-Roundtrip zu sparen.
  useEffect(() => {
    const fromAddr = env.from[0]?.email?.toLowerCase().trim();
    if (!fromAddr) {
      setFromLookup({ kind: "unknown" });
      return;
    }
    const ownEmails = new Set<string>();
    for (const a of accounts) {
      ownEmails.add(a.address.toLowerCase());
      for (const al of a.aliases) ownEmails.add(al.email.toLowerCase());
    }
    if (ownEmails.has(fromAddr)) {
      setFromLookup({ kind: "self" });
      return;
    }
    setFromLookup({ kind: "loading" });
    let cancelled = false;
    void (async () => {
      try {
        const result = await invoke<ContactLookup>("lookup_contact_by_email", {
          email: fromAddr,
        });
        if (cancelled) return;
        if (result.kind === "contact") {
          setFromLookup({
            kind: "contact",
            contactId: result.contact.id,
            displayName: result.contact.displayName,
          });
        } else if (result.kind === "history_only") {
          setFromLookup({ kind: "history_only" });
        } else {
          setFromLookup({ kind: "unknown" });
        }
      } catch (e) {
        console.warn("contact lookup failed:", e);
        if (!cancelled) setFromLookup({ kind: "unknown" });
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [env.id, accounts]);

  /** Klick auf das Person-Icon im Header. */
  const onContactIconClick = async () => {
    if (fromLookup.kind === "contact") {
      onShowContact(fromLookup.contactId);
      return;
    }
    setExtractingContact(true);
    setExtractionToast(null);
    try {
      const result = await invoke<ExtractionResult>(
        "extract_contact_from_message",
        { messageId: env.id },
      );
      switch (result.kind) {
        case "created":
        case "already_exists":
          onShowContact(result.contactId);
          break;
        case "empty":
          setExtractionToast(t("contacts.extractEmpty"));
          break;
        case "not_applicable":
          setExtractionToast(
            t("contacts.extractNotApplicable", { reason: result.reason }),
          );
          break;
        case "skipped":
          setExtractionToast(
            t("contacts.extractSkipped", { reason: result.reason }),
          );
          break;
      }
    } catch (e) {
      setExtractionToast(t("contacts.extractFailed", { detail: String(e) }));
    } finally {
      setExtractingContact(false);
    }
  };

  // Workflow-training toggle wired to the `t` hotkey. Flat
   // round-trip: probe → flip in DB → broadcast so the App can
   // refresh its `trainingIds` set that feeds the inbox TRAIN badge.
   // No local state because the badge lives in the list, not here.
  const toggleTrainingCandidate = async () => {
    try {
      const isCurrently = await invoke<boolean>(
        "is_workflow_training_candidate",
        { messageId: env.id },
      );
      await invoke(
        isCurrently ? "remove_workflow_training" : "add_workflow_training",
        { messageIds: [env.id] },
      );
      // Broadcast so the App-level listener repopulates the Set —
      // list badges flip immediately without a full refresh.
      window.dispatchEvent(new CustomEvent("cm:training:changed"));
    } catch (e) {
      console.warn("training toggle failed", e);
    }
  };

  const onReply = () => onCompose(buildReplyDraft(detail, account));
  const onReplyAll = () => onCompose(buildReplyAllDraft(detail, account));
  const onForward = () => onCompose(buildForwardDraft(detail, account));

  /**
   * Flag toggle with optimistic UI. The Reader's own detail state and
   * the App's inbox state flip *immediately* — including the sidebar
   * unread counter that rides on `onLocalFlagsUpdate`. The backend
   * call runs in the background; on failure we roll back to the
   * pre-click flags.
   *
   * Doesn't gate on `mutating` any more: the update is local and
   * instant, so rapid `u u` presses can't race themselves, they just
   * collapse to the last requested state.
   */
  const toggleFlag = (changes: FlagChanges) => {
    const optimistic: Flags = {
      seen: changes.seen ?? env.seen,
      answered: changes.answered ?? env.answered,
      flagged: changes.flagged ?? env.flagged,
      forwarded: changes.forwarded ?? env.forwarded,
      junk: changes.junk ?? env.junk,
      // Draft/Deleted aren't tracked in the envelope detail — keep
      // them false so the downstream merge logic has a full Flags.
      draft: false,
      deleted: false,
    };
    const previous: Flags = {
      seen: env.seen,
      answered: env.answered,
      flagged: env.flagged,
      forwarded: env.forwarded,
      junk: env.junk,
      draft: false,
      deleted: false,
    };
    // Paint the new state right away.
    onLocalFlagsUpdate(optimistic);

    void applyFlagChange(env.id, changes).then((flags) => {
      if (flags) {
        // Reconcile with the authoritative server response — should
        // match `optimistic` in the happy case.
        onLocalFlagsUpdate(flags);
      } else {
        // Backend returned null (= error). Revert.
        onLocalFlagsUpdate(previous);
      }
    });
  };

  // Archive/delete/move are "fire-and-forget" from the Reader's point of
  // view: they hand the intent up to App which pops the envelope out of
  // the inbox list immediately and runs the IMAP op in the background.
  // No await, no mutating gate — the Reader is already re-rendering the
  // next message by the time the backend starts working.
  const onArchive = () => onArchiveRequest(env.id);
  const onDelete = () => onDeleteRequest(env.id);
  const onMarkSpam = () => onMarkSpamRequest(env.id);
  const onSpamCandidate = () => onSpamCandidateRequest(env.id);
  const onMoveTo = (folder: string) => {
    setMovePickerOpen(false);
    onMoveRequest(env.id, folder);
  };
  /** Eigentlicher Apply-Aufruf. Erwartet die optionalen Prompt-Werte
   *  bereits eingesammelt (oder ein leeres Objekt für Workflows ohne
   *  Prompt-Params). */
  const runWorkflow = async (
    workflowId: string,
    name: string,
    promptValues: Record<string, string>,
  ) => {
    setWorkflowApplying(true);
    try {
      const result = await invoke<WorkflowRunResult>("apply_workflow", {
        workflowId,
        messageId: env.id,
        promptValues,
      });
      setWorkflowResult({ result, name });
    } catch (e) {
      setWorkflowResult({
        result: {
          workflowId,
          messageId: env.id,
          allOk: false,
          steps: [
            {
              stepIndex: 0,
              stepType: "runScript",
              ok: false,
              message: String(e),
              detail: null,
            },
          ],
        },
        name,
      });
    } finally {
      setWorkflowApplying(false);
    }
  };

  const onPickWorkflow = async (workflowId: string) => {
    setWorkflowPickerOpen(false);
    if (workflowApplying) return;
    try {
      // Workflow vollständig laden (für Step-Definitionen) — der
      // Picker hat nur Header-Infos.
      const list = await invoke<import("../types").Workflow[]>(
        "list_workflows",
      );
      const wf = list.find((w) => w.id === workflowId);
      if (!wf) return;
      // Prompt-Params? → Dialog vorne dran. Sonst direkt durchstarten.
      if (collectPromptParams(wf).length > 0) {
        setPendingWorkflow(wf);
        return;
      }
      void runWorkflow(workflowId, wf.name, {});
    } catch (e) {
      // Wrap top-level errors into the same dialog shape so the user
      // sees them in context. A single synthetic failed step carries
      // the message — no separate error-toast path to maintain.
      setWorkflowResult({
        result: {
          workflowId,
          messageId: env.id,
          allOk: false,
          steps: [
            {
              stepIndex: 0,
              stepType: "runScript",
              ok: false,
              message: String(e),
              detail: null,
            },
          ],
        },
        name: "",
      });
    } finally {
      setWorkflowApplying(false);
    }
  };

  // Subscribe to global hotkey events. Each handler is keyed off the
  // current message detail via closures, so the listeners are re-bound
  // whenever the selected message changes.
  useEffect(() => {
    const handlers: [string, () => void][] = [
      [HOTKEY_EVENTS.reply, onReply],
      [HOTKEY_EVENTS.replyAll, onReplyAll],
      [HOTKEY_EVENTS.forward, onForward],
      [HOTKEY_EVENTS.archive, () => void onArchive()],
      [HOTKEY_EVENTS.delete, () => void onDelete()],
      [HOTKEY_EVENTS.move, () => setMovePickerOpen(true)],
      [HOTKEY_EVENTS.markSpam, onMarkSpam],
      [HOTKEY_EVENTS.spamCandidate, onSpamCandidate],
      [HOTKEY_EVENTS.toggleRead, () => toggleFlag({ seen: !env.seen })],
      [HOTKEY_EVENTS.markUnread, () => toggleFlag({ seen: false })],
      [HOTKEY_EVENTS.workflow, () => setWorkflowPickerOpen(true)],
      [
        HOTKEY_EVENTS.trainingCandidate,
        () => void toggleTrainingCandidate(),
      ],
    ];
    handlers.forEach(([name, fn]) => window.addEventListener(name, fn));
    return () => {
      handlers.forEach(([name, fn]) =>
        window.removeEventListener(name, fn),
      );
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail, env.seen]);

  return (
    <section
      className="flex min-w-0 flex-1 flex-col"
      style={{ background: "var(--bg-panel)" }}
    >
      {/* Toolbar — its own row so actions never fight the subject for space.
          Grouped left→right: reply-ish, organize, status. */}
      <div
        className="flex items-center gap-1 border-b px-4 py-2"
        style={{ borderColor: "var(--border-soft)" }}
      >
        <ToolbarButton
          onClick={onReply}
          icon={<IconReply />}
          label={t("reader.reply")}
          hotkey="r"
          primary
        />
        <ToolbarButton
          onClick={onReplyAll}
          icon={<IconReplyAll />}
          label={t("reader.replyAll")}
          hotkey="a"
          primary
        />
        <ToolbarButton
          onClick={onForward}
          icon={<IconForward />}
          label={t("reader.forward")}
          hotkey="f"
          primary
        />

        <ToolbarDivider />

        <ToolbarIconButton
          onClick={onArchive}
          icon={<IconArchive />}
          title={t("reader.archive")}
          hotkey="e"
        />
        <ToolbarIconButton
          onClick={onDelete}
          icon={<IconTrash />}
          title={t("reader.delete")}
          hotkey="Del"
          tone="danger"
        />
        <ToolbarIconButton
          onClick={() => setMovePickerOpen(true)}
          icon={<IconMove />}
          title={t("reader.moveTo")}
          hotkey="v"
        />
        <ToolbarIconButton
          onClick={onMarkSpam}
          icon={<IconSpam />}
          title={t("reader.markSpam")}
          hotkey="!"
          tone="danger"
        />

        <div className="ml-auto flex items-center gap-1">
          <ToolbarIconButton
            onClick={() => toggleFlag({ seen: !env.seen })}
            icon={env.seen ? <IconEnvelopeOpen /> : <IconEnvelope />}
            title={
              env.seen ? t("reader.markUnread") : t("reader.markRead")
            }
            hotkey="u"
            active={!env.seen}
          />
          <ToolbarIconButton
            onClick={() => toggleFlag({ flagged: !env.flagged })}
            icon={<IconStar filled={env.flagged} />}
            title={env.flagged ? t("reader.unflag") : t("reader.flag")}
            active={env.flagged}
            tone={env.flagged ? "amber" : undefined}
          />
        </div>
      </div>

      <header
        className="border-b px-6 py-4"
        style={{ borderColor: "var(--border-soft)" }}
      >
        {/* Subject + date. Subject gets full width now; date sits top-right
            as a lighter secondary anchor. */}
        <div className="mb-3 flex items-start justify-between gap-4">
          <h1
            className="min-w-0 flex-1 text-lg font-semibold leading-snug"
            style={{ color: "var(--fg-base)" }}
          >
            {env.subject || "(kein Betreff)"}
          </h1>
          <time
            className="shrink-0 pt-0.5 text-[11px] font-mono"
            style={{ color: "var(--fg-subtle)" }}
            dateTime={env.date}
          >
            {new Date(env.date).toLocaleString()}
          </time>
        </div>

        {/* Sender row — avatar + name + email. Primary identity line.
            Plus Person-Icon: zeigt den Contact-Status (Kontakt vorhanden /
            unbekannt) und reagiert auf Klick mit Detail-Sprung oder
            Auto-Extraction-Trigger. Bei Self-Mail blenden wir's aus. */}
        <div className="mb-2 flex items-center gap-3">
          <Avatar
            color={account?.color}
            name={env.from[0]?.name ?? env.from[0]?.email ?? "?"}
          />
          <div className="min-w-0 flex-1">
            <div
              className="truncate text-sm"
              style={{ color: "var(--fg-base)", fontWeight: 500 }}
            >
              {env.from[0]?.name || env.from[0]?.email || "—"}
            </div>
            {env.from[0]?.name && (
              <div
                className="truncate text-[11px]"
                style={{ color: "var(--fg-muted)" }}
              >
                {env.from[0].email}
              </div>
            )}
          </div>
          {fromLookup.kind !== "self" && fromLookup.kind !== "loading" && (
            <button
              type="button"
              onClick={() => void onContactIconClick()}
              disabled={extractingContact}
              className="shrink-0 rounded-md border px-2 py-1 text-[11px] disabled:opacity-50"
              style={{
                borderColor:
                  fromLookup.kind === "contact"
                    ? "var(--accent)"
                    : "var(--border-base)",
                color:
                  fromLookup.kind === "contact"
                    ? "var(--accent)"
                    : "var(--fg-muted)",
                background: "transparent",
              }}
              title={
                fromLookup.kind === "contact"
                  ? t("contacts.openContact")
                  : t("contacts.extractFromMailHint")
              }
            >
              {extractingContact
                ? "…"
                : fromLookup.kind === "contact"
                  ? `👤 ${fromLookup.displayName}`
                  : `+ ${t("contacts.extractFromMail")}`}
            </button>
          )}
        </div>
        {extractionToast && (
          <div
            className="mb-2 rounded-md px-3 py-2 text-xs"
            style={{
              background: "var(--bg-base)",
              borderLeft: "3px solid var(--accent)",
              color: "var(--fg-muted)",
            }}
          >
            <div className="flex items-start justify-between gap-2">
              <span>{extractionToast}</span>
              <button
                type="button"
                onClick={() => setExtractionToast(null)}
                className="shrink-0"
                style={{ color: "var(--fg-subtle)" }}
                aria-label="dismiss"
              >
                ✕
              </button>
            </div>
          </div>
        )}

        {/* Recipients compressed into one line; full list in the details
            expander. */}
        <RecipientLine
          label={t("reader.to")}
          list={env.to}
          fgMuted="var(--fg-muted)"
          fgSubtle="var(--fg-subtle)"
        />
        {env.cc.length > 0 && (
          <RecipientLine
            label={t("reader.cc")}
            list={env.cc}
            fgMuted="var(--fg-muted)"
            fgSubtle="var(--fg-subtle)"
          />
        )}

        {/* Status chip row: account + folder + thread-state. No more
            "BEANTWORTET"/"WEITERGELEITET" capital labels — just the glyph. */}
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {account && <Badge color={account.color}>{account.displayName}</Badge>}
          <Badge>{decodeImapFolderName(env.folderName)}</Badge>
          {env.answered && (
            <Badge tone="muted" title={t("reader.answered")}>
              ↩
            </Badge>
          )}
          {env.forwarded && (
            <Badge tone="muted" title={t("reader.forwarded")}>
              ↪
            </Badge>
          )}
          {env.flagged && <Badge tone="amber">★</Badge>}

          <button
            type="button"
            onClick={() => setShowDetails((v) => !v)}
            className="ml-auto inline-flex items-center gap-1 rounded px-1 text-[11px] hover:underline"
            style={{ color: "var(--fg-subtle)" }}
          >
            <span
              aria-hidden
              style={{
                display: "inline-block",
                transform: showDetails ? "rotate(90deg)" : "rotate(0deg)",
                transition: "transform 120ms",
              }}
            >
              ▸
            </span>
            {showDetails ? t("reader.hideDetails") : t("reader.details")}
          </button>
        </div>

        {showDetails && (
          <div
            className="mt-3 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 rounded-md border px-3 py-2 text-xs"
            style={{
              borderColor: "var(--border-soft)",
              background: "var(--bg-base)",
              color: "var(--fg-muted)",
            }}
          >
            {account && (
              <MetaRow label={t("reader.account")} value={account.address} />
            )}
            <MetaRow label={t("reader.folder")} value={decodeImapFolderName(env.folderName)} />
            <MetaRow label={t("reader.uid")} value={String(env.imapUid)} />
            {env.messageIdHeader && (
              <MetaRow
                label={t("reader.messageId")}
                value={env.messageIdHeader}
              />
            )}
          </div>
        )}
      </header>

      <AttachmentBar messageId={env.id} attachments={detail.attachments} />

      <div className="min-h-0 flex-1 overflow-hidden">
        <BodyView
          messageId={env.id}
          senderEmail={env.from[0]?.email ?? null}
          plain={detail.plainText}
          html={detail.htmlText}
          attachments={detail.attachments}
          fallback={t("reader.noBody")}
        />
      </div>

      {movePickerOpen && (
        <MoveToDialog
          accountId={env.accountId}
          currentFolder={env.folderName}
          onPick={(folder) => void onMoveTo(folder)}
          onClose={() => setMovePickerOpen(false)}
        />
      )}

      {workflowPickerOpen && (
        <WorkflowPicker
          onClose={() => setWorkflowPickerOpen(false)}
          onPick={(id) => void onPickWorkflow(id)}
        />
      )}

      {pendingWorkflow && (
        <WorkflowPromptDialog
          workflow={pendingWorkflow}
          envelope={{
            id: env.id,
            accountId: env.accountId,
            accountColor: account?.color ?? "",
            folderId: env.folderId,
            subject: env.subject,
            // Reader-Detail hat `from` als Address[] — wir bauen die
            // gleiche "Name <email>"-Form zurück, die EnvelopeSummary
            // im Listing trägt; das WorkflowPromptDialog macht
            // einfaches `$from`-Substitute darauf.
            fromFirst:
              env.from[0]
                ? env.from[0].name
                  ? `${env.from[0].name} <${env.from[0].email}>`
                  : env.from[0].email
                : "",
            date: env.date,
            seen: env.seen,
            answered: env.answered,
            flagged: env.flagged,
            forwarded: env.forwarded,
            junk: env.junk,
            bodyCached: env.bodyCached,
            hasAttachments: false,
            scheduled: null,
          }}
          onCancel={() => setPendingWorkflow(null)}
          onSubmit={(values) => {
            const wf = pendingWorkflow;
            setPendingWorkflow(null);
            void runWorkflow(wf.id, wf.name, values);
          }}
        />
      )}

      {workflowResult && (
        <WorkflowResultDialog
          result={workflowResult.result}
          workflowName={workflowResult.name}
          onClose={() => setWorkflowResult(null)}
        />
      )}
    </section>
  );
}

function AttachmentBar({
  messageId,
  attachments,
}: {
  messageId: string;
  attachments: AttachmentMeta[];
}) {
  const { t } = useTranslation();
  // Inline parts (images referenced via cid:) are decorative — no chip needed.
  const visible = attachments.filter((a) => !a.isInline);
  const [busyIdx, setBusyIdx] = useState<number | null>(null);
  const [toast, setToast] = useState<string | null>(null);

  if (visible.length === 0) return null;

  const saveOne = async (att: AttachmentMeta) => {
    if (busyIdx !== null) return;
    setBusyIdx(att.partIdx);
    try {
      const dest = await saveDialog({
        defaultPath: att.filename,
        title: t("attachments.saveAs"),
      });
      if (!dest) return;
      const written = await invoke<string>("save_attachment", {
        messageId,
        partIdx: att.partIdx,
        destination: dest,
      });
      setToast(t("attachments.savedTo", { path: written }));
      window.setTimeout(() => setToast(null), 3200);
    } catch (e) {
      setToast(t("common.error", { message: String(e) }));
      window.setTimeout(() => setToast(null), 4500);
    } finally {
      setBusyIdx(null);
    }
  };

  /**
   * Decode the attachment, drop it into a per-message temp directory, and
   * hand the path to the OS default app (PDF viewer, Word, image viewer …).
   * The chip's primary click triggers this — saving stays available via
   * the small ⤓ icon next to each chip. Idempotent: re-opening the same
   * attachment writes to the same path, so an already-open viewer instance
   * may even pick up the existing tab.
   */
  const openOne = async (att: AttachmentMeta) => {
    if (busyIdx !== null) return;
    setBusyIdx(att.partIdx);
    try {
      await invoke<string>("open_attachment", {
        messageId,
        partIdx: att.partIdx,
      });
      // Don't show a "opened from …" toast on success — the user's
      // viewer popping up is feedback enough, and the temp path is
      // implementation noise. Errors do get a toast.
    } catch (e) {
      setToast(t("common.error", { message: String(e) }));
      window.setTimeout(() => setToast(null), 4500);
    } finally {
      setBusyIdx(null);
    }
  };

  return (
    <div
      className="flex flex-wrap items-center gap-1.5 border-b px-6 py-2"
      style={{
        borderColor: "var(--border-soft)",
        background: "var(--bg-base)",
      }}
    >
      <span
        className="text-[10px] uppercase tracking-wider"
        style={{ color: "var(--fg-subtle)" }}
      >
        {t("attachments.label", { count: visible.length })}
      </span>
      {visible.map((att) => (
        <span
          key={att.partIdx}
          className="inline-flex items-stretch overflow-hidden rounded-md border text-[11px]"
          style={{
            borderColor: "var(--border-base)",
            background: "transparent",
            opacity: busyIdx !== null && busyIdx !== att.partIdx ? 0.5 : 1,
          }}
        >
          {/* Primary action: open with the OS default app. The chip body
              is the big hit target so the common case is one click. */}
          <button
            type="button"
            onClick={() => void openOne(att)}
            disabled={busyIdx !== null}
            title={t("attachments.openTooltip", {
              mime: att.mimeType,
              size: formatBytes(att.sizeBytes),
            })}
            className="inline-flex items-center gap-1.5 px-2 py-0.5 transition-colors disabled:opacity-50"
            style={{ color: "var(--fg-base)" }}
            onMouseEnter={(e) => {
              if (busyIdx === null)
                e.currentTarget.style.background = "var(--bg-hover)";
            }}
            onMouseLeave={(e) =>
              (e.currentTarget.style.background = "transparent")
            }
          >
            <span aria-hidden>📎</span>
            <span className="max-w-[20rem] truncate">{att.filename}</span>
            <span style={{ color: "var(--fg-subtle)" }}>
              {formatBytes(att.sizeBytes)}
            </span>
          </button>
          {/* Secondary action: save-as dialog. Smaller hit target so it
              doesn't compete visually with the open action, but still
              fully keyboard-reachable (it's a separate <button>). */}
          <button
            type="button"
            onClick={() => void saveOne(att)}
            disabled={busyIdx !== null}
            title={t("attachments.saveTooltip")}
            aria-label={t("attachments.saveTooltip")}
            className="inline-flex items-center border-l px-1.5 transition-colors disabled:opacity-50"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-subtle)",
            }}
            onMouseEnter={(e) => {
              if (busyIdx === null) {
                e.currentTarget.style.background = "var(--bg-hover)";
                e.currentTarget.style.color = "var(--fg-base)";
              }
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.background = "transparent";
              e.currentTarget.style.color = "var(--fg-subtle)";
            }}
          >
            <span aria-hidden>⤓</span>
          </button>
        </span>
      ))}
      {toast && (
        <span
          className="ml-auto text-[11px]"
          style={{ color: "var(--fg-subtle)" }}
        >
          {toast}
        </span>
      )}
    </div>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

// ─── Toolbar primitives ─────────────────────────────────────────────────────
//
// Two button flavors:
//   * ToolbarButton  — icon + label, used for the three primary compose
//                      actions (Reply / Reply All / Forward).
//   * ToolbarIconButton — icon only, used for organize (archive/delete) and
//                      status toggles (read/flag). Tooltip supplies the
//                      label + hotkey.
// A shared hover treatment keeps the whole row visually coherent.

function ToolbarButton({
  onClick,
  icon,
  label,
  hotkey,
  disabled,
  primary,
}: {
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
  hotkey?: string;
  disabled?: boolean;
  primary?: boolean;
}) {
  const title = hotkey ? `${label} (${hotkey})` : label;
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      className="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs transition-colors disabled:opacity-50"
      style={{
        color: "var(--fg-base)",
        background: "transparent",
        fontWeight: primary ? 500 : 400,
      }}
      onMouseEnter={(e) => {
        if (!disabled) e.currentTarget.style.background = "var(--bg-hover)";
      }}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      <span className="inline-flex h-4 w-4 items-center justify-center" aria-hidden>
        {icon}
      </span>
      <span>{label}</span>
    </button>
  );
}

function ToolbarIconButton({
  onClick,
  icon,
  title,
  hotkey,
  disabled,
  active,
  tone,
}: {
  onClick: () => void;
  icon: React.ReactNode;
  title: string;
  hotkey?: string;
  disabled?: boolean;
  active?: boolean;
  tone?: "danger" | "amber";
}) {
  const tip = hotkey ? `${title} (${hotkey})` : title;
  const activeColor =
    tone === "amber"
      ? "#f59e0b"
      : tone === "danger"
        ? "var(--fg-base)"
        : "var(--accent)";
  const hoverColor =
    tone === "danger" ? "#ef4444" : active ? activeColor : "var(--fg-base)";
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={tip}
      className="inline-flex h-8 w-8 items-center justify-center rounded-md transition-colors disabled:opacity-50"
      style={{
        color: active ? activeColor : "var(--fg-muted)",
        background: active ? "var(--bg-hover)" : "transparent",
      }}
      onMouseEnter={(e) => {
        if (!disabled) {
          e.currentTarget.style.background = "var(--bg-hover)";
          e.currentTarget.style.color = hoverColor;
        }
      }}
      onMouseLeave={(e) => {
        e.currentTarget.style.background = active
          ? "var(--bg-hover)"
          : "transparent";
        e.currentTarget.style.color = active
          ? activeColor
          : "var(--fg-muted)";
      }}
    >
      <span className="inline-flex h-4 w-4 items-center justify-center" aria-hidden>
        {icon}
      </span>
    </button>
  );
}

function ToolbarDivider() {
  return (
    <span
      aria-hidden
      className="mx-1 inline-block h-5 w-px"
      style={{ background: "var(--border-base)" }}
    />
  );
}

// ─── Icons (inline SVG, lucide-ish geometry for consistency) ────────────────

function Svg({ children }: { children: React.ReactNode }) {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {children}
    </svg>
  );
}

const IconReply = () => (
  <Svg>
    <polyline points="9 17 4 12 9 7" />
    <path d="M20 18v-2a4 4 0 0 0-4-4H4" />
  </Svg>
);

const IconReplyAll = () => (
  <Svg>
    <polyline points="7 17 2 12 7 7" />
    <polyline points="12 17 7 12 12 7" />
    <path d="M22 18v-2a4 4 0 0 0-4-4H7" />
  </Svg>
);

const IconForward = () => (
  <Svg>
    <polyline points="15 17 20 12 15 7" />
    <path d="M4 18v-2a4 4 0 0 1 4-4h12" />
  </Svg>
);

const IconSpam = () => (
  <Svg>
    {/* shield with exclamation — "mark as spam / watch out" glyph */}
    <path d="M12 3l7 3v6c0 4-3 7.5-7 9-4-1.5-7-5-7-9V6z" />
    <line x1="12" y1="9" x2="12" y2="13" />
    <circle cx="12" cy="16" r="0.6" fill="currentColor" stroke="none" />
  </Svg>
);

const IconMove = () => (
  <Svg>
    {/* folder with arrow — "move into folder" glyph */}
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
    <polyline points="10 14 13 11 10 8" />
    <line x1="13" y1="11" x2="7" y2="11" />
  </Svg>
);

const IconArchive = () => (
  <Svg>
    <rect x="2" y="3" width="20" height="5" rx="1" />
    <path d="M4 8v12a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V8" />
    <line x1="10" y1="13" x2="14" y2="13" />
  </Svg>
);

const IconTrash = () => (
  <Svg>
    <polyline points="3 6 5 6 21 6" />
    <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
    <path d="M10 11v6" />
    <path d="M14 11v6" />
    <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
  </Svg>
);

const IconEnvelope = () => (
  <Svg>
    <rect x="2" y="4" width="20" height="16" rx="2" />
    <polyline points="22 6 12 13 2 6" />
  </Svg>
);

const IconEnvelopeOpen = () => (
  <Svg>
    <path d="M21 10v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V10l9-6 9 6z" />
    <polyline points="3 10 12 17 21 10" />
  </Svg>
);

const IconStar = ({ filled }: { filled: boolean }) => (
  <svg
    width="16"
    height="16"
    viewBox="0 0 24 24"
    fill={filled ? "currentColor" : "none"}
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
  </svg>
);


// ─── Header primitives ──────────────────────────────────────────────────────

function Avatar({ color, name }: { color?: string; name: string }) {
  const initials = (() => {
    const words = name.trim().split(/\s+/).filter((w) => w.length > 0);
    if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
    const local = (name.split("@")[0] ?? name).replace(/[^a-zA-Z0-9]/g, "");
    return (local.slice(0, 2) || "?").toUpperCase();
  })();
  return (
    <span
      className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold"
      style={{
        background: color ?? "var(--bg-hover)",
        color: color ? "#fff" : "var(--fg-muted)",
      }}
      aria-hidden
    >
      {initials}
    </span>
  );
}

function RecipientLine({
  label,
  list,
  fgMuted,
  fgSubtle,
}: {
  label: string;
  list: Address[];
  fgMuted: string;
  fgSubtle: string;
}) {
  if (list.length === 0) return null;
  const text = list.map(addressText).join(", ");
  return (
    <div className="flex items-baseline gap-2 text-xs">
      <span className="shrink-0" style={{ color: fgSubtle }}>
        {label}
      </span>
      <span className="min-w-0 flex-1 truncate" style={{ color: fgMuted }}>
        {text}
      </span>
    </div>
  );
}

function Badge({
  children,
  color,
  tone,
  title,
}: {
  children: React.ReactNode;
  color?: string;
  tone?: "amber" | "muted";
  title?: string;
}) {
  if (color) {
    return (
      <span
        title={title}
        className="inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-[11px]"
        style={{
          borderColor: "var(--border-base)",
          color: "var(--fg-base)",
        }}
      >
        <span
          className="inline-block h-1.5 w-1.5 rounded-full"
          style={{ background: color }}
          aria-hidden
        />
        {children}
      </span>
    );
  }
  const toneColor =
    tone === "amber"
      ? "#f59e0b"
      : tone === "muted"
        ? "var(--fg-muted)"
        : "var(--fg-muted)";
  return (
    <span
      title={title}
      className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px]"
      style={{
        borderColor: "var(--border-base)",
        color: toneColor,
      }}
    >
      {children}
    </span>
  );
}

function addressText(a: Address): string {
  if (a.name && a.name.length > 0) return `${a.name} <${a.email}>`;
  return a.email;
}

function MetaRow({ label, value }: { label: string; value: string }) {
  return (
    <>
      <span style={{ color: "var(--fg-subtle)" }}>{label}</span>
      <span
        className="break-all font-mono text-[11px]"
        style={{ color: "var(--fg-base)" }}
      >
        {value}
      </span>
    </>
  );
}

function BodyView({
  messageId,
  senderEmail,
  plain,
  html,
  attachments,
  fallback,
}: {
  messageId: string;
  /** Lowercased-on-write email of the From: header, or null if missing.
   *  Used to look up the trusted-senders allowlist and pre-flip the
   *  remote-image gate for senders the user has marked as trusted. */
  senderEmail: string | null;
  plain: string | null;
  html: string | null;
  attachments: AttachmentMeta[];
  fallback: string;
}) {
  const { t } = useTranslation();
  // Remote images are blocked by the CSP in the iframe doc. The gate
  // has two persistent opt-ins (address allowlist, domain allowlist)
  // plus a per-message override:
  //   * `remoteImages` — user clicked "Bilder laden" without ticking
  //     either remember-checkbox. True for this mail only, resets on
  //     selection change.
  //   * `trustReason` — non-null when the sender is on the persistent
  //     allowlist. Distinguishes "address match" from "domain match"
  //     so the banner can show the right text. Re-read on every
  //     `cm:trusted-senders-changed` event so a remove from another
  //     Reader / the settings panel propagates instantly.
  //
  // Two banner-checkboxes (`rememberAddress`, `rememberDomain`) drive
  // *what* gets persisted on the next "Bilder laden" click. They're
  // mutually exclusive in spirit (trusting the domain implies the
  // address), but we don't enforce — picking domain just covers more.
  const [remoteImages, setRemoteImages] = useState(false);
  const [trustReason, setTrustReason] = useState<TrustReason | null>(() =>
    trustReasonFor(senderEmail),
  );
  const [rememberAddress, setRememberAddress] = useState(false);
  const [rememberDomain, setRememberDomain] = useState(false);

  useEffect(() => {
    setRemoteImages(false);
    setRememberAddress(false);
    setRememberDomain(false);
    setTrustReason(trustReasonFor(senderEmail));
  }, [messageId, senderEmail]);

  // React to allowlist mutations from anywhere — settings panel,
  // another Reader instance, or this same banner's "remove" action.
  useEffect(() => {
    const onChange = () => {
      setTrustReason(trustReasonFor(senderEmail));
    };
    window.addEventListener(TRUSTED_SENDERS_CHANGED, onChange);
    return () => {
      window.removeEventListener(TRUSTED_SENDERS_CHANGED, onChange);
    };
  }, [senderEmail]);

  const senderDomain = extractDomain(senderEmail);
  // The effective "show remote images" decision. Either source of trust
  // (per-message click or persisted allowlist) flips the bit.
  const allowRemote = remoteImages || trustReason !== null;

  // Resolve every `cid:<content-id>` reference to a data URL by calling the
  // backend once per inline part. Cached per (messageId, cid) so re-renders
  // don't re-fetch.
  const [cidMap, setCidMap] = useState<Record<string, string>>({});
  useEffect(() => {
    let cancelled = false;
    setCidMap({});
    const inlineParts = attachments.filter(
      (a) => a.contentId && (a.isInline || a.mimeType.startsWith("image/")),
    );
    if (inlineParts.length === 0 || !html) return;
    (async () => {
      const entries: [string, string][] = [];
      for (const att of inlineParts) {
        try {
          const dataUrl = await invoke<string>(
            "get_inline_attachment_data_url",
            { messageId, partIdx: att.partIdx },
          );
          if (att.contentId) entries.push([att.contentId, dataUrl]);
        } catch (e) {
          console.warn("inline fetch failed for", att.contentId, e);
        }
      }
      if (!cancelled) {
        setCidMap(Object.fromEntries(entries));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [messageId, attachments, html]);

  const hasRemoteImages = useMemo(
    () => (html ? /<img[^>]+src=["']https?:/i.test(html) : false),
    [html],
  );

  const srcDoc = useMemo(() => {
    if (html) {
      const resolved = resolveCidImages(html, cidMap);
      return buildSandboxDoc(resolved, false, allowRemote);
    }
    if (plain) return buildSandboxDoc(escapeHtml(plain), true, false);
    return null;
  }, [plain, html, cidMap, allowRemote]);

  if (!srcDoc) {
    return (
      <div
        className="px-6 py-8 text-sm"
        style={{ color: "var(--fg-subtle)" }}
      >
        {fallback}
      </div>
    );
  }

  return (
    <div className="flex h-full w-full flex-col" style={{ background: "var(--bg-panel)" }}>
      {hasRemoteImages && !allowRemote && (
        <div
          className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1.5 border-b px-6 py-1.5 text-[11px]"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-muted)",
          }}
        >
          <span>{t("reader.remoteImagesBlocked")}</span>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            {/* Two persistent-trust checkboxes. Only rendered when we
                actually know the sender's email + can extract a
                domain — a malformed From: with neither would offer
                no stable identifier to remember. Domain-checkbox
                wins implicitly: at click time we add the broader
                scope, the address one is redundant in that case. */}
            {senderEmail && (
              <label
                className="flex select-none items-center gap-1.5"
                title={t("reader.trustSenderHint", { email: senderEmail })}
              >
                <input
                  type="checkbox"
                  checked={rememberAddress}
                  onChange={(e) => {
                    setRememberAddress(e.target.checked);
                    if (e.target.checked) setRememberDomain(false);
                  }}
                  className="h-3 w-3 cursor-pointer"
                />
                <span className="cursor-pointer">
                  {t("reader.trustSender")}
                </span>
              </label>
            )}
            {senderDomain && (
              <label
                className="flex select-none items-center gap-1.5"
                title={t("reader.trustDomainHint", { domain: senderDomain })}
              >
                <input
                  type="checkbox"
                  checked={rememberDomain}
                  onChange={(e) => {
                    setRememberDomain(e.target.checked);
                    if (e.target.checked) setRememberAddress(false);
                  }}
                  className="h-3 w-3 cursor-pointer"
                />
                <span className="cursor-pointer">
                  {t("reader.trustDomain", { domain: senderDomain })}
                </span>
              </label>
            )}
            <button
              type="button"
              onClick={() => {
                setRemoteImages(true);
                // Persist whichever scope the user picked. Domain
                // wins — covering address as a side effect — when
                // both happen to be ticked despite the mutual-flip
                // above (defense in depth).
                if (rememberDomain && senderDomain) {
                  addTrustedDomain(senderDomain);
                  setTrustReason({ kind: "domain", domain: senderDomain });
                } else if (rememberAddress && senderEmail) {
                  addTrustedSender(senderEmail);
                  setTrustReason({
                    kind: "address",
                    address: senderEmail.toLowerCase().trim(),
                  });
                }
              }}
              className="rounded-md border px-2 py-0.5 text-[11px]"
              style={{
                borderColor: "var(--border-base)",
                color: "var(--fg-base)",
                background: "transparent",
              }}
            >
              {t("reader.loadRemoteImages")}
            </button>
          </div>
        </div>
      )}
      {/* Trusted-sender banner. Only shown when the mail actually
          contains remote images — a no-images mail from a trusted
          sender doesn't need the Reader to claim "loaded automatically"
          for content that isn't there. Trust can still be revoked
          from the settings panel in that case. */}
      {trustReason && hasRemoteImages && (
        <div
          className="flex items-center justify-between gap-2 border-b px-6 py-1.5 text-[11px]"
          style={{
            borderColor: "var(--border-soft)",
            background: "var(--bg-base)",
            color: "var(--fg-muted)",
          }}
        >
          <span>
            {trustReason.kind === "domain"
              ? t("reader.domainTrustedBanner", {
                  domain: trustReason.domain,
                })
              : t("reader.senderTrustedBanner", {
                  email: trustReason.address,
                })}
          </span>
          <button
            type="button"
            onClick={() => {
              if (trustReason.kind === "domain") {
                removeTrustedDomain(trustReason.domain);
              } else {
                removeTrustedSender(trustReason.address);
              }
              // Local mirror — the change event will also fire but
              // running this synchronously avoids a flicker.
              setTrustReason(null);
              setRemoteImages(false);
            }}
            className="rounded-md border px-2 py-0.5 text-[11px]"
            style={{
              borderColor: "var(--border-base)",
              color: "var(--fg-base)",
              background: "transparent",
            }}
          >
            {t("reader.untrustSender")}
          </button>
        </div>
      )}
      <iframe
        title="message"
        srcDoc={srcDoc}
        // `allow-scripts` is the minimum needed for our link-interceptor
        // to run inside the sandbox. We deliberately do NOT grant
        // `allow-same-origin` — the iframe stays null-origin, so even
        // if a malicious mail sneaks through sanitize it can't touch
        // parent cookies or storage. Scripts can still postMessage up.
        sandbox="allow-scripts"
        className="h-full w-full flex-1 border-0"
        style={{ background: "transparent" }}
      />
    </div>
  );
}

/// Rewrite `cid:<id>` URLs anywhere in the HTML to the resolved data URLs.
/// Matches both quoted (src="cid:...") and unquoted forms. Missing cids are
/// left as-is so the sandbox CSP blocks them cleanly rather than leaking a
/// network request.
function resolveCidImages(
  html: string,
  cidMap: Record<string, string>,
): string {
  if (Object.keys(cidMap).length === 0) return html;
  return html.replace(
    /(["'])cid:([^"'\s>]+)\1/gi,
    (whole, quote: string, cid: string) => {
      const key = cid.trim();
      const data = cidMap[key] ?? cidMap[decodeURIComponent(key)];
      return data ? `${quote}${data}${quote}` : whole;
    },
  );
}

function buildSandboxDoc(
  innerHtml: string,
  preformatted: boolean,
  allowRemoteImages: boolean,
): string {
  // Mail-HTML kommt oft als komplettes Dokument (`<html><body style="…">…`).
  // Wenn wir das 1:1 in unseren Sandbox-Body kippen, klemmt's: Browser
  // folden zwar nested `<html>`/`<body>` weg, aber inline `style="color:
  // #1f2937"` vom Sender-Body wird bei vielen Engines auf einen
  // automatisch erzeugten Wrapper-Knoten umgeschrieben → near-black-on-
  // near-black im Dark-Mode. Vor dem Wrap also den Body extrahieren,
  // sodass nur der eigentliche Inhalt unter UNSEREM Body landet.
  const cleaned = preformatted ? innerHtml : extractHtmlBody(innerHtml);
  const body = preformatted
    ? `<pre style="white-space: pre-wrap; word-wrap: break-word; font-family: inherit; margin: 0;">${innerHtml}</pre>`
    : cleaned;
  // ──────────────────────────────────────────────────────────────────────
  // CSP layer 2 of 2.
  //
  // Layer 1 is the app-shell CSP in `src-tauri/tauri.conf.json`
  // (`default-src 'self'; …`). It governs the React UI and blocks
  // sender-controlled HTML from reaching the parent context in the
  // first place.
  //
  // Layer 2 — *this* meta-CSP — runs inside the sandbox iframe that
  // renders the email body. The iframe has `sandbox="allow-scripts"`
  // (no `allow-same-origin`) so it's null-origin and cannot reach the
  // parent's storage/cookies even if the sanitizer misses something.
  // The CSP below is the belt to that suspenders.
  //
  // Rules:
  // - `default-src 'none'`  — deny everything by default; opt in below.
  // - `img-src data:` (+`http: https:` after user clicks "Bilder laden")
  //   — inline CIDs resolved to data: URLs always work; remote images
  //   are user-gated to defeat tracking pixels.
  // - `style-src 'unsafe-inline'` — needed for the inline `<style>` block
  //   we ship with the iframe AND for sender's inline `style="…"` attrs.
  // - `script-src 'unsafe-inline'` — only the hard-coded link-interceptor
  //   below runs; the sanitizer strips sender `<script>` tags.
  // - `font-src data:` — data-URL embedded fonts are allowed; remote
  //   fonts are blocked (another tracking vector).
  // ──────────────────────────────────────────────────────────────────────
  const imgSrc = allowRemoteImages
    ? "data: https: http:"
    : "data:";
  const csp = `default-src 'none'; img-src ${imgSrc}; style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src data:;`;
  return `<!DOCTYPE html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="color-scheme" content="light dark" />
    <meta http-equiv="Content-Security-Policy" content="${csp}" />
    <style>
      :root { color-scheme: light dark; }
      html, body {
        margin: 0;
        padding: 1rem 1.25rem;
        font: 14px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
        color: #1f2937;
        background: #ffffff;
      }
      /* Dark-Mode-Body: pure white Text auf near-black. Vorher off-white
         (#e5e7eb), das auf groesseren Mail-Body-Flaechen gegen das
         near-black subjektiv matt wirkte. !important zieht inline
         color:#1f2937 (typisch von Outlook/Apple-Mail-erzeugten Body-
         Wrappern) auf lesbare Helligkeit hoch; Inline-Tags innerhalb
         von Spans/Divs mit tatsaechlich gewollter Faerbung bleiben
         unangetastet (CSS-Specificity: html/body !important schlaegt
         nur das top-level Body, nicht die der inneren Elemente). */
      @media (prefers-color-scheme: dark) {
        html, body {
          color: #ffffff !important;
          background: #1a1a1c;
        }
        blockquote { border-left-color: #4b5563 !important; color: #d1d5db !important; }
        a { color: #93c5fd !important; }
      }
      a { color: #2563eb; cursor: pointer; }
      img { max-width: 100%; height: auto; }
      table { max-width: 100%; }
      blockquote {
        border-left: 3px solid #d1d5db;
        margin: 0.5em 0; padding: 0.25em 0.75em;
        color: #6b7280;
      }
      pre, code {
        font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
        font-size: 13px;
      }
      pre { white-space: pre-wrap; word-wrap: break-word; }
    </style>
  </head>
  <body>${body}
<script>
  // Intercept clicks on any anchor with an http/https/mailto href and
  // forward the URL to the parent window via postMessage. The parent
  // hands it to Tauri's shell plugin which opens the OS-default browser
  // (not another Tauri window). Runs inside the "allow-scripts"-only
  // sandbox so it has no access to parent DOM, cookies, or storage.
  //
  // Defensive: damit's auch dann funktioniert, wenn das anchor-Element
  // ein target=_blank oder eine Inline-Onclick-Logik traegt (Marketing-
  // Mailer mit Tracking-Wrappern), preventDefault'en wir VOR jeder
  // weiteren Logik. preventDefault-stopPropagation-stopImmediate sicher
  // belegt, falls das Anchor-Element selbst einen Listener mit
  // capture=false traegt.
  console.log('[crystalmail-iframe] link-interceptor installed');
  document.addEventListener('click', function(e) {
    var el = e.target;
    while (el && el.nodeName !== 'A') el = el.parentNode;
    if (!el || !el.href) return;
    var href = el.getAttribute('href') || '';
    if (!/^(https?:|mailto:)/i.test(href)) {
      console.log('[crystalmail-iframe] click on non-http href — ignoring:', href);
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    if (typeof e.stopImmediatePropagation === 'function') {
      e.stopImmediatePropagation();
    }
    console.log('[crystalmail-iframe] forwarding click to parent:', el.href);
    try {
      window.parent.postMessage(
        { type: 'cm:open-url', href: el.href },
        '*'
      );
    } catch (err) {
      console.error('[crystalmail-iframe] postMessage failed:', err);
    }
  }, true);
  // Zusaetzliche Schutzschicht: einige Mail-Engines geben Anchors einen
  // submit-handler oder fuegen form-Wrapper drumrum. Wir blocken
  // beide Defaults global, damit der iframe niemals nach extern
  // navigiert. Visuelles Symptom des Fehlers war exakt das: WebView2
  // versuchte die externe URL im iframe zu laden und die Block-
  // Seite (X-Frame-Options / SmartScreen / corp policy) erschien.
  window.addEventListener('beforeunload', function(e) {
    console.warn('[crystalmail-iframe] beforeunload — blocking iframe navigation');
    e.preventDefault();
    e.returnValue = '';
  });
</script>
</body>
</html>`;
}

// ─── Reply / Forward draft builders ──────────────────────────────────────────

export function buildReplyDraft(
  detail: MessageDetail,
  account: AccountSummary | undefined,
): ComposeDraft {
  const env = detail.envelope;
  const subject = /^re:/i.test(env.subject.trim())
    ? env.subject
    : `Re: ${env.subject}`;
  const toAddr = env.from[0]?.email ?? "";
  const quotedPlain = quoteBodyPlain(detail, env.from[0]);
  const quotedHtml = quoteBodyHtml(detail, env.from[0]);
  return {
    accountId: account?.id,
    to: toAddr,
    cc: "",
    bcc: "",
    subject,
    // Body starts empty — the quote block is shown separately (read-only) in
    // Compose so the user types their reply above it without having to
    // scroll past the quote.
    body: "",
    quotedPlain,
    quotedHtml,
    inReplyToHeader: env.messageIdHeader ?? undefined,
    parentMessageId: env.id,
    parentMode: "answered",
    references: env.messageIdHeader ? [env.messageIdHeader] : [],
  };
}

/// Reply-All: `To` is the original sender + the other To-recipients; `Cc` is
/// the original Cc list. Our own addresses (account primary + aliases) are
/// filtered out so we don't reply to ourselves.
function buildReplyAllDraft(
  detail: MessageDetail,
  account: AccountSummary | undefined,
): ComposeDraft {
  const env = detail.envelope;
  const subject = /^re:/i.test(env.subject.trim())
    ? env.subject
    : `Re: ${env.subject}`;

  const ownEmails = new Set<string>();
  if (account) {
    ownEmails.add(account.address.toLowerCase());
    account.aliases.forEach((al) => ownEmails.add(al.email.toLowerCase()));
  }
  const notOwn = (a: Address) =>
    !ownEmails.has((a.email || "").toLowerCase());

  const to = [...env.from, ...env.to]
    .filter(notOwn)
    .map((a) => (a.name ? `${a.name} <${a.email}>` : a.email))
    .filter(Boolean);
  const cc = env.cc
    .filter(notOwn)
    .map((a) => (a.name ? `${a.name} <${a.email}>` : a.email));

  const quotedPlain = quoteBodyPlain(detail, env.from[0]);
  const quotedHtml = quoteBodyHtml(detail, env.from[0]);

  return {
    accountId: account?.id,
    to: unique(to).join(", "),
    cc: unique(cc).join(", "),
    bcc: "",
    subject,
    body: "",
    quotedPlain,
    quotedHtml,
    inReplyToHeader: env.messageIdHeader ?? undefined,
    parentMessageId: env.id,
    parentMode: "answered",
    references: env.messageIdHeader ? [env.messageIdHeader] : [],
  };
}

function unique(list: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const s of list) {
    const k = s.toLowerCase();
    if (!seen.has(k)) {
      seen.add(k);
      out.push(s);
    }
  }
  return out;
}

export function buildForwardDraft(
  detail: MessageDetail,
  account: AccountSummary | undefined,
): ComposeDraft {
  const env = detail.envelope;
  const subject = /^fwd?:/i.test(env.subject.trim())
    ? env.subject
    : `Fwd: ${env.subject}`;
  const headerPlain = [
    "---------- Weitergeleitete Nachricht ----------",
    `Von: ${env.from.map(addressText).join(", ")}`,
    `Datum: ${new Date(env.date).toLocaleString()}`,
    `Betreff: ${env.subject}`,
    `An: ${env.to.map(addressText).join(", ")}`,
    "",
  ].join("\n");
  const plain = detail.plainText ?? stripHtmlToText(detail.htmlText ?? "");
  const quotedPlain = `${headerPlain}${plain}`;

  const headerHtml =
    `<div style="margin:1em 0;padding-top:0.5em;border-top:1px solid #d1d5db;font-size:12px;color:#6b7280;">` +
    `<div><strong>Weitergeleitete Nachricht</strong></div>` +
    `<div><strong>Von:</strong> ${escapeHtml(env.from.map(addressText).join(", "))}</div>` +
    `<div><strong>Datum:</strong> ${escapeHtml(new Date(env.date).toLocaleString())}</div>` +
    `<div><strong>Betreff:</strong> ${escapeHtml(env.subject)}</div>` +
    `<div><strong>An:</strong> ${escapeHtml(env.to.map(addressText).join(", "))}</div>` +
    `</div>`;
  const bodyHtml = detail.htmlText
    ? sanitizeFragment(extractHtmlBody(detail.htmlText))
    : plainToHtml(plain);
  const quotedHtml = headerHtml + `<div>${bodyHtml}</div>`;

  return {
    accountId: account?.id,
    to: "",
    cc: "",
    bcc: "",
    subject,
    body: "",
    quotedPlain,
    quotedHtml,
    parentMessageId: env.id,
    parentMode: "forwarded",
  };
}

function quoteBodyPlain(detail: MessageDetail, from: Address | undefined): string {
  const plain = detail.plainText ?? stripHtmlToText(detail.htmlText ?? "");
  const date = new Date(detail.envelope.date).toLocaleString();
  const who = from ? addressText(from) : "Der Absender";
  const quoted = plain
    .split(/\r?\n/)
    .map((l) => `> ${l}`)
    .join("\n");
  return `Am ${date} schrieb ${who}:\n${quoted}`;
}

function quoteBodyHtml(detail: MessageDetail, from: Address | undefined): string {
  const date = new Date(detail.envelope.date).toLocaleString();
  const who = from ? addressText(from) : "Der Absender";
  const header = `<div style="font-size:12px;color:#6b7280;margin-top:1em;">Am ${escapeHtml(date)} schrieb ${escapeHtml(who)}:</div>`;
  // Pull the body-level content out of whatever the sender gave us — most
  // clients ship a full `<html><head>…</head><body>…</body></html>` document.
  // Nesting that inside our `<blockquote>` would produce invalid HTML and
  // many recipient MUAs degrade to text/plain on invalid HTML, which was
  // exactly the bug we saw.
  const innerRaw = detail.htmlText
    ? sanitizeFragment(extractHtmlBody(detail.htmlText))
    : plainToHtml(detail.plainText ?? "");
  return (
    `${header}` +
    `<blockquote style="margin:0.5em 0 0.5em 0;padding:0 0 0 0.75em;border-left:3px solid #d1d5db;color:inherit;">` +
    innerRaw +
    `</blockquote>`
  );
}

/**
 * Build a ComposeDraft for *editing* an existing draft. Differs from the
 * reply/forward builders in three places:
 *
 *   1. Recipients come straight from the draft envelope (To/Cc; Bcc isn't
 *      reliably retrievable from server-side drafts because most MUAs
 *      strip it out before APPEND — accept the loss).
 *   2. Subject is preserved verbatim (no `Re:` / `Fwd:` prefix).
 *   3. Body is the user's *own* prose: we drop in `bodyHtml` so the
 *      Compose editor seeds with the exact HTML the user last saved,
 *      no signature re-injection (the saved draft already contains
 *      whatever signature was there at save-time).
 *
 * `replacesDraftMessageId` is set so App's send/save-draft handlers can
 * delete the old draft after a successful new save.
 *
 * Limitation: server-side attachments are NOT round-tripped on edit.
 * They live as MIME parts and we'd need to extract them to a temp dir
 * to feed lettre — for now we drop them and the user re-attaches if
 * needed. Status is surfaced in the UI when a dropped-attachment draft
 * opens.
 */
export function buildEditDraft(
  detail: MessageDetail,
  account: AccountSummary | undefined,
): ComposeDraft {
  const env = detail.envelope;
  const fmt = (a: Address) =>
    a.name && a.name.trim().length > 0 ? `${a.name} <${a.email}>` : a.email;
  const toStr = env.to.map(fmt).filter(Boolean).join(", ");
  const ccStr = env.cc.map(fmt).filter(Boolean).join(", ");

  // Body: prefer HTML so formatting is preserved through the round-trip.
  // Fall back to plain wrapped in <div> so the editor still has *something*.
  const bodyHtml = detail.htmlText
    ? sanitizeFragment(extractHtmlBody(detail.htmlText))
    : detail.plainText
      ? `<div>${escapeHtml(detail.plainText).replace(/\r?\n/g, "<br>")}</div>`
      : "<div><br></div>";

  // Identity-Match: wenn der draft eine bestimmte From-Adresse hatte
  // (z. B. ein Alias des Account), versuchen wir die wieder zu treffen.
  // Beim Account-Lookup oben haben wir env.account_id genutzt; Alias-
  // Detection passiert in Compose anhand des `identityKey`.
  const fromEmail = env.from[0]?.email?.toLowerCase();
  const identityKey =
    account && fromEmail ? `${account.id}::${fromEmail}` : undefined;

  return {
    accountId: account?.id,
    identityKey,
    to: toStr,
    cc: ccStr,
    bcc: "",
    subject: env.subject,
    // body is the plain-text fallback for SMTP. Leave empty — the editor
    // has its own bodyHtml; Compose's snapshot rebuilds plaintext via
    // editorRef.getText() at send time.
    body: detail.plainText ?? "",
    bodyHtml,
    replacesDraftMessageId: env.id,
  };
}

// re-export EnvelopeDetail so callers that only import Reader get the type too
export type { EnvelopeDetail };
