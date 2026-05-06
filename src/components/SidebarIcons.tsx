// Shared stroke-icon set for the Sidebar. Hand-picked geometry in the
// same Lucide-influenced DNA as the Reader toolbar icons: 24×24 grid,
// 1.5 px stroke, rounded caps, `currentColor` so the sidebar's
// "selected → accent" re-coloring just works.
//
// Kept in its own file (not inline in Sidebar.tsx) so the eventual
// Reader-icon unification has a natural target — when we unify we
// migrate Reader's private icons into this module.

import type { SVGProps } from "react";

/** Shared wrapper — keeps stroke params uniform across the set. */
function Svg({
  children,
  size = 18,
  ...rest
}: SVGProps<SVGSVGElement> & { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      {...rest}
    >
      {children}
    </svg>
  );
}

/** Inbox tray — universal "incoming mail" symbol. Reads at small sizes. */
export const IconInbox = (p: { size?: number }) => (
  <Svg size={p.size}>
    <polyline points="22 12 16 12 14 15 10 15 8 12 2 12" />
    <path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" />
  </Svg>
);

/** Storage box with a lid and a dash — classic archive glyph. */
export const IconArchive = (p: { size?: number }) => (
  <Svg size={p.size}>
    <rect x="2" y="3" width="20" height="5" rx="1" />
    <path d="M4 8v12a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V8" />
    <line x1="10" y1="13" x2="14" y2="13" />
  </Svg>
);

/** Document with a pencil overlay — implies "in progress". */
export const IconDrafts = (p: { size?: number }) => (
  <Svg size={p.size}>
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
    <polyline points="14 2 14 8 20 8" />
    <path d="m15.5 15.5-1.2 1.2a.8.8 0 0 1-.7.2l-1.7-.3.3-1.7a.8.8 0 0 1 .2-.7l3.5-3.5a1 1 0 0 1 1.4 1.4z" />
  </Svg>
);

/** Paper plane — outgoing. Lucide "Send". */
export const IconSent = (p: { size?: number }) => (
  <Svg size={p.size}>
    <line x1="22" y1="2" x2="11" y2="13" />
    <polygon points="22 2 15 22 11 13 2 9 22 2" />
  </Svg>
);

/** Five-point star — "markiert / gestarred". Filled variant lets the
 *  Sidebar selection rendering show an emphasized state if needed. */
export const IconStarred = (p: { size?: number }) => (
  <Svg size={p.size}>
    <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
  </Svg>
);

/** Shield with exclamation — matches the Reader's spam glyph. */
export const IconSpam = (p: { size?: number }) => (
  <Svg size={p.size}>
    <path d="M12 3l7 3v6c0 4-3 7.5-7 9-4-1.5-7-5-7-9V6z" />
    <line x1="12" y1="9" x2="12" y2="13" />
    <circle cx="12" cy="16" r="0.6" fill="currentColor" stroke="none" />
  </Svg>
);

/** Wastebasket with handle — matches the Reader's trash glyph. */
export const IconTrash = (p: { size?: number }) => (
  <Svg size={p.size}>
    <polyline points="3 6 5 6 21 6" />
    <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
    <path d="M10 11v6" />
    <path d="M14 11v6" />
    <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
  </Svg>
);

// ─── Header-row buttons ──────────────────────────────────────────────

/** Pen-square — "compose new". Lucide "SquarePen". */
export const IconCompose = (p: { size?: number }) => (
  <Svg size={p.size}>
    <path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
    <path d="M18.375 2.625a2.121 2.121 0 1 1 3 3L13 14l-4 1 1-4 8.375-8.375Z" />
  </Svg>
);

/**
 * Circular arrow pair — "sync". `spinning` callers wrap the element in
 * a rotating span; the SVG itself stays still. Geometry borrowed from
 * Lucide "RefreshCw".
 */
export const IconSync = (p: { size?: number }) => (
  <Svg size={p.size}>
    <path d="M21 12a9 9 0 0 0-15-6.7L3 8" />
    <path d="M3 4v4h4" />
    <path d="M3 12a9 9 0 0 0 15 6.7l3-2.7" />
    <path d="M21 20v-4h-4" />
  </Svg>
);

/**
 * Proper gear — settings. The Lucide "Settings" glyph: a circular body
 * with eight abgesetzte teeth around the rim plus a hub. Reads as a
 * real zahnrad at 16 px where the simplified 8-tick variant we had
 * before looked more like a compass rose.
 */
/** Person-Silhouette für den "Kontakte"-Eintrag. Zwei-Element-
 *  Aufbau: Kreis für den Kopf, abgerundete Bogen für die Schultern.
 *  Hält bei 16px noch sauber, wird bei großem Zoom nicht weich. */
export const IconContacts = (p: { size?: number }) => (
  <Svg size={p.size}>
    <circle cx="12" cy="8" r="4" />
    <path d="M4 21v-1a8 8 0 0 1 16 0v1" />
  </Svg>
);

export const IconCalendar = (p: { size?: number }) => (
  <Svg size={p.size}>
    {/* Calendar body + tear-off ring at the top, pre-styled to match
        the other icons' stroke-only aesthetic. */}
    <rect x="3" y="5" width="18" height="16" rx="2" />
    <line x1="3" y1="10" x2="21" y2="10" />
    <line x1="8" y1="3" x2="8" y2="7" />
    <line x1="16" y1="3" x2="16" y2="7" />
  </Svg>
);

export const IconSettings = (p: { size?: number }) => (
  <Svg size={p.size}>
    <path d="M19.14 12.94a7.07 7.07 0 0 0 .06-.94 7.07 7.07 0 0 0-.06-.94l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.61-.22l-2.39.96a7 7 0 0 0-1.62-.94l-.36-2.54a.5.5 0 0 0-.5-.42h-3.84a.5.5 0 0 0-.5.42l-.36 2.54c-.59.24-1.13.56-1.62.94l-2.39-.96a.5.5 0 0 0-.61.22L2.74 8.84a.5.5 0 0 0 .12.64l2.03 1.58c-.04.31-.06.62-.06.94s.02.63.06.94l-2.03 1.58a.5.5 0 0 0-.12.64l1.92 3.32c.14.24.43.33.67.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.25.42.5.42h3.84a.5.5 0 0 0 .5-.42l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.24.11.53.02.67-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58Z" />
    <circle cx="12" cy="12" r="3" />
  </Svg>
);
