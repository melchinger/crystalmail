import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import { Sidebar, type FolderSelection } from "./components/Sidebar";
import { LearnSpamRuleDialog } from "./components/LearnSpamRuleDialog";
import { decodeImapFolderName } from "./utils/imapFolderName";
import { sortAccounts } from "./utils/accountOrder";
import { InboxList } from "./components/InboxList";
import { AccountFilterBar } from "./components/AccountFilterBar";
import {
  Reader,
  buildReplyDraft,
  buildForwardDraft,
  buildEditDraft,
} from "./components/Reader";
import { AddAccountDialog } from "./components/AddAccountDialog";
import {
  Compose,
  splitAddresses,
  type ComposeSendSnapshot,
} from "./components/Compose";
import { CalendarView } from "./components/CalendarView";
import { ContactsView } from "./components/ContactsView";
import { ContactDetail } from "./components/ContactDetail";
import { UndoSendOverlay } from "./components/UndoSendOverlay";
import { HotkeyHelp } from "./components/HotkeyHelp";
import { CommandPalette } from "./components/CommandPalette";
import { useAiEnabled } from "./utils/aiState";
import { SettingsDialog } from "./components/SettingsDialog";
import { PiTerminal } from "./components/PiTerminal";
import { WorkflowRuleToastStack } from "./components/WorkflowRuleToast";
import { useFontZoom } from "./hooks/useFontZoom";
import { useHotkeys } from "./hooks/useHotkeys";
import { loadHotkeys, saveHotkeys, type HotkeyBindings } from "./settings/hotkeys";
import {
  loadNotificationSettings,
  type NotificationSettings,
} from "./settings/notifications";
import { playNotifySound } from "./utils/notifySound";
import { renderBadgeRgba } from "./utils/badgeIcon";
import { parseSearchQuery } from "./utils/searchDsl";
import {
  loadRecentSearches,
  pushRecentSearch,
  removeRecentSearch,
} from "./utils/recentSearches";
import type {
  AccountSummary,
  ComposeDraft,
  EnvelopeSummary,
  MessageDetail,
  PreparedImportDraft,
  SyncProgress,
  SyncReport,
  UnifiedUnreadCount,
} from "./types";

/** How the user wants to use pi's response text in a new draft. */
export type ComposeFromPiIntent = "new" | "reply" | "forward";

type FolderKey =
  | "unified"
  | "starred"
  | "contacts"
  | "calendar"
  | "archive"
  | "drafts"
  | "sent"
  | "trash"
  | "spam";

// `undefined` = closed; `null` = add-new dialog; AccountSummary = edit dialog.
type DialogState = undefined | null | AccountSummary;

const BLANK_DRAFT: ComposeDraft = {
  to: "",
  cc: "",
  bcc: "",
  subject: "",
  body: "",
};

/** Übersetzt einen vom Backend gelieferten Import-Draft (CLI-Trigger
 *  aus Python o.ä.) in das `ComposeDraft`-Schema, das der Composer
 *  versteht. Sucht den `accountEmail`-Treffer in der Account-Liste
 *  (matched gegen primäre Adresse + Aliase, case-insensitive); fehlt
 *  ein Match oder ist das Feld leer, bleibt `accountId`/`identityKey`
 *  undefined und der Composer wählt den Default. */
function importDraftToComposeDraft(
  imp: PreparedImportDraft,
  accounts: AccountSummary[],
): ComposeDraft {
  let accountId: string | undefined;
  let identityKey: string | undefined;
  if (imp.accountEmail) {
    const wanted = imp.accountEmail.toLowerCase();
    for (const a of accounts) {
      if (a.address.toLowerCase() === wanted) {
        accountId = a.id;
        identityKey = `${a.id}::${a.address.toLowerCase()}`;
        break;
      }
      const alias = a.aliases.find((x) => x.email.toLowerCase() === wanted);
      if (alias) {
        accountId = a.id;
        identityKey = `${a.id}::${alias.email.toLowerCase()}`;
        break;
      }
    }
  }
  return {
    accountId,
    identityKey,
    to: imp.to,
    cc: imp.cc,
    bcc: imp.bcc,
    subject: imp.subject,
    body: imp.body,
    attachments: imp.attachments,
  };
}

/**
 * Auto-sync cooldown: when the user switches folders and the last sync was
 * longer ago than this, kick off a fresh sync in the background. Short
 * enough that stale views are rare, long enough that rapid folder cycling
 * (e.g. Inbox → Drafts → Inbox) doesn't hammer the server.
 */
const AUTO_SYNC_COOLDOWN_MS = 90_000;

/**
 * Sliding-window page sizes for the envelope list.
 *
 *  - `PAGE_SIZE_INITIAL`: cap on the first fetch in any view. Picked to
 *    fill a typical screen comfortably without wasting bandwidth on
 *    rarely-used sub-folders.
 *  - `PAGE_SIZE_STEP`: how much the window grows on each
 *    scroll-to-bottom. Same value as the initial cap so the user can
 *    "double the list" with one scroll gesture.
 *  - `PAGE_SIZE_SEARCH`: hard cap when the FTS path is active. Larger
 *    than the browse cap because FTS results aren't a chronological
 *    page — they're a top-N ranking. The user explicitly asked for
 *    search to span all DB headers, so we lean generous here.
 */
const PAGE_SIZE_INITIAL = 100;
const PAGE_SIZE_STEP = 100;
const PAGE_SIZE_SEARCH = 500;
const SYNC_OLDER_BATCH = 50;

export function App() {
  const { t } = useTranslation();
  const [activeFolder, setActiveFolder] = useState<FolderKey>("unified");
  // null = unified over all accounts; an accountId = only that account's envelopes.
  const [accountFilter, setAccountFilter] = useState<string | null>(null);
  // Per-account sub-folder pin. When set, the envelope list is scoped to
  // this exact IMAP folder on the server and overrides activeFolder /
  // accountFilter for list + search queries.
  const [selectedFolder, setSelectedFolder] = useState<FolderSelection | null>(
    null,
  );
  const [status, setStatus] = useState<string>("…");
  const [accounts, setAccounts] = useState<AccountSummary[]>([]);
  const [inbox, setInbox] = useState<EnvelopeSummary[]>([]);
  // Sliding window for the envelope list. Grows in PAGE_STEP chunks
  // each time the user scrolls past the bottom; reset on view-context
  // change (folder switch, account filter, search). When the window
  // overshoots what's locally cached, the scroll handler also asks the
  // backend for older envelopes via `sync_folder_older` /
  // `sync_unified_folder_older`. Module-scope constants are below the
  // component definition.
  const [pageSize, setPageSize] = useState(PAGE_SIZE_INITIAL);
  const [selectedId, setSelectedId] = useState<string | undefined>();
  const [dialog, setDialog] = useState<DialogState>(undefined);
  const [syncing, setSyncing] = useState(false);
  const [composeDraft, setComposeDraft] = useState<ComposeDraft | null>(null);
  /** Fehler-Banner für externen Draft-Import (CLI-Trigger). Wird vom
   *  Backend-Event `compose-from-template-error` gesetzt, wenn ein
   *  `--draft-from-template`/`--draft-job` Aufruf scheitert (Template
   *  nicht da, Anhang fehlt, Frontmatter kaputt, …). Auto-Dismiss
   *  nach ~12 s, manuelles Schließen jederzeit. */
  const [importErrorBanner, setImportErrorBanner] = useState<{
    message: string;
    sourceTemplate: string;
  } | null>(null);
  /** Pending undo-send slot. While set, the bottom overlay is visible
   *  and counts down to the actual SMTP submit. Cancel re-opens
   *  Compose with the snapshot intact; expiry kicks off the real
   *  invoke and clears this. Only ever one in flight at a time —
   *  starting another send before the timer expires is harmless
   *  because `canSend` in Compose is gated by accountId+to anyway. */
  const [pendingSend, setPendingSend] = useState<ComposeSendSnapshot | null>(
    null,
  );
  /** Last accountId the user actually viewed a mail from. Cached across
   *  folder switches so a subsequent "new mail" can fall back to it
   *  when neither the account-filter nor a pinned subfolder pin one
   *  down. Updated whenever an envelope opens. */
  const lastViewedAccountIdRef = useRef<string | null>(null);

  // ── Contacts-Mode-State ────────────────────────────────────────
  // `selectedContactId` = aktuell offen im Detail-Inspector. `null`
  // wenn der User auf "neuer Kontakt" geklickt hat (Detail rendert
  // dann im New-Mode). `undefined` = nichts ausgewählt (leeres
  // Detail-Panel mit Hinweis).
  const [selectedContactId, setSelectedContactId] = useState<
    string | null | undefined
  >(undefined);
  // Bumpt bei create/update/delete damit ContactsView neu lädt ohne
  // dass wir interne refresh-Pfade in der View brauchen.
  const [contactsRefreshKey, setContactsRefreshKey] = useState(0);
  const [hotkeyHelp, setHotkeyHelp] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // Optional deep-link target when opening Settings — e.g. the
  // AiRequiredNotice in dialogs sets this to "pi" before flipping
  // settingsOpen on, so the user lands directly on the right pane.
  // Reset to undefined whenever Settings closes so the next plain
  // open goes back to the Accounts default.
  const [settingsInitialCategory, setSettingsInitialCategory] = useState<
    | "accounts"
    | "folders"
    | "spam"
    | "workflows"
    | "notifications"
    | "hotkeys"
    | "pi"
    | "tags"
    | "trusted"
    | "backup"
    | undefined
  >(undefined);
  // Command palette ("/"): a searchable list of every hotkey action.
  // Picking one runs through the same dispatch as the matching key —
  // see `dispatchHotkeyAction`.
  const [commandPaletteOpen, setCommandPaletteOpen] = useState(false);
  // Master AI kill-switch — read here to render the footer indicator.
  // Source of truth is the Rust state (`PiConfig.enabled`); the hook
  // syncs via the `cm:ai-enabled-changed` window event whenever any
  // entry point (settings switch, footer toggle, future palette
  // command) flips it.
  const [aiEnabled, setAiEnabled] = useAiEnabled();
  const [piExpanded, setPiExpanded] = useState(false);
  const [hotkeyBindings, setHotkeyBindings] = useState<HotkeyBindings>(() =>
    loadHotkeys(),
  );
  // Number of optimistic message mutations (archive/delete/move) still
  // running against IMAP. Shown as a discreet indicator in the footer so
  // the user knows something's happening, but the view is already free.
  const [pendingMutations, setPendingMutations] = useState(0);
  const [learnSpamOpen, setLearnSpamOpen] = useState(false);
  // Set of message IDs currently marked as workflow-training
  // candidates. Fed into `InboxList` so the matching rows get a TRAIN
  // badge. Refreshed on the `cm:training:changed` custom event that
  // Reader emits after a `t`-hotkey toggle, plus on every inbox
  // refresh to stay in lockstep when rules clear candidates.
  const [trainingIds, setTrainingIds] = useState<Set<string>>(() => new Set());

  const refreshTrainingIds = useCallback(async () => {
    try {
      const ids = await invoke<string[]>("list_workflow_training_ids");
      setTrainingIds(new Set(ids));
    } catch {
      // Non-fatal — the badge just stays stale for one cycle.
    }
  }, []);

  /**
   * Pick the most-likely-correct From-account for a brand-new mail, in
   * priority order:
   *   1. **Account-Filter** — explicit user intent: they're already
   *      narrowed to one account in the sidebar, send from that.
   *   2. **Pinned subfolder** — the user is browsing a specific server
   *      folder of one account, that's the implicit context.
   *   3. **Last-viewed mail's account** — soft hint from recent activity.
   *   4. **Falls back** to whatever Compose itself picks (typically
   *      `accounts[0]`) when none of the above is set.
   */
  const defaultComposeAccountId = useCallback((): string | undefined => {
    if (accountFilter) return accountFilter;
    if (selectedFolder?.accountId) return selectedFolder.accountId;
    if (lastViewedAccountIdRef.current) return lastViewedAccountIdRef.current;
    return undefined;
  }, [accountFilter, selectedFolder]);

  /** Open Compose with a fresh blank draft, account pre-selected from
   *  the current context. Wrapper around `setComposeDraft` that the
   *  hotkey + sidebar + toolbar paths all share. */
  const openBlankCompose = useCallback(() => {
    setComposeDraft({
      ...BLANK_DRAFT,
      accountId: defaultComposeAccountId(),
    });
  }, [defaultComposeAccountId]);

  /**
   * Doppelklick auf eine Envelope-Zeile. Wenn die Mail in dem
   * konfigurierten Drafts-Ordner des zugehörigen Accounts liegt,
   * öffnet sie sich im Compose zur Bearbeitung. Nach erfolgreichem
   * Speichern / Senden räumt der Send-Pfad den Original-Draft auf
   * (`replacesDraftMessageId`).
   *
   * Für Mails außerhalb des Drafts-Ordners ist Doppelklick aktuell
   * ein No-Op (Single-Click hat ja schon im Reader geöffnet).
   */
  const onEnvelopeActivate = useCallback(
    async (id: string) => {
      const env = inbox.find((e) => e.id === id);
      if (!env) return;
      const account = accounts.find((a) => a.id === env.accountId);
      if (!account) return;
      try {
        const detail = await invoke<MessageDetail>("open_message", {
          messageId: id,
        });
        // "Ist das ein Draft?" — Folder-Name muss zum konfigurierten
        // Drafts-Folder passen. (IMAP `\Draft`-Flag wäre theoretisch
        // präziser, aber manche Server setzen das nicht zuverlässig
        // bei APPEND, der Folder-Match ist robuster.)
        const isInDrafts =
          detail.envelope.folderName === account.draftsFolder;
        if (!isInDrafts) {
          // Nicht-Draft: Doppelklick ist hier kein Edit-Trigger.
          // Single-Click hat den Reader schon geöffnet, also nichts
          // zu tun.
          return;
        }
        const editDraft = buildEditDraft(detail, account);
        setComposeDraft(editDraft);
        if (detail.attachments.length > 0) {
          // Honest disclosure: Server-Side-Anhänge round-trippen wir
          // nicht (bräuchte Extract auf Disk). User merkt es weil
          // Anhänge fehlen — der Status-Toast erklärt warum.
          setStatus(
            t("compose.draftAttachmentsLost", {
              count: detail.attachments.length,
            }),
          );
        }
      } catch (e) {
        console.error("draft activate failed:", e);
        setStatus(t("compose.draftOpenFailed", { detail: String(e) }));
      }
    },
    [inbox, accounts, t],
  );

  useEffect(() => {
    void refreshTrainingIds();
    const onChanged = () => void refreshTrainingIds();
    window.addEventListener("cm:training:changed", onChanged);
    return () => {
      window.removeEventListener("cm:training:changed", onChanged);
    };
  }, [refreshTrainingIds]);
  // Unread counts per canonical unified folder — populated by
  // `unified_unread_counts` after startup, every sync, every refresh,
  // and every mark-read mutation. Feeds both the sidebar badges and
  // the window title.
  const [unreadCounts, setUnreadCounts] = useState<
    Record<string, number>
  >({});
  // Live progress from the Rust sync task — fed via `sync-progress`
  // Tauri events. `null` between runs, a fresh payload while syncing.
  const [syncProgress, setSyncProgress] = useState<SyncProgress | null>(null);

  // Search state. `searchInput` is what the user is typing; `searchQuery`
  // is the debounced value we actually hit the backend with. Empty query
  // → normal folder listing.
  const [searchInput, setSearchInput] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const [searchError, setSearchError] = useState<string | null>(null);
  // Recent searches lifted from localStorage. Re-rendered as chips in
  // the empty-state panel — see SearchPanel + utils/recentSearches.ts.
  const [recentSearches, setRecentSearches] = useState<string[]>(() =>
    loadRecentSearches(),
  );
  // "Über alle Ordner suchen" toggle. Off ⇒ search scoped to the
  // currently active folder (the historical default). On ⇒ search
  // skips the folder filter entirely; account scope still rides on
  // the existing accountFilter from the sidebar. Sticky during the
  // session, resets when the active folder changes (along with the
  // input itself).
  const [searchAllFolders, setSearchAllFolders] = useState(false);

  useEffect(() => {
    const trimmed = searchInput.trim();
    // 200ms debounce — long enough that typing a word doesn't fire five
    // SQL queries; short enough that it feels instant.
    const t = window.setTimeout(() => setSearchQuery(trimmed), 200);
    return () => window.clearTimeout(t);
  }, [searchInput]);

  // Reset the field when the user switches folder — stale "from:bob"
  // in the Inbox shouldn't linger into "Gesendet". Same for the
  // all-folders toggle: a deliberate user choice tied to one search
  // session, not a global preference.
  useEffect(() => {
    setSearchInput("");
    setSearchQuery("");
    setSearchError(null);
    setSearchAllFolders(false);
  }, [activeFolder]);

  const syncInFlight = useRef(false);
  /// Timestamp (ms) of the last completed sync, used by the folder-switch
  /// auto-sync to decide whether data is fresh enough. `null` = never.
  const lastSyncAt = useRef<number | null>(null);

  // Install global Ctrl+/- / Ctrl+Wheel zoom. Persisted in localStorage.
  const zoom = useFontZoom();

  // ↑/↓ navigate the envelope list without touching the mouse. Paired
  // with the optimistic archive/delete hotkeys, this is the "mausfreier
  // Inbox-Durchflug": Down advances, `e` archives and auto-advances,
  // Down skips, `Delete` removes, etc.
  //
  // Refs let the listener read the current list without re-binding on
  // every state change — the listener itself stays mounted once.
  const inboxRef = useRef(inbox);
  const selectedIdRef = useRef(selectedId);
  useEffect(() => {
    inboxRef.current = inbox;
  }, [inbox]);
  useEffect(() => {
    selectedIdRef.current = selectedId;
  }, [selectedId]);

  // Wenn der User eine Mail anklickt, merken wir uns das Konto — das
  // ist später der schwächste Default-Absender für eine neue Mail
  // (siehe `defaultComposeAccountId`). Wir lesen das aus dem aktuellen
  // `inbox`-State, weil das die einzige Quelle für `accountId` pro
  // Envelope ist, ohne dass wir einen DB-Roundtrip starten müssen.
  useEffect(() => {
    if (!selectedId) return;
    const env = inbox.find((e) => e.id === selectedId);
    if (env) lastViewedAccountIdRef.current = env.accountId;
  }, [selectedId, inbox]);

  useEffect(() => {
    const isTypingTarget = (t: EventTarget | null) => {
      if (!(t instanceof HTMLElement)) return false;
      const tag = t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
      return t.isContentEditable;
    };

    const onKey = (e: KeyboardEvent) => {
      if (
        e.key !== "ArrowDown" &&
        e.key !== "ArrowUp" &&
        e.key !== "Home" &&
        e.key !== "End"
      ) {
        return;
      }
      // Modifier combos are reserved for other things (Ctrl+Home = scroll,
      // Shift+Arrow = text selection in inputs). Only bare navigation keys
      // drive the list cursor.
      if (e.ctrlKey || e.altKey || e.metaKey || e.shiftKey) return;
      if (isTypingTarget(e.target)) return;

      const list = inboxRef.current;
      if (list.length === 0) return;
      const currentId = selectedIdRef.current;
      const currentIdx = currentId
        ? list.findIndex((r) => r.id === currentId)
        : -1;

      let nextIdx: number;
      switch (e.key) {
        case "ArrowDown":
          nextIdx = currentIdx < 0 ? 0 : Math.min(list.length - 1, currentIdx + 1);
          break;
        case "ArrowUp":
          nextIdx =
            currentIdx < 0 ? list.length - 1 : Math.max(0, currentIdx - 1);
          break;
        case "Home":
          nextIdx = 0;
          break;
        case "End":
          nextIdx = list.length - 1;
          break;
        default:
          return;
      }

      const picked = list[nextIdx];
      if (picked && picked.id !== currentId) {
        e.preventDefault();
        setSelectedId(picked.id);
      }
    };

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Global hotkeys. Message-scoped actions are forwarded as CustomEvents
  // that the Reader subscribes to; app-wide ones go through callbacks.
  useHotkeys(hotkeyBindings, {
    onCompose: () => openBlankCompose(),
    onSyncAll: () => {
      void syncAllRef.current?.();
    },
    onMarkAllRead: () => {
      void markAllReadRef.current?.();
    },
    onShowHelp: () => setHotkeyHelp(true),
    onShowSettings: () => setSettingsOpen(true),
    onShowCommandPalette: () => setCommandPaletteOpen(true),
    onEscape: () => {
      // Priority: close topmost layer first.
      if (commandPaletteOpen) setCommandPaletteOpen(false);
      else if (settingsOpen) setSettingsOpen(false);
      else if (hotkeyHelp) setHotkeyHelp(false);
      else if (composeDraft) setComposeDraft(null);
      else if (dialog !== undefined) setDialog(undefined);
      else setSelectedId(undefined);
    },
  });

  // syncAll is defined below via useCallback and depends on `accounts`; a
  // ref lets the hotkey hook call the current version without forcing a
  // circular dep with the callback.
  const syncAllRef = useRef<() => Promise<void>>();
  // Same trick for mark-all-read — the callback depends on the current
  // `inbox` list and that changes often; keeping the hotkey dispatcher
  // stable via this ref avoids re-registering the global keydown.
  const markAllReadRef = useRef<() => Promise<void>>();

  /**
   * Folder key that the currently-visible envelope list draws from.
   * Used by the optimistic unread-count adjustments to know which
   * sidebar badge to decrement. Declared up here (not next to the
   * rest of the unread-count machinery below) because
   * `runOptimisticRemoval` needs it and that's the first thing that
   * gets wired up.
   */
  const currentUnreadKey: string | null = selectedFolder
    ? null
    : activeFolder === "unified"
      ? "inbox"
      : activeFolder;

  const bumpUnreadCount = useCallback(
    (key: string | null, delta: number) => {
      if (!key || delta === 0) return;
      setUnreadCounts((c) => {
        const before = c[key] ?? 0;
        const after = Math.max(0, before + delta);
        if (after === before) return c;
        return { ...c, [key]: after };
      });
    },
    [],
  );

  /**
   * Tombstones for envelopes that have been optimistically popped but
   * whose IMAP-side delete/archive/move hasn't been confirmed yet.
   * `refresh` and `loadMoreOlder` filter against this set so a re-sync
   * that fires between the optimistic-pop and the backend's expunge
   * doesn't resurrect the row as a zombie. Cleared on backend failure
   * (where `runOptimisticRemoval` also re-injects the row); on success
   * the entry stays for the rest of the session — once a row is gone,
   * it's gone, and the cost of a few stale ids in a Set is nil.
   */
  const optimisticallyRemovedRef = useRef<Set<string>>(new Set());

  /**
   * Optimistic removal engine. Used by archive / delete / move — all three
   * have the same UI contract: the envelope vanishes from the list
   * immediately and the selection advances, even while the IMAP op is
   * still in flight.
   *
   * On failure the envelope is re-inserted at its natural chronological
   * position in whatever the list looks like *now* (not a full snapshot
   * rollback — that would undo other successful mutations the user did
   * in the meantime). The user's current selection is preserved.
   */
  const runOptimisticRemoval = useCallback(
    (
      id: string,
      backend: () => Promise<void>,
      errorMessage: (detail: string) => string,
    ) => {
      // Capture the row being removed so we can re-inject it on failure.
      let removed: EnvelopeSummary | undefined;

      // Tombstone the id *before* the optimistic pop so any refresh
      // that races in (e.g. a sync-progress auto-refresh that landed
      // mid-click) filters this row out of its result set instead of
      // putting it back on screen.
      optimisticallyRemovedRef.current.add(id);

      setInbox((rows) => {
        const idx = rows.findIndex((r) => r.id === id);
        if (idx < 0) return rows;
        removed = rows[idx];
        const next = rows.filter((r) => r.id !== id);
        // If the mail being removed is currently selected, auto-advance
        // to the next envelope in list order. This mirrors what
        // `onMessageRemoved` used to do, but happens *before* the backend
        // even starts.
        setSelectedId((cur) => {
          if (cur !== id) return cur; // user selected something else — respect it
          if (next.length === 0) return undefined;
          return next[idx]?.id ?? next[idx - 1]?.id ?? next[0]?.id;
        });
        return next;
      });

      // If a body fetch is still running for this message, abort it —
       // no point burning IMAP bandwidth on a body we're about to
       // discard, and on servers that serialize per-account sessions
       // this unblocks our own operation. Fire-and-forget; a no-match
       // on the backend is cheap.
      void invoke("cancel_pending_fetch", { messageId: id });

      // Decrement the unread counter for the current view if the
      // envelope we just popped was still unread. Covers archive /
      // delete / move — all three remove the row from this list, so
      // the user expects the sidebar badge to follow.
      if (removed && !removed.seen) {
        bumpUnreadCount(currentUnreadKey, -1);
      }

      setPendingMutations((n) => n + 1);
      backend()
        .catch((e) => {
          console.error("mutation failed:", e);
          setStatus(errorMessage(String(e)));
          // Lift the tombstone first — otherwise the re-injected row
          // would be filtered straight back out by the next refresh.
          optimisticallyRemovedRef.current.delete(id);
          // Re-inject at the natural (date-desc) position. Don't touch
          // selection — the user has moved on.
          if (removed) {
            const toRestore = removed;
            setInbox((rows) => {
              if (rows.some((r) => r.id === toRestore.id)) return rows;
              const next = [...rows, toRestore];
              next.sort((a, b) => b.date.localeCompare(a.date));
              return next;
            });
            // Unwind the optimistic decrement — the mail is back on
            // screen in its unread state.
            if (!toRestore.seen) {
              bumpUnreadCount(currentUnreadKey, +1);
            }
          }
        })
        .finally(() => {
          setPendingMutations((n) => Math.max(0, n - 1));
        });
    },
    [t, bumpUnreadCount, currentUnreadKey],
  );

  const onArchiveRequest = useCallback(
    (id: string) => {
      runOptimisticRemoval(
        id,
        () => invoke("archive_message", { messageId: id }),
        (detail) => t("mutation.archiveFailed", { detail }),
      );
    },
    [runOptimisticRemoval, t],
  );

  const onDeleteRequest = useCallback(
    (id: string) => {
      runOptimisticRemoval(
        id,
        () => invoke("delete_message", { messageId: id }),
        (detail) => t("mutation.deleteFailed", { detail }),
      );
    },
    [runOptimisticRemoval, t],
  );

  const onMoveRequest = useCallback(
    (id: string, folder: string) => {
      runOptimisticRemoval(
        id,
        () => invoke("move_message_to", { messageId: id, folder }),
        (detail) => t("mutation.moveFailed", { folder, detail }),
      );
    },
    [runOptimisticRemoval, t],
  );

  /**
   * Is the user currently looking at a Spam-view? Drives the "!" hotkey
   * semantics: in Spam, "!" is a flag-toggle (bestätigen / zurücknehmen)
   * instead of the usual flag+move.
   *
   * Two shapes of Spam-view:
   *   - canonical unified Spam folder (`activeFolder === "spam"`)
   *   - sub-folder pin via the sidebar expander that happens to point at
   *     this account's spam_folder (compared after IMAP UTF-7 decoding,
   *     since `selectedFolder.name` is decoded but `account.spamFolder`
   *     is still in server form)
   */
  const inSpamView = (() => {
    if (activeFolder === "spam") return true;
    if (!selectedFolder) return false;
    const acc = accounts.find((a) => a.id === selectedFolder.accountId);
    if (!acc) return false;
    return decodeImapFolderName(acc.spamFolder) === selectedFolder.name;
  })();

  const onMarkSpamRequest = useCallback(
    (id: string) => {
      if (inSpamView) {
        // In-Spam behavior: "!" toggles the $Junk flag on the mail
        // that's right here. No move (it's already in Spam); no row
        // removal (the user is curating, not banishing).
        const current = inbox.find((r) => r.id === id);
        if (!current) return;
        const targetJunk = !current.junk;
        setInbox((rows) =>
          rows.map((r) => (r.id === id ? { ...r, junk: targetJunk } : r)),
        );
        setPendingMutations((n) => n + 1);
        invoke("set_message_flags", {
          messageId: id,
          changes: { junk: targetJunk },
        })
          .catch((e) => {
            console.error("spam flag toggle failed:", e);
            setStatus(
              t("mutation.spamCandidateFailed", { detail: String(e) }),
            );
            // Roll the badge back.
            setInbox((rows) =>
              rows.map((r) =>
                r.id === id ? { ...r, junk: current.junk } : r,
              ),
            );
          })
          .finally(() => {
            setPendingMutations((n) => Math.max(0, n - 1));
          });
        return;
      }

      // Outside Spam: original behavior — set flag + move to Spam
      // folder. Optimistic pop, selection advances.
      runOptimisticRemoval(
        id,
        () => invoke("mark_as_spam", { messageId: id }),
        (detail) => t("mutation.markSpamFailed", { detail }),
      );
    },
    [inSpamView, inbox, runOptimisticRemoval, t],
  );

  /**
   * "Spam-candidate" action — toggles the `$Junk` keyword without moving
   * the mail. Used to collect a "hmm, probably spam" corpus during a
   * normal read-through. Because it's a toggle, pressing `j` on an
   * already-flagged mail clears the flag ("doch kein Spam").
   *
   * Auto-advance on the "set" direction (matches the rest of the
   * mutation hotkeys — `j j j` flies through the list). On the "clear"
   * direction, stay put — the user explicitly corrected themselves on
   * *this* row.
   */
  const onSpamCandidateRequest = useCallback(
    (id: string) => {
      const current = inbox.find((r) => r.id === id);
      if (!current) return;
      const targetJunk = !current.junk;

      setInbox((rows) => {
        const idx = rows.findIndex((r) => r.id === id);
        if (idx < 0) return rows;
        const next = rows.map((r) =>
          r.id === id ? { ...r, junk: targetJunk } : r,
        );
        // Advance only when we're setting the flag (first-time candidate).
        if (targetJunk) {
          setSelectedId((cur) => {
            if (cur !== id) return cur;
            const after = next[idx + 1];
            return after?.id ?? cur;
          });
        }
        return next;
      });

      setPendingMutations((n) => n + 1);
      invoke("set_message_flags", {
        messageId: id,
        changes: { junk: targetJunk },
      })
        .catch((e) => {
          console.error("spam candidate toggle failed:", e);
          setStatus(t("mutation.spamCandidateFailed", { detail: String(e) }));
          // Rollback to whatever the state was before the optimistic flip.
          setInbox((rows) =>
            rows.map((r) => (r.id === id ? { ...r, junk: current.junk } : r)),
          );
        })
        .finally(() => {
          setPendingMutations((n) => Math.max(0, n - 1));
        });
    },
    [inbox, t],
  );

  /**
   * Pipe a pi response into a fresh Compose draft. Three flavours:
   *   - "new"     → blank draft, body = pi text. Works regardless of
   *                  whether a single mail was pinned as context.
   *   - "reply"   → builds a reply against the pinned mail, body
   *                  replaced with pi's suggestion (quoted block stays
   *                  intact below so the user can see what they're
   *                  answering to).
   *   - "forward" → builds a forward of the pinned mail, body above
   *                  the forwarded block is pi's suggested preamble.
   *
   * Reply/forward silently fall back to "new" if no context mail is
   * available, so the caller's button-gating logic doesn't have to
   * duplicate the check.
   */
  const onComposeFromPi = useCallback(
    async (
      intent: ComposeFromPiIntent,
      body: string,
      contextMessageId?: string,
    ) => {
      if (intent === "new" || !contextMessageId) {
        setComposeDraft({
          ...BLANK_DRAFT,
          accountId: defaultComposeAccountId(),
          body,
        });
        return;
      }
      try {
        const detail = await invoke<MessageDetail>("open_message", {
          messageId: contextMessageId,
        });
        const account = accounts.find(
          (a) => a.id === detail.envelope.accountId,
        );
        const base =
          intent === "reply"
            ? buildReplyDraft(detail, account)
            : buildForwardDraft(detail, account);
        // pi's text is the user's own prose — replaces the empty `body`
        // field the builders leave untouched. Quote/header/in-reply-to
        // metadata remain intact.
        setComposeDraft({ ...base, body });
      } catch (e) {
        console.error("compose-from-pi failed:", e);
        // Fallback to blank draft so the user at least keeps the text.
        setComposeDraft({
          ...BLANK_DRAFT,
          accountId: defaultComposeAccountId(),
          body,
        });
      }
    },
    [accounts, defaultComposeAccountId],
  );

  /**
   * Convert a Compose snapshot into the wire-format payload the
   * `send_mail` and `save_draft` Tauri commands accept. Centralised so
   * the undo-send timer and the save-draft path use byte-identical
   * encoding rules.
   */
  const buildSendRequest = useCallback(
    (snap: ComposeSendSnapshot) => ({
      accountId: snap.accountId,
      from: snap.from ?? undefined,
      to: splitAddresses(snap.to),
      cc: splitAddresses(snap.cc),
      bcc: splitAddresses(snap.bcc),
      subject: snap.subject,
      body: snap.body,
      bodyHtml: snap.bodyHtml,
      inReplyTo: snap.inReplyToHeader,
      references: snap.references ?? [],
      attachments: snap.attachments.map((a) => ({
        path: a.path,
        filename: a.filename,
        mimeType: a.mimeType,
      })),
    }),
    [],
  );

  /** Re-open Compose with the snapshot's contents. Used by undo-send and
   *  by the failure-path of an actual send (the mail comes back as a
   *  draft so the user can retry / fix). `bodyHtml` is the editor's
   *  exact HTML at the moment Send was clicked, so reformatting is a
   *  non-issue. */
  const reopenAsDraft = useCallback((snap: ComposeSendSnapshot) => {
    setComposeDraft({
      accountId: snap.accountId,
      identityKey: snap.identityKey,
      to: snap.to,
      cc: snap.cc,
      bcc: snap.bcc,
      subject: snap.subject,
      body: snap.body,
      bodyHtml: snap.bodyHtml,
      attachments: snap.attachments,
      inReplyToHeader: snap.inReplyToHeader,
      references: snap.references,
      parentMessageId: snap.parentMessageId,
      parentMode: snap.parentMode,
      replacesDraftMessageId: snap.replacesDraftMessageId,
    });
  }, []);

  /** Actually fire the SMTP submit. Called by the undo-send timer when
   *  the 5 s grace expires, or directly when the user explicitly says
   *  "send now" in the overlay. On failure: snapshot lands as a server-
   *  side draft (so the work isn't lost) and a toast surfaces. */
  const performSend = useCallback(
    async (snap: ComposeSendSnapshot) => {
      try {
        await invoke("send_mail", {
          request: {
            ...buildSendRequest(snap),
            markAnswered:
              snap.parentMode === "answered"
                ? snap.parentMessageId
                : undefined,
            markForwarded:
              snap.parentMode === "forwarded"
                ? snap.parentMessageId
                : undefined,
          },
        });
        setStatus(t("compose.sent"));
        // Wenn das ein Edit eines bestehenden Drafts war, alten
        // Draft entsorgen — best-effort. delete_message verschiebt
        // in den Papierkorb statt hart zu löschen, das ist ok als
        // Sicherheitsnetz falls der Send-Roundtrip später streitig
        // wird ("Hab ich das wirklich abgeschickt?").
        if (snap.replacesDraftMessageId) {
          void invoke("delete_message", {
            messageId: snap.replacesDraftMessageId,
          }).catch((err) =>
            console.warn("draft cleanup after send failed:", err),
          );
        }
        // Pull fresh list so reply/forward icons show up server-side flag-flips.
        void refreshRef.current?.();
      } catch (e) {
        console.error("send_mail failed:", e);
        setStatus(t("compose.sendFailedToDraft", { detail: String(e) }));
        // Best-effort save-as-draft so the user's text isn't lost. If
        // even the draft-save fails we still show the snapshot in
        // Compose so they can copy it out manually.
        try {
          await invoke("save_draft", { request: buildSendRequest(snap) });
          reopenAsDraft(snap);
        } catch (e2) {
          console.error("save_draft fallback failed:", e2);
          reopenAsDraft(snap);
        }
      }
    },
    [t, buildSendRequest, reopenAsDraft],
  );

  /** Explicit "Save as Draft" button in Compose. Optimistic close (the
   *  Compose dialog is already gone by the time we get here), result
   *  surfaces as a status message. */
  const performSaveDraft = useCallback(
    async (snap: ComposeSendSnapshot) => {
      try {
        await invoke("save_draft", { request: buildSendRequest(snap) });
        setStatus(t("compose.draftSaved"));
        // Edit-Pfad: Original-Draft wegräumen damit der Drafts-Ordner
        // nicht mit jeder Bearbeitung dupliziert. Best-effort.
        if (snap.replacesDraftMessageId) {
          void invoke("delete_message", {
            messageId: snap.replacesDraftMessageId,
          }).catch((err) =>
            console.warn("draft cleanup after save_draft failed:", err),
          );
        }
        // Wenn der User gerade im Drafts-View steht, Liste neu lesen
        // damit der frisch gespeicherte Draft (und das verschwundene
        // Original) sichtbar werden.
        if (activeFolder === "drafts") {
          void refreshRef.current?.();
        }
      } catch (e) {
        console.error("save_draft failed:", e);
        setStatus(t("compose.draftSaveFailed", { detail: String(e) }));
        // Re-open the dialog so the user can retry / copy the body out.
        reopenAsDraft(snap);
      }
    },
    [t, activeFolder, buildSendRequest, reopenAsDraft],
  );

  /** Compose's Send button → schedule a 5 s undo-send buffer. Stash the
   *  snapshot in `pendingSend`; the overlay watches it. The actual
   *  invoke runs in `UndoSendOverlay` (which owns the countdown timer)
   *  via the `onTimeout` callback. */
  const enqueueSend = useCallback((snap: ComposeSendSnapshot) => {
    setPendingSend(snap);
  }, []);

  /** Overlay reports timer-expiry → kick off the actual send and
   *  clear the slot. */
  const onUndoSendTimeout = useCallback(
    (snap: ComposeSendSnapshot) => {
      setPendingSend(null);
      void performSend(snap);
    },
    [performSend],
  );

  /** Overlay reports user-cancel → drop the pending send and re-open
   *  Compose with the snapshot. */
  const onUndoSendCancel = useCallback(
    (snap: ComposeSendSnapshot) => {
      setPendingSend(null);
      reopenAsDraft(snap);
    },
    [reopenAsDraft],
  );

  /**
   * Pull fresh unread-count snapshots from the backend and store them
   * keyed by folder string. Called opportunistically after anything
   * that could shift the numbers — sync, refresh, mark-all-read,
   * optimistic archive/delete/move (which bumps the local `inbox`
   * state but that's a subset of the DB).
   */
  const refreshUnreadCounts = useCallback(async () => {
    try {
      const rows = await invoke<UnifiedUnreadCount[]>(
        "unified_unread_counts",
      );
      const map: Record<string, number> = {};
      for (const r of rows) map[r.folder] = r.unread;
      setUnreadCounts(map);
    } catch (e) {
      // Non-fatal — the previous snapshot stays on screen. Surface in
      // the console for debugging.
      console.error("unread counts fetch failed:", e);
    }
  }, []);

  // Window title + Windows taskbar overlay-icon both reflect the
  // unified-inbox unread count. Title is the universal fallback;
  // overlay-icon is the pCloud/Teams/Outlook-style red badge that
  // sticks onto the taskbar entry itself. Both run on every
  // unreadCount change — the cost is tiny and keeps them in
  // lockstep.
  useEffect(() => {
    const n = unreadCounts["inbox"] ?? 0;
    const title = n > 0 ? `CrystalMail (${n})` : "CrystalMail";
    void getCurrentWindow().setTitle(title).catch(() => {});

    const rgba = renderBadgeRgba(n);
    void invoke("set_unread_badge", {
      rgba: rgba ? Array.from(rgba) : null,
      width: 16,
      height: 16,
    }).catch(() => {
      // Non-Windows hosts or very early startup — don't spam the
      // console. The backend already no-ops when the main window
      // isn't up yet.
    });
  }, [unreadCounts]);

  // New-mail chime trigger. Compare the current unified-inbox unread
  // count against the previous snapshot; when it grows, that's new
  // mail arriving (either from sync or from optimistic mark-unread).
  // Guards:
  //   * First effect run (previous undefined): skip — the initial
  //     load reflects what was already on disk, not "new" arrivals.
  //   * Rate-limit: at most one chime every 5s so a big sync batch
  //     doesn't play a series of overlapping beeps.
  //   * User toggle: loaded once and refreshed on the
  //     `cm:notifications:changed` broadcast from the settings UI.
  const notifyPrefsRef = useRef<NotificationSettings>(
    loadNotificationSettings(),
  );
  useEffect(() => {
    const refresh = () => {
      notifyPrefsRef.current = loadNotificationSettings();
    };
    window.addEventListener("cm:notifications:changed", refresh);
    return () => {
      window.removeEventListener("cm:notifications:changed", refresh);
    };
  }, []);
  // Chime trigger lives on the sync-progress listener further down.
  // Gated on `done && newInInbox > 0` — `newInInbox` counts only
  // brand-new INBOX rows the writer actually INSERTED, not the
  // re-fetches of known UIDs the SINCE-30d window picks up every
  // sync. Local toggles like "mark as unread" never touch this
  // field, so they can't trip the sound either.
  const lastChimeAtRef = useRef<number>(0);

  // Refs für die im sync-progress-Listener gerufenen Funktionen.
  // Der Listener wird mit leeren Deps registriert (StrictMode-
  // Stabilität), würde also die initial-render-Closure einfangen.
  // Die Refs werden weiter unten via `useEffect` ge-update't, sobald
  // sich die Funktions-Identität ändert — Standardweg um den Listener
  // nicht bei jedem Render neu zu registrieren UND trotzdem die
  // aktuelle Funktion zu rufen.
  //
  // Typing wird via `Promise<void>`-Annotation bewusst handvergeben,
  // weil `typeof refresh` in TDZ-Bereich liegt (refresh ist weiter
  // unten als const definiert).
  const refreshRef = useRef<(() => Promise<void>) | null>(null);
  const refreshUnreadCountsRef = useRef<(() => Promise<void>) | null>(null);

  // Live mirror of the view-selection tuple so an in-flight `refresh`
  // can detect that the user navigated away during its IMAP/DB round-
  // trip. Without this, a captured-closure refresh (e.g. from
  // `syncAll`'s tail or a sync-progress auto-refresh that started
  // before the click) writes the *previous* folder's rows into the
  // inbox state and catapults the user out of the folder they're
  // currently looking at.
  const activeFolderRef = useRef(activeFolder);
  const selectedFolderRef = useRef(selectedFolder);
  const accountFilterRef = useRef(accountFilter);
  const searchQueryRef = useRef(searchQuery);
  useEffect(() => {
    activeFolderRef.current = activeFolder;
  }, [activeFolder]);
  useEffect(() => {
    selectedFolderRef.current = selectedFolder;
  }, [selectedFolder]);
  useEffect(() => {
    accountFilterRef.current = accountFilter;
  }, [accountFilter]);
  useEffect(() => {
    searchQueryRef.current = searchQuery;
  }, [searchQuery]);

  // Subscribe to live sync progress. Same StrictMode-cancellation
  // pattern as the pi chat-stream listener: the `listen()` resolution
  // is async, so a hastily-mounted-then-unmounted effect in dev would
  // otherwise double-register and apply every tick twice.
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    (async () => {
      const fn = await listen<SyncProgress>("sync-progress", (e) => {
        const p = e.payload;
        if (p.done) {
          // Hold the final numbers briefly so the user sees the "done"
          // state, then clear — otherwise the tooltip snaps to "idle"
          // the instant the last byte arrives and feels abrupt.
          setSyncProgress(p);
          window.setTimeout(() => {
            setSyncProgress((cur) =>
              cur && cur.accountId === p.accountId && cur.done ? null : cur,
            );
          }, 1500);
          // New-mail chime: ring once when this sync actually
          // *inserted* fresh INBOX rows. `fetched` re-counts every
          // UID the SINCE-30d window picks up — including ones we
          // already have — so a quiet re-sync would still report
          // a non-zero `fetched` and falsely ring. `newInInbox` is
          // the count of brand-new INSERTs into the INBOX folder
          // only; Sent/Drafts/Archive arrivals don't count, and
          // re-syncs of known UIDs don't count. Rate-limit 5 s
          // covers back-to-back multi-account syncs that genuinely
          // each delivered new mail (each account's done event
          // would otherwise fire its own chime).
          if (p.newInInbox > 0) {
            const prefs = notifyPrefsRef.current;
            if (prefs.soundEnabled) {
              const now = Date.now();
              if (now - lastChimeAtRef.current >= 5000) {
                lastChimeAtRef.current = now;
                playNotifySound(prefs.soundVolume);
              }
            }
            // Auto-Refresh der aktuellen Ansicht. Ohne das blieb die
            // Unified-Inbox-Liste nach einem IDLE-Push stehen, obwohl
            // die neue Mail schon in der DB lag — der User musste
            // manuell auf Refresh klicken. Mit dem `newInInbox > 0`-
            // Gate refreshen wir nur dann, wenn tatsächlich was Neues
            // ankam, also kein Flicker bei stillen Re-Syncs.
            //
            // Per-Folder-Sidebar-Counters laufen ebenfalls neu, damit
            // die "ungelesen"-Zahl im Account-Tree mit der Liste
            // synchron bleibt.
            void refreshRef.current?.();
            void refreshUnreadCountsRef.current?.();
          }
        } else {
          setSyncProgress(p);
        }
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

  // Externer Draft-Import-Trigger.
  //
  // Backend (`application/draft_import.rs`) liefert über das Tauri-
  // Event `compose-from-template` einen fertig substituierten
  // `PreparedImportDraft`, sobald ein Python-Script die App via
  // `crystalmail.exe --draft-from-template …` aufruft. Cold-Start-
  // Aufrufe (App lief noch nicht) landen erstmal im Backend-Puffer,
  // den wir hier nach dem ersten Mount einmalig leeren —
  // anschließend übernimmt die Live-Subscription.
  //
  // `accountsRef` hält die aktuelle Account-Liste, damit der Listener
  // (registriert mit leeren Deps für StrictMode-Stabilität) nicht
  // gegen die initial-leere Liste mappen muss.
  const accountsRef = useRef<AccountSummary[]>(accounts);
  useEffect(() => {
    accountsRef.current = accounts;
  }, [accounts]);

  useEffect(() => {
    let cancelled = false;
    const offFns: UnlistenFn[] = [];
    const apply = (imp: PreparedImportDraft) => {
      const draft = importDraftToComposeDraft(imp, accountsRef.current);
      setComposeDraft(draft);
    };
    (async () => {
      const fnDraft = await listen<PreparedImportDraft>(
        "compose-from-template",
        (e) => apply(e.payload),
      );
      const fnErr = await listen<{ message: string; sourceTemplate: string }>(
        "compose-from-template-error",
        (e) => setImportErrorBanner(e.payload),
      );
      if (cancelled) {
        fnDraft();
        fnErr();
      } else {
        offFns.push(fnDraft, fnErr);
      }
      // Pending-Puffer leeren (Cold-Start-Race). Bei mehreren Drafts
      // gewinnt der zuletzt gepushte den Composer — Wahrscheinlichkeit
      // > 1 bei normaler Bedienung praktisch null, und der UX-Kompromiss
      // (eine Compose-Instance gleichzeitig) ist im sonstigen UI auch
      // konsequent so.
      try {
        const pending = await invoke<PreparedImportDraft[]>(
          "consume_pending_import_drafts",
        );
        if (!cancelled && pending.length > 0) {
          apply(pending[pending.length - 1]);
        }
      } catch (err) {
        // Nicht-fatal — der Live-Listener läuft trotzdem.
        console.warn("consume_pending_import_drafts failed", err);
      }
    })();
    return () => {
      cancelled = true;
      for (const off of offFns) off();
    };
  }, []);

  // Auto-Dismiss für den Import-Fehler-Banner. Zeit lang genug, dass
  // man die Message gemütlich liest, kurz genug dass der Banner nicht
  // ewig im Weg steht.
  useEffect(() => {
    if (!importErrorBanner) return;
    const t = window.setTimeout(() => setImportErrorBanner(null), 12000);
    return () => window.clearTimeout(t);
  }, [importErrorBanner]);

  // Global postMessage bridge for the mail-iframe's link interceptor.
  // The sandboxed iframe forwards {type:"cm:open-url", href} up to us;
  // we hand it to the shell plugin which opens the OS-default browser.
  // Single listener on window — one effect for every Reader-iframe.
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const data = e.data as
        | { type?: string; href?: string }
        | null
        | undefined;
      if (!data || data.type !== "cm:open-url" || !data.href) return;
      // Allow http/https/mailto only — matches the capability
      // whitelist in tauri.conf. Anything else is silently dropped.
      if (!/^(https?:|mailto:)/i.test(data.href)) {
        console.warn("[crystalmail] open-url rejected (not http/https/mailto):", data.href);
        return;
      }
      console.log("[crystalmail] open-url:", data.href);
      void openUrl(data.href).catch((err: unknown) => {
        console.error("[crystalmail] open-url failed:", err);
      });
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  /**
   * Pure data-loader — given a limit, returns the rows that the current
   * view (folder + account filter + search query) should display.
   * Doesn't touch React state, just hits the backend. `loadMoreOlder`
   * also calls this directly so it can grow the window without the
   * ceremony of a full `refresh()`.
   *
   * Routing decisions live here, not in `refresh()`:
   *
   *   (1) Ad-hoc sub-folder pinned + search active ⇒ FTS via
   *       `search_advanced` with `folderId`. The "alle Ordner" toggle
   *       drops the folder pin entirely (nothing else makes sense in a
   *       sub-folder context), and an explicit `in:` operator from the
   *       DSL wins over both.
   *   (2) Ad-hoc sub-folder pinned, no search ⇒ plain folder listing.
   *   (3) Canonical view + search ⇒ FTS via `search_advanced` with
   *       `folder` (canonical key). The toggle widens to all-folders.
   *   (4) Canonical view, no search ⇒ unified-folder listing.
   *
   * Returns `null` on error so the caller can decide whether to keep
   * the current list visible (search syntax errors) or surface the
   * status (network blow-ups).
   */
  const fetchInboxFor = useCallback(
    async (limit: number): Promise<{
      rows: EnvelopeSummary[];
      searchError: string | null;
    } | null> => {
      const folderKey = activeFolder === "unified" ? "inbox" : activeFolder;
      try {
        // Ad-hoc sub-folder pinned: search and listing both go through
        // a folder-id constraint so FTS reaches mails in this folder
        // even when they aren't in the currently-loaded slice.
        if (selectedFolder) {
          if (searchQuery.length > 0) {
            const parsed = parseSearchQuery(searchQuery);
            // `in:foo` wins; otherwise the toggle decides folder scope.
            // Without override + toggle off ⇒ pin to the selected
            // folder via folderId (search_advanced doesn't have a
            // canonical key for ad-hoc folders).
            const useFolderId =
              parsed.folderOverride === undefined && !searchAllFolders;
            const rows = await invoke<EnvelopeSummary[]>("search_advanced", {
              fts: parsed.fts,
              folder: parsed.folderOverride ?? null,
              folderId: useFolderId ? selectedFolder.folderId : null,
              accountId: accountFilter,
              filters: parsed.filters,
              limit,
            });
            return {
              rows,
              searchError:
                parsed.errors.length > 0 ? parsed.errors.join("; ") : null,
            };
          }
          const rows = await invoke<EnvelopeSummary[]>(
            "list_folder_envelopes",
            {
              folderId: selectedFolder.folderId,
              limit,
              offset: 0,
            },
          );
          return { rows, searchError: null };
        }

        // Canonical view (unified inbox / archive / sent / …).
        if (searchQuery.length > 0) {
          const parsed = parseSearchQuery(searchQuery);
          const folderForSearch =
            parsed.folderOverride ?? (searchAllFolders ? null : folderKey);
          const rows = await invoke<EnvelopeSummary[]>("search_advanced", {
            fts: parsed.fts,
            folder: folderForSearch,
            folderId: null,
            accountId: accountFilter,
            filters: parsed.filters,
            limit,
          });
          return {
            rows,
            searchError:
              parsed.errors.length > 0 ? parsed.errors.join("; ") : null,
          };
        }
        const rows = await invoke<EnvelopeSummary[]>("list_unified_folder", {
          folder: folderKey,
          accountId: accountFilter,
          limit,
          offset: 0,
        });
        return { rows, searchError: null };
      } catch (e) {
        // FTS5 syntax errors surface here. Don't blow away the previous
        // list — let the caller decide. We signal "soft error" by
        // returning a result with `searchError` set when the query
        // path failed; for the listing path, propagate up.
        if (searchQuery.length > 0) {
          return { rows: [], searchError: String(e) };
        }
        setStatus(t("common.error", { message: String(e) }));
        return null;
      }
    },
    [
      t,
      activeFolder,
      accountFilter,
      searchQuery,
      searchAllFolders,
      selectedFolder,
    ],
  );

  const refresh = useCallback(
    async (limitOverride?: number) => {
      // Snapshot the view this fetch corresponds to. `fetchInboxFor`
      // closes over the same tuple, so its rows describe exactly this
      // selection. If the user clicks a different folder while the
      // backend round-trip is in flight, the live refs drift away
      // from the snapshot and we drop the result instead of writing
      // stale rows into the inbox state.
      const expectedFolder = activeFolder;
      const expectedSelected = selectedFolder?.folderId ?? null;
      const expectedAccountFilter = accountFilter;
      const expectedQuery = searchQuery;
      try {
        const accs = await invoke<AccountSummary[]>("list_accounts");
        setAccounts(sortAccounts(accs));
        // Fire-and-forget — don't gate the envelope fetch on this.
        void refreshUnreadCounts();

        // Search FTS uses bm25 ranking, so "limit" is really top-N —
        // give it the larger search cap. Browsing uses the sliding
        // `pageSize`. The view-change effect passes an explicit limit
        // so it doesn't have to wait for the React re-render after
        // setPageSize before its fetch sees the right value.
        const baseLimit =
          searchQuery.length > 0 ? PAGE_SIZE_SEARCH : pageSize;
        const limit = limitOverride ?? baseLimit;
        const result = await fetchInboxFor(limit);
        if (!result) return;
        if (
          activeFolderRef.current !== expectedFolder ||
          (selectedFolderRef.current?.folderId ?? null) !== expectedSelected ||
          accountFilterRef.current !== expectedAccountFilter ||
          searchQueryRef.current !== expectedQuery
        ) {
          return;
        }
        // Filter out optimistically-removed rows so a re-sync between
        // the user's delete/archive/move and the IMAP-side expunge
        // doesn't resurrect them.
        const tomb = optimisticallyRemovedRef.current;
        const visibleRows =
          tomb.size === 0
            ? result.rows
            : result.rows.filter((r) => !tomb.has(r.id));
        setInbox(visibleRows);
        setSearchError(result.searchError);
      } catch (e) {
        setStatus(t("common.error", { message: String(e) }));
      }
    },
    [
      t,
      pageSize,
      searchQuery,
      fetchInboxFor,
      refreshUnreadCounts,
      activeFolder,
      accountFilter,
      selectedFolder,
    ],
  );

  // Pendant zur Ref-Deklaration weiter oben: synchronisiert die Refs mit
  // den aktuellen useCallback-Identitäten, sobald `refresh` oder
  // `refreshUnreadCounts` neu erzeugt werden. Der sync-progress-Listener
  // ruft dann immer die jüngste Closure auf.
  useEffect(() => {
    refreshRef.current = refresh;
  }, [refresh]);
  useEffect(() => {
    refreshUnreadCountsRef.current = refreshUnreadCounts;
  }, [refreshUnreadCounts]);

  // One-shot startup probe + first fetch. Keeps db_ping from firing again
  // every time the folder selection changes.
  useEffect(() => {
    const timer = setTimeout(async () => {
      try {
        const probe = await invoke<string>("db_ping");
        setStatus(probe);
      } catch (e) {
        setStatus(t("common.error", { message: String(e) }));
        return;
      }
      await refresh();
      // Kick body prefetch for every known account. Backend respects each
      // account's `prefetchDays` and is a no-op when already running — so
      // it's safe to call redundantly. Without this, a user who closed the
      // app for days would cold-open mail via IMAP until the next sync.
      try {
        const accs = await invoke<AccountSummary[]>("list_accounts");
        for (const a of accs) {
          if (a.prefetchDays > 0) {
            void invoke("prefetch_account_bodies", { accountId: a.id });
          }
        }
      } catch {
        // non-fatal — prefetch is best-effort
      }
    }, 200);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Re-fetch the envelope list whenever folder, account filter, or the
  // debounced search query changes. Same effect resets the sliding
  // window — combining the two avoids a double-fetch on view change
  // (one with the old, one with the new pageSize). The explicit
  // `PAGE_SIZE_INITIAL` argument to `refresh` mirrors the pageSize
  // we're about to set, so the fetch doesn't have to wait for the
  // React re-render before seeing the right value. Selection is
  // cleared on folder/account switch but kept across search
  // refinement — typing into the search box shouldn't throw the
  // currently-open message out.
  useEffect(() => {
    setPageSize(PAGE_SIZE_INITIAL);
    void refresh(
      searchQuery.length > 0 ? PAGE_SIZE_SEARCH : PAGE_SIZE_INITIAL,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeFolder, accountFilter, searchQuery, selectedFolder]);

  useEffect(() => {
    setSelectedId(undefined);
  }, [activeFolder, accountFilter, selectedFolder]);

  // Lazy-on-open: when the user picks an ad-hoc sub-folder from the
  // sidebar, kick off a background sync of its 50 newest envelopes so
  // the list populates even if this folder has never been synced
  // before. TTL-gated backend-side — rapid folder cycling doesn't
  // hammer the server. Specials and unified/starred views are already
  // covered by the main sync button, so we only care about
  // `selectedFolder` here.
  useEffect(() => {
    if (!selectedFolder) return;
    const folderId = selectedFolder.folderId;
    let cancelled = false;
    void (async () => {
      try {
        const report = await invoke<SyncReport>("sync_folder_recent", {
          folderId,
          limit: 50,
        });
        // Only refresh when (a) we actually pulled something and (b)
        // the user hasn't navigated away since we kicked off.
        if (!cancelled && report.stored > 0) {
          await refresh();
        }
      } catch (e) {
        // Non-fatal — the DB-cached listing is already showing.
        console.warn("lazy sync_folder_recent failed:", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [selectedFolder, refresh]);

  // Scroll-to-bottom pager. Two-phase: first grow the visible window
  // by another `PAGE_SIZE_STEP` rows from the local DB; if the new
  // fetch returns the same count we had before, the DB is exhausted at
  // this size — fall back to an explicit IMAP "give me older" round.
  // `loadingOlderRef` guards against repeat fires while a load is in
  // flight (onScroll can emit dozens of events a second). `exhaustedRef`
  // latches when an IMAP-older round comes back empty — no point
  // hammering the backend for more if the server has no older mail.
  //
  // The exhaustion latch is keyed per view so switching folders resets
  // it implicitly (a different key won't match).
  const loadingOlderRef = useRef(false);
  const exhaustedRef = useRef<string | null>(null);
  const viewKey = useMemo(() => {
    if (selectedFolder) return `folder:${selectedFolder.folderId}`;
    return `canonical:${activeFolder}|${accountFilter ?? "all"}`;
  }, [selectedFolder, activeFolder, accountFilter]);

  const loadMoreOlder = useCallback(() => {
    if (loadingOlderRef.current) return;
    // Pagination during search makes no sense — search results are
    // already top-N relevance hits across the whole DB, not a paged
    // chronological window. Pulling older from IMAP wouldn't add to
    // the visible result set without re-running FTS.
    if (searchQuery.length > 0) return;
    if (exhaustedRef.current === viewKey) return;
    loadingOlderRef.current = true;
    void (async () => {
      try {
        const beforeLen = inbox.length;
        const nextSize = pageSize + PAGE_SIZE_STEP;

        // Phase 1: grow the visible window from the local cache. Cheap
        // — pure SQL — and covers the common case where the user has
        // way more mail in the DB than the initial window showed.
        const grown = await fetchInboxFor(nextSize);
        const tomb = optimisticallyRemovedRef.current;
        const grownVisible =
          grown && tomb.size > 0
            ? { ...grown, rows: grown.rows.filter((r) => !tomb.has(r.id)) }
            : grown;
        if (grownVisible) {
          setInbox(grownVisible.rows);
          setSearchError(grownVisible.searchError);
        }

        const grewLocally =
          (grownVisible?.rows.length ?? beforeLen) > beforeLen;
        if (grewLocally) {
          setPageSize(nextSize);
          return;
        }

        // Phase 2: DB exhausted at this size. Ask IMAP for more. The
        // ad-hoc-folder path uses the existing per-folder pager; the
        // canonical-view path fans out across each account that
        // contributes to the bucket via `sync_unified_folder_older`.
        let stored = 0;
        if (selectedFolder) {
          const r = await invoke<SyncReport>("sync_folder_older", {
            folderId: selectedFolder.folderId,
            limit: SYNC_OLDER_BATCH,
          });
          stored = r.stored;
        } else {
          const folderKey =
            activeFolder === "unified" ? "inbox" : activeFolder;
          // Starred is a flag-filtered view, not a real folder — it
          // can't be paged. Just latch and stop.
          if (folderKey !== "starred") {
            const r = await invoke<SyncReport>("sync_unified_folder_older", {
              folder: folderKey,
              accountId: accountFilter,
              limit: SYNC_OLDER_BATCH,
            });
            stored = r.stored;
          }
        }

        if (stored > 0) {
          // New rows landed in the DB — re-fetch with the bigger window
          // so they show up.
          const refreshed = await fetchInboxFor(nextSize);
          if (refreshed) {
            const tomb2 = optimisticallyRemovedRef.current;
            const visibleRows =
              tomb2.size === 0
                ? refreshed.rows
                : refreshed.rows.filter((r) => !tomb2.has(r.id));
            setInbox(visibleRows);
            setSearchError(refreshed.searchError);
          }
          setPageSize(nextSize);
        } else {
          exhaustedRef.current = viewKey;
        }
      } catch (e) {
        console.warn("loadMoreOlder failed:", e);
      } finally {
        loadingOlderRef.current = false;
      }
    })();
  }, [
    inbox.length,
    pageSize,
    searchQuery,
    selectedFolder,
    activeFolder,
    accountFilter,
    viewKey,
    fetchInboxFor,
  ]);

  // If the currently filtered account gets deleted, fall back to "all".
  useEffect(() => {
    if (
      accountFilter &&
      accounts.length > 0 &&
      !accounts.some((a) => a.id === accountFilter)
    ) {
      setAccountFilter(null);
    }
  }, [accounts, accountFilter]);

  const syncAll = useCallback(async () => {
    if (syncInFlight.current || accounts.length === 0) return;
    syncInFlight.current = true;
    setSyncing(true);
    let total = 0;
    let totalMs = 0;
    const errors: string[] = [];
    // Resolve the folder the user is currently looking at to the raw
    // IMAP name *per account* — the backend uses that name verbatim in
    // a SELECT. When the user is in a unified/starred view there isn't
    // one server folder to prioritise, so we fall back to the flat
    // sync (priority=null). selectedFolder is also left for the
    // (later) folder_id-based lazy-sync path.
    const priorityFor = (a: (typeof accounts)[number]): string | null => {
      switch (activeFolder) {
        // "unified" == the unified inbox across all accounts, so the
        // equivalent server folder per account is literally INBOX.
        case "unified":
          return "INBOX";
        case "archive":
          return a.archiveFolder;
        case "sent":
          return a.sentFolder;
        case "drafts":
          return a.draftsFolder;
        case "trash":
          return a.trashFolder;
        case "spam":
          return a.spamFolder;
        // "starred" is a cross-folder flag filter, no single server
        // folder maps to it — fall through to the flat sync.
        case "starred":
          return null;
        // "contacts" ist kein Sync-Target — Kontakte-View ist client-
        // side derived. Sync-All trotzdem regulär durchlaufen lassen
        // damit auch in dem View ein Refresh greift, aber kein
        // Priority-Folder.
        case "contacts":
          return null;
        // "calendar" ist ebenfalls client-derived (lokaler Store, Phase 1).
        // Kein IMAP-Sync hängt am Calendar-View.
        case "calendar":
          return null;
      }
    };
    try {
      for (const a of accounts) {
        setStatus(t("sync.perAccount", { name: a.displayName }));
        try {
          const r = await invoke<SyncReport>("sync_account", {
            accountId: a.id,
            priorityFolder: priorityFor(a),
          });
          total += r.stored;
          totalMs += r.durationMs;
        } catch (e) {
          errors.push(`${a.displayName}: ${String(e)}`);
        }
      }
      if (errors.length > 0) {
        setStatus(errors.join(" | "));
      } else {
        setStatus(t("sync.done", { count: total, ms: totalMs }));
      }
      lastSyncAt.current = Date.now();
      await refresh();
    } finally {
      setSyncing(false);
      syncInFlight.current = false;
    }
  }, [accounts, activeFolder, t, refresh]);

  // Keep the ref pointing at the latest syncAll so the hotkey hook (which
  // captured callbacks on mount) can always invoke the current version.
  useEffect(() => {
    syncAllRef.current = syncAll;
  }, [syncAll]);

  /**
   * "Alle als gelesen" — collects ids of every unread envelope in the
   * current view (respects account filter + search + sub-folder pin)
   * and hands them to the batch backend command. Local state is
   * flipped optimistically so the user sees unread markers vanish
   * instantly; the backend report adjusts the status bar.
   */
  const markAllInView = useCallback(async () => {
    const unreadIds = inbox.filter((m) => !m.seen).map((m) => m.id);
    if (unreadIds.length === 0) {
      setStatus(t("mutation.markAllReadNoneFound"));
      return;
    }
    // Optimistic: flip every unread to seen right now. On partial
    // failure the backend report gives us the real count; we leave
    // the UI in its optimistic state and just show the discrepancy.
    setInbox((rows) =>
      rows.map((r) => (r.seen ? r : { ...r, seen: true })),
    );
    // Badge: drop by the number we just flipped. This keeps the
    // sidebar counter in sync without waiting for a refresh.
    bumpUnreadCount(currentUnreadKey, -unreadIds.length);
    setPendingMutations((n) => n + 1);
    try {
      const report = await invoke<{
        marked: number;
        failed: number;
        requested: number;
      }>("mark_messages_read", { messageIds: unreadIds });
      if (report.failed > 0) {
        setStatus(
          t("mutation.markAllReadPartial", {
            marked: report.marked,
            requested: report.requested,
          }),
        );
      } else {
        setStatus(
          t("mutation.markAllReadDone", { count: report.marked }),
        );
      }
    } catch (e) {
      console.error("mark_messages_read failed:", e);
      setStatus(t("mutation.markAllReadFailed", { detail: String(e) }));
      // Revert optimism — easiest is to trigger a refresh so we pull
      // the real server state back in.
      await refresh();
    } finally {
      setPendingMutations((n) => Math.max(0, n - 1));
    }
  }, [inbox, t, refresh, bumpUnreadCount, currentUnreadKey]);

  useEffect(() => {
    markAllReadRef.current = markAllInView;
  }, [markAllInView]);

  // Auto-sync on folder switch, but only when the previous sync is stale
  // enough. Guards rapid folder cycling against pointless re-fetches.
  // Only fires after the initial mount — the first render already triggered
  // a refresh via the mount effect.
  const didMount = useRef(false);
  useEffect(() => {
    if (!didMount.current) {
      didMount.current = true;
      return;
    }
    if (accounts.length === 0) return;
    const last = lastSyncAt.current;
    const stale = last === null || Date.now() - last >= AUTO_SYNC_COOLDOWN_MS;
    if (stale && !syncInFlight.current) {
      void syncAll();
    }
    // Intentionally only watching activeFolder — the filter changing alone
    // isn't a reason to hit the server.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeFolder]);

  return (
    <div className="flex h-full w-full flex-col">
      <div className="flex min-h-0 flex-1">
        <Sidebar
          active={activeFolder}
          onSelect={setActiveFolder}
          accounts={accounts}
          onSyncAll={syncAll}
          onCompose={openBlankCompose}
          onOpenSettings={() => setSettingsOpen(true)}
          syncing={syncing}
          syncProgress={syncProgress}
          selectedFolder={selectedFolder}
          onSelectFolder={setSelectedFolder}
          unreadCounts={unreadCounts}
        />
        {activeFolder === "calendar" ? (
          <div className="min-h-0 flex-1">
            <CalendarView />
          </div>
        ) : activeFolder === "contacts" ? (
          <>
            <div
              className="flex w-[22.5rem] shrink-0 flex-col border-r"
              style={{ borderColor: "var(--border-base)" }}
            >
              <ContactsView
                selectedId={
                  selectedContactId === null ? undefined : selectedContactId
                }
                onSelect={setSelectedContactId}
                refreshKey={contactsRefreshKey}
                onCreateNew={() => setSelectedContactId(null)}
              />
            </div>
            <div className="min-h-0 flex-1">
              <ContactDetail
                contactId={
                  selectedContactId === null ? undefined : selectedContactId
                }
                newMode={selectedContactId === null}
                onSaved={(id) => {
                  setSelectedContactId(id);
                  setContactsRefreshKey((k) => k + 1);
                }}
                onDeleted={() => {
                  setSelectedContactId(undefined);
                  setContactsRefreshKey((k) => k + 1);
                }}
                onCancel={() => setSelectedContactId(undefined)}
                onCompose={(d) => setComposeDraft(d)}
                onOpenMessage={(id) => {
                  // Sprung aus dem Kontakte-Mode zurück ins Mail-View.
                  // Unified-Inbox als Default — der Reader fetcht eh
                  // per messageId, also ist die Liste "drumherum" für
                  // die Anzeige der ausgewählten Mail egal. Falls die
                  // Mail im Sent/Trash/Archive ist, sieht der User die
                  // Selektion nicht in der Liste, aber den Inhalt im
                  // Reader. Pragmatisch genug; mehr Aufwand wäre die
                  // folder-Lookup-Pipeline um den canonical-folder-Key
                  // aus envelope.folderId zu finden.
                  setActiveFolder("unified");
                  setSelectedId(id);
                }}
              />
            </div>
          </>
        ) : (
        <>
        <div
          // rem-based so it grows with Ctrl+/-; ≈ 360px at 16px root.
          className="flex w-[22.5rem] shrink-0 flex-col border-r"
          style={{ borderColor: "var(--border-base)" }}
        >
          <AccountFilterBar
            accounts={accounts}
            selectedAccountId={accountFilter}
            onSelect={setAccountFilter}
          />
          {/*
            Spam-Kandidaten-Banner. Sichtbar sobald der User mindestens
            eine Mail im aktuellen (Nicht-Spam-)View als Verdacht mit
            `j` markiert hat. Ein Klick öffnet den pi-Lern-Dialog, der
            genau diese IDs als Analyse-Korpus bekommt.
          */}
          {!inSpamView &&
            inbox.some((m) => m.junk) &&
            (() => {
              const candidateIds = inbox.filter((m) => m.junk).map((m) => m.id);
              return (
                <button
                  type="button"
                  onClick={() => setLearnSpamOpen(true)}
                  className="flex w-full items-center justify-between gap-2 border-b px-3 py-2 text-left text-xs transition-colors"
                  style={{
                    background: "rgba(234,179,8,0.10)",
                    borderColor: "rgba(234,179,8,0.25)",
                    color: "var(--fg-base)",
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.background =
                      "rgba(234,179,8,0.18)";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.background =
                      "rgba(234,179,8,0.10)";
                  }}
                  title={t("learnSpam.bannerTooltip")}
                >
                  <span className="flex items-center gap-2">
                    <span
                      aria-hidden
                      className="inline-flex h-4 items-center rounded px-1 text-[10px] font-semibold"
                      style={{
                        background: "rgba(234,179,8,0.2)",
                        color: "#ca8a04",
                      }}
                    >
                      {candidateIds.length}
                    </span>
                    <span>
                      {t("learnSpam.bannerLabel", {
                        count: candidateIds.length,
                      })}
                    </span>
                  </span>
                  <span style={{ color: "var(--fg-subtle)" }}>→</span>
                </button>
              );
            })()}
          <div className="min-h-0 flex-1">
            <InboxList
              items={inbox}
              selectedId={selectedId}
              onSelect={setSelectedId}
              onActivate={(id) => void onEnvelopeActivate(id)}
              searchValue={searchInput}
              onSearchChange={setSearchInput}
              onSearchSubmit={(q) => {
                // Push the *raw* user input into history — chips
                // re-run literally what was typed (typos and all),
                // matching Spark's behaviour. Pure-whitespace queries
                // are filtered out inside `pushRecentSearch`.
                setRecentSearches(pushRecentSearch(q));
              }}
              searchError={searchError}
              searching={searchInput.trim() !== searchQuery}
              junkBadgeTone={inSpamView ? "confirmed" : "candidate"}
              embedded
              onNearBottom={loadMoreOlder}
              trainingIds={trainingIds}
              recentSearches={recentSearches}
              onRemoveRecent={(q) => setRecentSearches(removeRecentSearch(q))}
              searchAllFolders={searchAllFolders}
              onSearchAllFoldersChange={setSearchAllFolders}
            />
          </div>
        </div>
        <Reader
          selectedId={selectedId}
          accounts={accounts}
          onCompose={(d) => setComposeDraft(d)}
          onArchiveRequest={onArchiveRequest}
          onDeleteRequest={onDeleteRequest}
          onMoveRequest={onMoveRequest}
          onMarkSpamRequest={onMarkSpamRequest}
          onSpamCandidateRequest={onSpamCandidateRequest}
          onShowContact={(id) => {
            setSelectedContactId(id);
            setActiveFolder("contacts");
          }}
          onFlagsChanged={(id, flags) => {
            // Mirror the change into the inbox list so the unread/answered/
            // forwarded indicators update instantly, without a full refresh.
            // At the same time, adjust the sidebar unread counter if the
            // seen-bit flipped — covers both the explicit u-toggle and
            // the auto-mark-seen-after-1.2s path in the Reader.
            setInbox((rows) => {
              let seenDelta = 0;
              const next = rows.map((r) => {
                if (r.id !== id) return r;
                if (r.seen !== flags.seen) {
                  // unread → seen is -1 on the badge; seen → unread is +1.
                  seenDelta = flags.seen ? -1 : +1;
                }
                return {
                  ...r,
                  seen: flags.seen,
                  answered: flags.answered,
                  flagged: flags.flagged,
                  forwarded: flags.forwarded,
                };
              });
              if (seenDelta !== 0) {
                bumpUnreadCount(currentUnreadKey, seenDelta);
              }
              return next;
            });
          }}
        />
        </>
        )}
      </div>

      <PiTerminal
        expanded={piExpanded}
        onToggle={() => setPiExpanded((v) => !v)}
        selectedMessageId={selectedId}
        inbox={inbox}
        activeFolderLabel={t(`inbox.${activeFolder}`)}
        onSelectMessage={(id) => {
          setSelectedId(id);
        }}
        onComposeFromPi={(intent, body, ctxId) =>
          void onComposeFromPi(intent, body, ctxId)
        }
      />

      <footer
        className="flex items-center justify-between border-t px-3 py-1.5 text-[11px]"
        style={{
          borderColor: "var(--border-base)",
          background: "var(--bg-panel)",
          color: "var(--fg-subtle)",
        }}
      >
        {/* While a sync is running, live progress pre-empts the
            status line — guaranteed-reactive unlike the hover-tooltip
            on the sync icon (which fights HTML's static `title`
            attribute). `syncProgress` clears 1.5 s after completion,
            at which point the regular status message takes over. */}
        <span>
          {syncProgress
            ? syncProgress.done
              ? t("sync.tooltipDone", { account: syncProgress.accountName })
              : syncProgress.total > 0
                ? t("sync.tooltipActive", {
                    account: syncProgress.accountName,
                    folder: syncProgress.folder || "…",
                    fetched: syncProgress.fetched,
                    total: syncProgress.total,
                  })
                : t("sync.tooltipActiveNoTotal", {
                    account: syncProgress.accountName,
                    folder: syncProgress.folder || "…",
                  })
            : status}
        </span>
        <div className="flex items-center gap-3">
          {pendingMutations > 0 && (
            <span
              className="inline-flex items-center gap-1 rounded px-1.5 text-[11px]"
              style={{
                color: "var(--fg-muted)",
                background: "var(--bg-hover)",
              }}
              title={t("mutation.pendingTooltip")}
            >
              <span
                aria-hidden
                className="inline-block"
                style={{
                  animation: "cm-spin 1.4s linear infinite",
                }}
              >
                ↻
              </span>
              {t("mutation.pending", { count: pendingMutations })}
            </span>
          )}
          {/* AI kill-switch indicator. Always visible — when AI is on
              we show a quiet "KI ein" pill, when off a louder red one
              so the user can't accidentally wonder why pi-features
              suddenly do nothing. Clicking flips the flag (with
              optimistic update + automatic event broadcast via
              useAiEnabled). */}
          <button
            type="button"
            onClick={() => {
              void setAiEnabled(!aiEnabled).catch((e) => {
                setStatus(t("common.error", { message: String(e) }));
              });
            }}
            title={
              aiEnabled
                ? t("aiSwitch.tooltipOn")
                : t("aiSwitch.tooltipOff")
            }
            className="rounded px-1.5 text-[10px] font-semibold uppercase tracking-wider"
            style={{
              color: aiEnabled ? "#16a34a" : "#ef4444",
              background: aiEnabled
                ? "rgba(34,197,94,0.10)"
                : "rgba(239,68,68,0.15)",
            }}
          >
            {aiEnabled ? t("aiSwitch.on") : t("aiSwitch.off")}
          </button>
          <button
            type="button"
            onClick={() => setHotkeyHelp(true)}
            title={t("hotkeys.open")}
            className="rounded px-1 text-[11px] hover:underline"
            style={{ color: "var(--fg-subtle)" }}
          >
            ?
          </button>
          <button
            type="button"
            onClick={zoom.reset}
            title={t("zoom.resetTooltip")}
            className="rounded px-1 text-[11px] hover:underline"
            style={{ color: "var(--fg-subtle)" }}
          >
            {Math.round((zoom.size / zoom.default) * 100)}%
          </button>
          <span>
            {selectedFolder ? selectedFolder.name : t(`inbox.${activeFolder}`)}
          </span>
        </div>
      </footer>

      {composeDraft && (
        <Compose
          draft={composeDraft}
          accounts={accounts}
          onClose={() => setComposeDraft(null)}
          onSendRequest={(snap) => {
            // Compose hat bereits onClose() gerufen → wir landen unten
            // im Undo-Send-Overlay-Pfad. Snapshot in den Buffer
            // schmeißen, das Overlay zählt 5s runter.
            enqueueSend(snap);
          }}
          onSaveDraft={(snap) => {
            // Optimistic: Compose-Dialog ist schon zu, der eigentliche
            // IMAP-APPEND läuft im Hintergrund. Status / Fehler kommen
            // als Toast, nicht modal.
            void performSaveDraft(snap);
          }}
        />
      )}

      {/* Undo-Send-Overlay sitzt unten am Footer-Rand und ist nicht-
          blockierend (`pointer-events` auf den umgebenden Container
          beschränkt). Beim Cancel landet die Mail wieder im Compose;
          beim Timeout läuft der echte SMTP-Send. */}
      {pendingSend && (
        <UndoSendOverlay
          snapshot={pendingSend}
          onTimeout={onUndoSendTimeout}
          onCancel={onUndoSendCancel}
        />
      )}

      {hotkeyHelp && (
        <HotkeyHelp
          onClose={() => setHotkeyHelp(false)}
          bindings={hotkeyBindings}
        />
      )}

      {commandPaletteOpen && (
        <CommandPalette
          bindings={hotkeyBindings}
          hasSelection={!!selectedId}
          callbacks={{
            onCompose: () => openBlankCompose(),
            onSyncAll: () => {
              void syncAllRef.current?.();
            },
            onMarkAllRead: () => {
              void markAllReadRef.current?.();
            },
            onShowHelp: () => setHotkeyHelp(true),
            onShowSettings: () => setSettingsOpen(true),
            // Picking "Befehlspalette öffnen" from inside the open
            // palette would be a no-op recursion; the palette already
            // hides this row. Wire it anyway so the type matches.
            onShowCommandPalette: () => setCommandPaletteOpen(true),
            onEscape: () => setCommandPaletteOpen(false),
          }}
          onClose={() => setCommandPaletteOpen(false)}
        />
      )}

      {/* SettingsDialog must render BEFORE AddAccountDialog so that when the
          user triggers add/edit from the accounts panel, the dialog stacks
          on top (same z-index, later DOM wins). */}
      {settingsOpen && (
        <SettingsDialog
          onClose={() => {
            setSettingsOpen(false);
            // Don't leak the deep-link target across opens — next
            // plain Settings open should land on Accounts again.
            setSettingsInitialCategory(undefined);
          }}
          initialCategory={settingsInitialCategory}
          hotkeys={hotkeyBindings}
          onHotkeysChange={(next) => {
            setHotkeyBindings(next);
            saveHotkeys(next);
          }}
          accounts={accounts}
          onAddAccount={() => setDialog(null)}
          onEditAccount={(a) => setDialog(a)}
          onReorderAccounts={setAccounts}
        />
      )}

      {learnSpamOpen && (
        <LearnSpamRuleDialog
          candidateIds={inbox.filter((m) => m.junk).map((m) => m.id)}
          onClose={() => setLearnSpamOpen(false)}
          onApplied={() => void refresh()}
          onOpenAiSettings={() => {
            // Dialog closes itself before calling this — we just open
            // Settings on the pi pane.
            setSettingsInitialCategory("pi");
            setSettingsOpen(true);
          }}
        />
      )}

      {dialog !== undefined && (
        <AddAccountDialog
          initial={dialog ?? undefined}
          onClose={() => setDialog(undefined)}
          onSaved={async () => {
            setDialog(undefined);
            await refresh();
          }}
          onDeleted={async () => {
            setDialog(undefined);
            await refresh();
          }}
        />
      )}

      {/* Subscribes to `workflow-rule-match` and stacks one toast per
          confirm-mode hit. Rendered outside the layout tree so the
          toasts float independently of inbox/reader state. */}
      <WorkflowRuleToastStack />

      {/* Import-Trigger-Fehler (CLI: `--draft-from-template` etc.).
          Floatet rechts oben, eigene z-Stage über dem Composer, weil
          der Trigger oft genau dann scheitert wenn der User den
          Composer kurz vorher erwartet hat — er soll sofort sehen
          warum nichts kam. Auto-Dismiss läuft als separater Effect. */}
      {importErrorBanner && (
        <div
          role="alert"
          className="pointer-events-auto fixed right-4 top-4 z-[80] flex max-w-md flex-col gap-1 rounded-md border px-3 py-2 text-xs shadow-xl"
          style={{
            background: "var(--bg-panel)",
            borderColor: "#ef4444",
            color: "var(--fg-base)",
          }}
        >
          <div className="flex items-start justify-between gap-3">
            <strong style={{ color: "#ef4444" }}>Draft-Import fehlgeschlagen</strong>
            <button
              type="button"
              onClick={() => setImportErrorBanner(null)}
              className="text-[11px]"
              style={{ color: "var(--fg-muted)" }}
              aria-label="Schließen"
            >
              ✕
            </button>
          </div>
          <div style={{ color: "var(--fg-base)" }}>
            {importErrorBanner.message}
          </div>
          {importErrorBanner.sourceTemplate && (
            <div
              className="mt-1 truncate text-[11px]"
              style={{ color: "var(--fg-subtle)" }}
              title={importErrorBanner.sourceTemplate}
            >
              {importErrorBanner.sourceTemplate}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
