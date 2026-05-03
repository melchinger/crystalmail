import { useLayoutEffect, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

type Props = {
  /** Anchor element (the search input). Drives the panel's position
   *  via `getBoundingClientRect()` — the panel itself is `position:
   *  fixed`, so it can extend past the narrow InboxList column and
   *  use the full app width. */
  anchorRef: RefObject<HTMLElement | null>;
  /** Most-recent-first list of past search strings. Empty array hides
   *  the entire "Kürzliche Suchen" block. */
  recents: string[];
  /** Click handler for a recent-search chip — caller wires this up to
   *  set the input value AND fire the search immediately. */
  onPickRecent: (query: string) => void;
  /** × button on a recent chip — caller removes the entry from the
   *  history (typically via `removeRecentSearch`). */
  onRemoveRecent: (query: string) => void;
  /** Click on a tip → caller writes the example into the input but
   *  does *not* fire the search yet. The user is expected to tweak
   *  `from:alex` to `from:erika` before pressing Enter. */
  onPickTip: (example: string) => void;
};

/**
 * Empty-state panel anchored just below the search input. Floats with
 * `position: fixed` so it can break out of the narrow InboxList column
 * and span a comfortable width across the app — Spark's layout falls
 * apart at 360px (which is what the InboxList column gives us); we
 * extend right toward the Reader pane instead.
 *
 * The panel re-positions on window resize so dragging the Tauri
 * window or hitting fullscreen keeps it aligned with the input.
 */
export function SearchPanel({
  anchorRef,
  recents,
  onPickRecent,
  onRemoveRecent,
  onPickTip,
}: Props) {
  const { t } = useTranslation();
  // Computed `position: fixed` rect, lazily — undefined until the
  // anchor has rendered and we've measured it. Width is *also*
  // computed (clamped between a min/max) so the grid is always
  // legible regardless of viewport size.
  const [pos, setPos] = useState<
    { left: number; top: number; width: number } | undefined
  >(undefined);

  useLayoutEffect(() => {
    function update() {
      const rect = anchorRef.current?.getBoundingClientRect();
      if (!rect) return;
      const RIGHT_MARGIN = 16; // breathing room from the window edge
      const MAX_WIDTH = 960; // enough for 4 generous columns; capped so
      //                       it doesn't stretch absurdly on 4K screens
      const MIN_WIDTH = Math.min(360, window.innerWidth - 32);
      const left = rect.left;
      const top = rect.bottom + 4;
      const available = window.innerWidth - left - RIGHT_MARGIN;
      const width = Math.max(
        MIN_WIDTH,
        Math.min(MAX_WIDTH, available),
      );
      setPos({ left, top, width });
    }
    update();
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, [anchorRef]);

  if (!pos) return null;

  return (
    <div
      className="overflow-hidden rounded-md border shadow-xl"
      style={{
        position: "fixed",
        left: pos.left,
        top: pos.top,
        width: pos.width,
        zIndex: 40,
        background: "var(--bg-panel)",
        borderColor: "var(--border-base)",
        color: "var(--fg-base)",
      }}
      // Suppress mousedown so clicking inside the panel doesn't blur
      // the input — that would close the panel before the click
      // reached the chip / tip handler. Pointer-up still fires.
      onMouseDown={(e) => e.preventDefault()}
    >
      {recents.length > 0 && (
        <section
          className="border-b px-4 pb-2.5 pt-3"
          style={{ borderColor: "var(--border-soft)" }}
        >
          <h3
            className="mb-1.5 text-[11px] font-semibold uppercase tracking-wide"
            style={{ color: "var(--fg-subtle)" }}
          >
            {t("search.recent")}
          </h3>
          <div className="flex flex-wrap gap-1.5">
            {recents.map((q) => (
              <RecentChip
                key={q}
                query={q}
                onPick={() => onPickRecent(q)}
                onRemove={() => onRemoveRecent(q)}
              />
            ))}
          </div>
        </section>
      )}

      <section className="px-4 pb-3 pt-3">
        <h3
          className="mb-2 text-[11px] font-semibold uppercase tracking-wide"
          style={{ color: "var(--fg-subtle)" }}
        >
          {t("search.tips")}
        </h3>
        {/*
          The tips render as four columns of stacked rows — one column
          per category (Personen / Inhalt / Ordner / Zeit). At narrower
          widths we collapse to two cols. Each tip is its own button so
          the click target stays predictable, and `whitespace-nowrap`
          on the example token keeps `since:2025-01-01` from breaking
          mid-string the way the screenshot showed.
        */}
        <div className="grid gap-x-6 gap-y-1 md:grid-cols-2 lg:grid-cols-4">
          {TIPS.map((group) => (
            <div key={group.glyph} className="flex flex-col gap-0.5">
              {group.items.map((tip) => (
                <TipRow
                  key={tip.example}
                  glyph={group.glyph}
                  example={tip.example}
                  labelKey={tip.labelKey}
                  onPick={() => onPickTip(tip.example)}
                />
              ))}
            </div>
          ))}
        </div>
        <p
          className="mt-3 border-t pt-2 text-[11px]"
          style={{
            color: "var(--fg-subtle)",
            borderColor: "var(--border-soft)",
          }}
        >
          {t("search.combineHint")}
        </p>
      </section>
    </div>
  );
}

function RecentChip({
  query,
  onPick,
  onRemove,
}: {
  query: string;
  onPick: () => void;
  onRemove: () => void;
}) {
  return (
    <span
      className="inline-flex items-center gap-1 rounded-full border px-2.5 py-1 text-xs"
      style={{
        background: "var(--bg-base)",
        borderColor: "var(--border-soft)",
      }}
    >
      <button
        type="button"
        onClick={onPick}
        className="flex items-center gap-1.5 outline-none"
        style={{ color: "var(--fg-base)" }}
      >
        <span aria-hidden style={{ color: "var(--accent)" }}>
          ⌕
        </span>
        <span className="max-w-[20rem] truncate">{query}</span>
      </button>
      <button
        type="button"
        onClick={onRemove}
        aria-label="Aus Verlauf entfernen"
        className="rounded px-1 text-[11px] leading-none outline-none"
        style={{ color: "var(--fg-subtle)" }}
      >
        ✕
      </button>
    </span>
  );
}

function TipRow({
  glyph,
  example,
  labelKey,
  onPick,
}: {
  glyph: string;
  example: string;
  labelKey: string;
  onPick: () => void;
}) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={onPick}
      className="flex items-baseline gap-2 rounded px-1.5 py-1 text-left transition-colors"
      style={{ color: "var(--fg-base)" }}
      onMouseEnter={(e) => {
        e.currentTarget.style.background = "var(--bg-hover)";
      }}
      onMouseLeave={(e) => {
        e.currentTarget.style.background = "transparent";
      }}
    >
      <span
        aria-hidden
        className="shrink-0 text-[11px]"
        style={{ color: "var(--accent)" }}
      >
        {glyph}
      </span>
      <span className="flex min-w-0 flex-1 items-baseline gap-2">
        <code
          className="whitespace-nowrap font-mono text-[12px]"
          style={{ color: "var(--fg-base)" }}
        >
          {example}
        </code>
        <span
          className="truncate text-[11px]"
          style={{ color: "var(--fg-subtle)" }}
        >
          {t(labelKey)}
        </span>
      </span>
    </button>
  );
}

/**
 * Tips grid content. Keep operators and example values in sync with
 * `utils/searchDsl.ts` — only what the parser actually understands
 * belongs here, otherwise we promise UX that doesn't fire.
 */
const TIPS: Array<{
  glyph: string;
  items: Array<{ example: string; labelKey: string }>;
}> = [
  {
    glyph: "👤",
    items: [
      { example: "from:alex", labelKey: "search.tipFrom" },
      { example: "to:max", labelKey: "search.tipTo" },
      { example: "cc:john", labelKey: "search.tipCc" },
      { example: '"projekt rechnung"', labelKey: "search.tipPhrase" },
    ],
  },
  {
    glyph: "✎",
    items: [
      { example: "subject:asap", labelKey: "search.tipSubject" },
      { example: "body:vertrag", labelKey: "search.tipBody" },
      { example: "has:attachments", labelKey: "search.tipAttachments" },
      { example: "-newsletter", labelKey: "search.tipNegate" },
    ],
  },
  {
    glyph: "📁",
    items: [
      { example: "in:inbox", labelKey: "search.tipInInbox" },
      { example: "in:archive", labelKey: "search.tipInArchive" },
      { example: "in:sent", labelKey: "search.tipInSent" },
      { example: "is:unread", labelKey: "search.tipIsUnread" },
    ],
  },
  {
    glyph: "📅",
    items: [
      { example: "since:2025-01-01", labelKey: "search.tipSince" },
      { example: "before:2025-12-31", labelKey: "search.tipBefore" },
      { example: "last week", labelKey: "search.tipLastWeek" },
      { example: "is:flagged", labelKey: "search.tipIsFlagged" },
    ],
  },
];
