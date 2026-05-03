import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { EnvelopeSummary } from "../types";
import { scheduledGlyph, scheduledTooltip } from "../utils/lifetimeTag";
import { SearchPanel } from "./SearchPanel";

type Props = {
  items: EnvelopeSummary[];
  selectedId?: string;
  onSelect: (id: string) => void;
  /** Optional: fired on double-click of a row. Currently only used to
   *  re-open drafts in Compose for editing — for non-draft mails the
   *  parent typically ignores this (single-click already opens the
   *  Reader). When omitted, double-click falls back to a regular
   *  selection. */
  onActivate?: (id: string) => void;
  /** Search box is controlled from App so the query can drive the backend
   *  fetch and reset on folder switch. */
  searchValue: string;
  onSearchChange: (v: string) => void;
  /** Optional: if set, called on Enter or chip-pick to commit the query
   *  (push to history). When omitted, history isn't recorded — useful
   *  when the search box is used in a transient context. */
  onSearchSubmit?: (v: string) => void;
  searchError?: string | null;
  searching?: boolean;
  /** When embedded in a parent column (with the account filter above), skip
   *  the fixed width + right border — the parent owns the chrome. */
  embedded?: boolean;
  /**
   * Tone for the SPAM badge: "candidate" (yellow) when mails are shown in
   * a non-Spam folder (the user pre-marked them with `j`), "confirmed"
   * (red) when the view *is* the Spam folder (the flag there means
   * "classified as spam"). Defaults to candidate.
   */
  junkBadgeTone?: "candidate" | "confirmed";
  /**
   * Called when the viewport gets close to the bottom of the list and
   * an additional page should be loaded. Parent owns debounce / state
   * (no double-fires while a load is already in flight). Omitted for
   * views that don't paginate (search, unified).
   */
  onNearBottom?: () => void;
  /**
   * IDs of messages currently marked as workflow-training candidates.
   * Used to render a TRAIN badge next to the SPAM one — visual parity
   * with the spam-candidate flow. Omitted = no badges.
   */
  trainingIds?: Set<string>;

  // ── Search-panel wiring ──────────────────────────────────────────
  /** Recent-search history (most-recent-first). Empty array hides
   *  the "Kürzliche Suchen" block. Caller persists this list — see
   *  `utils/recentSearches.ts`. */
  recentSearches?: string[];
  /** Callback when user clicks the × on a recent chip. */
  onRemoveRecent?: (q: string) => void;
  /** "Über alle Ordner suchen" toggle state. When omitted, the toggle
   *  is hidden entirely (used in views where folder scope is
   *  meaningless, e.g. unified-inbox already spans folders). */
  searchAllFolders?: boolean;
  onSearchAllFoldersChange?: (v: boolean) => void;
};

export function InboxList({
  items,
  selectedId,
  onSelect,
  onActivate,
  searchValue,
  onSearchChange,
  onSearchSubmit,
  searchError,
  searching,
  embedded,
  junkBadgeTone = "candidate",
  onNearBottom,
  trainingIds,
  recentSearches,
  onRemoveRecent,
  searchAllFolders,
  onSearchAllFoldersChange,
}: Props) {
  const { t } = useTranslation();
  const listRef = useRef<HTMLUListElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  // The empty-state panel only shows while the input has focus AND
  // the field is empty — same trigger as Spark's. We track focus
  // locally; folder/account context is the parent's concern.
  const [searchFocused, setSearchFocused] = useState(false);
  const showPanel =
    searchFocused && !searchValue && (recentSearches?.length ?? 0) >= 0;

  // Scroll the selected row into view when it changes. Triggered by ↑/↓
  // keyboard navigation and by the auto-advance after archive/delete —
  // otherwise the selection highlight would disappear off-screen in long
  // lists. `block: "nearest"` means no jump when the row is already
  // visible — purely corrective scrolling.
  useEffect(() => {
    if (!selectedId) return;
    const el = listRef.current?.querySelector<HTMLLIElement>(
      `li[data-id="${selectedId}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [selectedId]);

  return (
    <section
      className={
        embedded
          ? "flex h-full w-full flex-col"
          : "flex w-[22.5rem] shrink-0 flex-col border-r"
      }
      style={{
        background: "var(--bg-base)",
        borderColor: embedded ? undefined : "var(--border-base)",
      }}
    >
      <header
        className="border-b px-3 py-2"
        style={{ borderColor: "var(--border-soft)" }}
      >
        <div className="relative">
          <input
            ref={inputRef}
            type="search"
            value={searchValue}
            onChange={(e) => onSearchChange(e.target.value)}
            onFocus={() => setSearchFocused(true)}
            onBlur={() => setSearchFocused(false)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && onSearchSubmit) {
                onSearchSubmit(searchValue);
                inputRef.current?.blur();
              } else if (e.key === "Escape") {
                inputRef.current?.blur();
              }
            }}
            placeholder={t("search.placeholder")}
            className="w-full rounded-md px-3 py-1.5 pr-16 text-sm outline-none focus:ring-2 transition-[box-shadow]"
            style={{
              background: "var(--bg-panel)",
              color: "var(--fg-base)",
              border: `1px solid ${searchError ? "#ef4444" : "var(--border-base)"}`,
            }}
          />
          {searchValue && (
            <button
              type="button"
              onClick={() => onSearchChange("")}
              aria-label={t("search.clear")}
              className="absolute right-2 top-1/2 -translate-y-1/2 rounded px-1.5 text-[11px]"
              style={{ color: "var(--fg-subtle)" }}
              title={t("search.clear")}
            >
              ✕
            </button>
          )}
          {searching && !searchError && (
            <span
              aria-hidden
              className="absolute right-8 top-1/2 -translate-y-1/2 text-[10px]"
              style={{ color: "var(--fg-subtle)" }}
            >
              …
            </span>
          )}
          {showPanel && (
            <SearchPanel
              anchorRef={inputRef}
              recents={recentSearches ?? []}
              onPickRecent={(q) => {
                onSearchChange(q);
                onSearchSubmit?.(q);
                inputRef.current?.blur();
              }}
              onRemoveRecent={(q) => onRemoveRecent?.(q)}
              onPickTip={(example) => {
                // Insert tip into the input but keep focus so the
                // user can edit (`from:alex` → `from:erika`) before
                // submitting. The blur-on-submit only fires when the
                // user actually presses Enter on a real query.
                onSearchChange(example);
                inputRef.current?.focus();
              }}
            />
          )}
        </div>
        {searchError && (
          <p
            className="mt-1.5 text-[11px]"
            style={{ color: "#ef4444" }}
            title={searchError}
          >
            {t("search.error")}
          </p>
        )}
        {!searchError && searchValue && (
          <p
            className="mt-1 text-[10px]"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("search.hint")}
          </p>
        )}
        {/* "Über alle Ordner suchen" toggle — only when the parent
            actually wants this control rendered (omitted on unified
            views where it'd be redundant). Sits here so it lives next
            to the input but can't intercept clicks meant for the
            search panel above. */}
        {onSearchAllFoldersChange !== undefined && (
          <label
            className="mt-1.5 flex select-none items-center gap-1.5 text-[11px]"
            style={{ color: "var(--fg-subtle)" }}
            title={t("search.allFoldersTooltip")}
          >
            <input
              type="checkbox"
              checked={!!searchAllFolders}
              onChange={(e) => onSearchAllFoldersChange(e.target.checked)}
              className="h-3 w-3 cursor-pointer"
            />
            <span className="cursor-pointer">{t("search.allFolders")}</span>
          </label>
        )}
      </header>

      <div
        className="flex-1 overflow-y-auto"
        onScroll={(e) => {
          if (!onNearBottom) return;
          // Fire when the bottom edge comes within 100px of the
          // scrollport. The parent is responsible for ignoring repeat
          // fires while a page load is in flight — we just signal.
          const el = e.currentTarget;
          if (el.scrollTop + el.clientHeight >= el.scrollHeight - 100) {
            onNearBottom();
          }
        }}
      >
        {items.length === 0 ? (
          <EmptyState searching={!!searchValue.trim()} />
        ) : (
          <ul ref={listRef} className="flex flex-col">
            {items.map((m) => {
              const selected = m.id === selectedId;
              const unread = !m.seen;
              return (
                <li
                  key={m.id}
                  data-id={m.id}
                  onClick={() => onSelect(m.id)}
                  onDoubleClick={() => onActivate?.(m.id)}
                  className="relative cursor-pointer border-b px-3 py-2.5 transition-colors"
                  style={{
                    borderColor: "var(--border-soft)",
                    background: selected ? "var(--bg-selected)" : "transparent",
                  }}
                  onMouseEnter={(e) => {
                    if (!selected) e.currentTarget.style.background = "var(--bg-hover)";
                  }}
                  onMouseLeave={(e) => {
                    if (!selected) e.currentTarget.style.background = "transparent";
                  }}
                >
                  {/* Accent bar on the left for unread — the most glanceable
                      "new mail" signal across the list. */}
                  {unread && (
                    <span
                      aria-hidden
                      className="absolute inset-y-1.5 left-0 w-[3px] rounded-r"
                      style={{ background: "var(--accent)" }}
                    />
                  )}
                  <div className="flex items-start gap-2">
                    <span
                      className="mt-[7px] inline-block h-2 w-2 shrink-0 rounded-full"
                      style={{ background: m.accountColor }}
                      aria-hidden
                    />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-baseline justify-between gap-2">
                        <span
                          className="truncate text-sm"
                          style={{
                            fontWeight: unread ? 700 : 400,
                            color: unread ? "var(--fg-base)" : "var(--fg-muted)",
                          }}
                        >
                          {m.fromFirst || "—"}
                        </span>
                        <span
                          className="shrink-0 text-[11px]"
                          style={{
                            color: unread ? "var(--fg-base)" : "var(--fg-subtle)",
                            fontWeight: unread ? 600 : 400,
                          }}
                        >
                          {formatDate(m.date)}
                        </span>
                      </div>
                      <div className="flex items-center gap-1.5">
                        {m.junk && (
                          // Yellow = candidate (user pre-marked with `j`
                          // in a non-Spam folder, waiting for rule
                          // learning). Red = confirmed (mail lives in
                          // the Spam folder and carries $Junk). Tone
                          // comes from the parent since the row itself
                          // doesn't know the folder context.
                          <span
                            className="inline-flex h-4 items-center rounded px-1 text-[10px] font-semibold"
                            style={
                              junkBadgeTone === "confirmed"
                                ? {
                                    background: "rgba(239,68,68,0.15)",
                                    color: "#ef4444",
                                  }
                                : {
                                    background: "rgba(234,179,8,0.15)",
                                    color: "#ca8a04",
                                  }
                            }
                            title={
                              junkBadgeTone === "confirmed"
                                ? "Als Spam bestätigt"
                                : "Spam-Verdacht (Kandidat)"
                            }
                          >
                            SPAM
                          </span>
                        )}
                        {trainingIds?.has(m.id) && (
                          // Workflow-training candidate — user pressed
                          // `t` in the Reader. Blue to echo the
                          // confirm-mode toast styling elsewhere; same
                          // shape as the SPAM badge so the two sit
                          // side-by-side cleanly.
                          <span
                            className="inline-flex h-4 items-center rounded px-1 text-[10px] font-semibold"
                            style={{
                              background: "rgba(59,130,246,0.15)",
                              color: "#3b82f6",
                            }}
                            title={t("inbox.trainingBadgeTooltip")}
                          >
                            TRAIN
                          </span>
                        )}
                        {(m.answered || m.forwarded) && (
                          <span
                            className="text-[11px]"
                            style={{ color: "var(--fg-subtle)" }}
                            aria-hidden
                            title={
                              m.answered && m.forwarded
                                ? "Beantwortet + Weitergeleitet"
                                : m.answered
                                  ? "Beantwortet"
                                  : "Weitergeleitet"
                            }
                          >
                            {m.answered ? "↩" : ""}
                            {m.forwarded ? "↪" : ""}
                          </span>
                        )}
                        {m.hasAttachments && (
                          // Paperclip — universal "this mail has an
                          // attachment" affordance. Same muted tone as
                          // the answered/forwarded glyphs so it doesn't
                          // compete with the SPAM/TRAIN badges or the
                          // ★ flagged star. Inline-only mails (cid:
                          // images in HTML) intentionally don't trigger
                          // this — those would be noisy false positives.
                          <span
                            className="text-[11px]"
                            style={{ color: "var(--fg-subtle)" }}
                            aria-hidden
                            title={t("inbox.attachmentTooltip")}
                          >
                            📎
                          </span>
                        )}
                        {m.scheduled && (
                          // Auto-Rule-Marker. Glyph-Triple (👁/⏰/⏱) je
                          // nach Status (dry_run / überfällig / aktiv).
                          // Tooltip im `title` zeigt, welche Regel was wann
                          // tut — siehe `utils/lifetimeTag::scheduledTooltip`.
                          // Unobtrusive Tone: Subtle-Foreground wenn dry_run,
                          // Warning-Tone wenn überfällig, sonst Muted.
                          // Bewusst KEIN aria-hidden — der Tooltip-Text ist
                          // semantisch relevant (Datenverlust-Warnung).
                          <span
                            className="text-[11px] cursor-default"
                            style={{
                              color: m.scheduled.dryRun
                                ? "var(--fg-subtle)"
                                : new Date(m.scheduled.scheduledAt).getTime() <=
                                    Date.now()
                                  ? "#dc2626"
                                  : "var(--fg-muted)",
                            }}
                            title={scheduledTooltip(m.scheduled)}
                          >
                            {scheduledGlyph(m.scheduled)}
                          </span>
                        )}
                        <div
                          className="flex-1 truncate text-sm"
                          style={{
                            fontWeight: unread ? 600 : 400,
                            color: unread ? "var(--fg-base)" : "var(--fg-muted)",
                          }}
                        >
                          {m.subject || "(kein Betreff)"}
                        </div>
                      </div>
                    </div>
                    {m.flagged && (
                      <span style={{ color: "#f59e0b" }} aria-hidden>★</span>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </section>
  );
}

/**
 * Empty-state celebration. When the list is empty and the user is *not*
 * searching, we assume "inbox zero" rather than "no mail at all" and show a
 * friendly "Arbeit geschafft" screen. During an active search query, a
 * neutral "no matches" message is the right affordance — don't congratulate
 * someone for narrowing their filter to nothing.
 */
function EmptyState({ searching }: { searching: boolean }) {
  const { t } = useTranslation();

  if (searching) {
    return (
      <div
        className="px-6 py-10 text-center text-sm"
        style={{ color: "var(--fg-muted)" }}
      >
        {t("common.empty")}
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col items-center justify-center px-6 py-10 text-center">
      {/* Hand-drawn style checkmark — stroke-based so it inherits color from
          the parent `--accent`. Sized large enough to feel like a moment,
          not a tooltip. */}
      <svg
        width="84"
        height="84"
        viewBox="0 0 64 64"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.25"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
        style={{ color: "var(--accent)", opacity: 0.9 }}
      >
        <circle cx="32" cy="32" r="26" opacity="0.25" />
        <path d="M20 33 L29 42 L45 24" />
      </svg>
      <div
        className="mt-4 text-base font-semibold"
        style={{ color: "var(--fg-base)" }}
      >
        {t("empty.inboxZeroTitle")}
      </div>
      <div
        className="mt-1 max-w-[20rem] text-xs"
        style={{ color: "var(--fg-muted)" }}
      >
        {t("empty.inboxZeroSubtitle")}
      </div>
    </div>
  );
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  return sameDay
    ? d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString(undefined, { day: "2-digit", month: "short" });
}
