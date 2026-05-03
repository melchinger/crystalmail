// Search DSL parser. The user types a query like
//   from:alex has:attachments since:2025-01-01 rechnung
// and we split it into:
//   * an FTS5 free-text part (`from_text:alex rechnung`)
//   * structured filters (`hasAttachments=true`, `since=2025-01-01`)
//   * an optional folder override (`in:archive` → "archive")
// The backend's `search_advanced` command then ANDs the FTS5 query and
// the filter struct against `envelopes` + `fts_envelopes`.
//
// Parsing rules (deliberately permissive — fail soft, never throw):
//
//   key:value           — recognised operators below; unknown keys fall
//                         through to the FTS5 part as-is
//   key:"phrase value"  — quoted value, supports spaces
//   "phrase"            — bare phrase, passed to FTS5 verbatim
//   -word               — FTS5 negation, passed through
//   bare-word           — FTS5 free text
//   today | yesterday | last week | this week | this month | this year
//                       — relative time phrases, parsed without prefix
//
// Recognised operators:
//   from:    → FTS5 column `from_text:` (re-aliased)
//   to:      → FTS5 column `to_text:`
//   subject: → FTS5 column `subject:`
//   body:    → FTS5 column `body_text:`
//   in:      → folder override (inbox/archive/sent/drafts/trash/spam/starred)
//   is:      → flag filter (read/unread/flagged/answered/spam/junk)
//   has:     → has:attachments
//   since:   → date filter, lower bound (RFC3339)
//   before:  → date filter, upper bound (RFC3339)
//   on:      → date filter, exact day (since=midnight, before=midnight+1d)
//   after:   → alias for since:
//
// Unknown operators or malformed values are recorded in `errors` and
// left in the FTS5 text — the user still sees their query produce
// *something*, with a small inline note about what went wrong.

export type SearchFilters = {
  seen?: boolean;
  flagged?: boolean;
  answered?: boolean;
  junk?: boolean;
  hasAttachments?: boolean;
  /** RFC3339 lower bound, inclusive. */
  since?: string;
  /** RFC3339 upper bound, exclusive. */
  before?: string;
};

export type ParsedQuery = {
  /** FTS5-compatible query string. Empty string ⇒ no FTS narrowing
   *  (filter-only search). */
  fts: string;
  /** Canonical folder key when the user typed `in:foo`. Overrides the
   *  current folder selection. `null` when no override. */
  folderOverride: string | null;
  filters: SearchFilters;
  /** Human-readable validation notes. Empty ⇒ query parsed cleanly. */
  errors: string[];
};

const FOLDER_ALIASES: Record<string, string> = {
  inbox: "inbox",
  posteingang: "inbox",
  archive: "archive",
  archiv: "archive",
  sent: "sent",
  gesendet: "sent",
  drafts: "drafts",
  entwurf: "drafts",
  entwürfe: "drafts",
  entwuerfe: "drafts",
  trash: "trash",
  papierkorb: "trash",
  spam: "spam",
  junk: "spam",
  starred: "starred",
  markiert: "starred",
};

// ─── tokeniser ───────────────────────────────────────────────────────
// Splits on whitespace but respects double-quoted spans. Returns one
// token per logical unit; the parser then decides how to handle each.

function tokenise(input: string): string[] {
  const tokens: string[] = [];
  let i = 0;
  const n = input.length;
  while (i < n) {
    const c = input[i];
    if (c === " " || c === "\t" || c === "\n") {
      i += 1;
      continue;
    }
    // Read until the next unquoted whitespace, but treat `"…"`
    // as one continuous span (including a leading `key:`).
    let start = i;
    let inQuote = false;
    while (i < n) {
      const ch = input[i];
      if (ch === '"') {
        inQuote = !inQuote;
        i += 1;
        continue;
      }
      if (!inQuote && (ch === " " || ch === "\t" || ch === "\n")) break;
      i += 1;
    }
    tokens.push(input.slice(start, i));
  }
  return tokens;
}

/** Strip a single layer of double quotes from a string.  Used after
 *  splitting a `key:"value with spaces"` token. */
function unquote(s: string): string {
  if (s.length >= 2 && s.startsWith('"') && s.endsWith('"')) {
    return s.slice(1, -1);
  }
  return s;
}

// ─── relative-time helpers ───────────────────────────────────────────

/**
 * Translate phrases like "last week" into a `{ since, before }` window.
 * `null` if the phrase isn't recognised. Boundaries are aligned to
 * local-midnight so users get the day they expect, then converted to
 * UTC ISO strings for the SQLite predicate.
 */
function parseRelative(
  phrase: string,
): { since?: string; before?: string } | null {
  const p = phrase.trim().toLowerCase();
  const now = new Date();
  const startOfToday = new Date(
    now.getFullYear(),
    now.getMonth(),
    now.getDate(),
  );
  const startOfTomorrow = new Date(startOfToday);
  startOfTomorrow.setDate(startOfTomorrow.getDate() + 1);

  switch (p) {
    case "today":
    case "heute":
      return {
        since: startOfToday.toISOString(),
        before: startOfTomorrow.toISOString(),
      };
    case "yesterday":
    case "gestern": {
      const startYesterday = new Date(startOfToday);
      startYesterday.setDate(startYesterday.getDate() - 1);
      return {
        since: startYesterday.toISOString(),
        before: startOfToday.toISOString(),
      };
    }
    case "this week":
    case "diese woche": {
      const day = startOfToday.getDay(); // 0 = Sun
      const monday = new Date(startOfToday);
      monday.setDate(monday.getDate() - ((day + 6) % 7));
      return {
        since: monday.toISOString(),
        before: startOfTomorrow.toISOString(),
      };
    }
    case "last week":
    case "letzte woche": {
      const day = startOfToday.getDay();
      const monday = new Date(startOfToday);
      monday.setDate(monday.getDate() - ((day + 6) % 7));
      const prevMonday = new Date(monday);
      prevMonday.setDate(prevMonday.getDate() - 7);
      return { since: prevMonday.toISOString(), before: monday.toISOString() };
    }
    case "this month":
    case "dieser monat": {
      const first = new Date(now.getFullYear(), now.getMonth(), 1);
      return {
        since: first.toISOString(),
        before: startOfTomorrow.toISOString(),
      };
    }
    case "last month":
    case "letzter monat": {
      const first = new Date(now.getFullYear(), now.getMonth() - 1, 1);
      const firstThis = new Date(now.getFullYear(), now.getMonth(), 1);
      return { since: first.toISOString(), before: firstThis.toISOString() };
    }
    case "this year":
    case "dieses jahr": {
      const first = new Date(now.getFullYear(), 0, 1);
      return {
        since: first.toISOString(),
        before: startOfTomorrow.toISOString(),
      };
    }
    default:
      return null;
  }
}

// ─── absolute-date parser ────────────────────────────────────────────

const MONTH_NAMES: Record<string, number> = {
  jan: 0,
  january: 0,
  januar: 0,
  feb: 1,
  february: 1,
  februar: 1,
  mar: 2,
  march: 2,
  märz: 2,
  maerz: 2,
  apr: 3,
  april: 3,
  may: 4,
  mai: 4,
  jun: 5,
  june: 5,
  juni: 5,
  jul: 6,
  july: 6,
  juli: 6,
  aug: 7,
  august: 7,
  sep: 8,
  sept: 8,
  september: 8,
  oct: 9,
  october: 9,
  okt: 9,
  oktober: 9,
  nov: 10,
  november: 10,
  dec: 11,
  december: 11,
  dez: 11,
  dezember: 11,
};

/**
 * Best-effort date parser for `since:`/`before:`/`on:` values.
 * Accepts:
 *   YYYY-MM-DD
 *   DD.MM.YYYY      (German)
 *   "14 june 2022"  (Spark-style, English or German month)
 *   "june 14 2022"
 *   2022            (just a year — start of year)
 * Returns midnight-local converted to UTC ISO string, or null if
 * unparseable.
 */
function parseAbsoluteDate(value: string): Date | null {
  const v = value.trim().toLowerCase().replace(/,/g, "");
  if (!v) return null;

  // ISO-ish: YYYY-MM-DD
  const iso = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(v);
  if (iso) {
    const d = new Date(
      Number(iso[1]),
      Number(iso[2]) - 1,
      Number(iso[3]),
    );
    return Number.isFinite(d.getTime()) ? d : null;
  }

  // German: DD.MM.YYYY
  const de = /^(\d{1,2})\.(\d{1,2})\.(\d{2,4})$/.exec(v);
  if (de) {
    const year = de[3].length === 2 ? 2000 + Number(de[3]) : Number(de[3]);
    const d = new Date(year, Number(de[2]) - 1, Number(de[1]));
    return Number.isFinite(d.getTime()) ? d : null;
  }

  // Year only
  if (/^\d{4}$/.test(v)) return new Date(Number(v), 0, 1);

  // Word-based: split on spaces and hunt for a month name + day + year.
  const parts = v.split(/\s+/).filter(Boolean);
  let day: number | null = null;
  let month: number | null = null;
  let year: number | null = null;
  for (const part of parts) {
    if (month === null && MONTH_NAMES[part] !== undefined) {
      month = MONTH_NAMES[part];
      continue;
    }
    if (/^\d{1,2}$/.test(part)) {
      const n = Number(part);
      // Larger 2-digit numbers we treat as year (24 → 2024 etc.) only
      // when day already exists.
      if (day === null) day = n;
      else if (year === null) year = n >= 70 ? 1900 + n : 2000 + n;
      continue;
    }
    if (/^\d{4}$/.test(part)) {
      year = Number(part);
      continue;
    }
  }
  if (month !== null && day !== null) {
    const y = year ?? new Date().getFullYear();
    const d = new Date(y, month, day);
    return Number.isFinite(d.getTime()) ? d : null;
  }
  return null;
}

// ─── operator handlers ──────────────────────────────────────────────

const FTS_FIELD_ALIAS: Record<string, string> = {
  from: "from_text",
  to: "to_text",
  subject: "subject",
  body: "body_text",
  body_text: "body_text",
  from_text: "from_text",
  to_text: "to_text",
};

function handleIs(value: string, filters: SearchFilters, errors: string[]) {
  const v = value.trim().toLowerCase();
  switch (v) {
    case "unread":
    case "ungelesen":
      filters.seen = false;
      return;
    case "read":
    case "gelesen":
      filters.seen = true;
      return;
    case "flagged":
    case "starred":
    case "markiert":
      filters.flagged = true;
      return;
    case "answered":
    case "beantwortet":
      filters.answered = true;
      return;
    case "spam":
    case "junk":
      filters.junk = true;
      return;
    default:
      errors.push(`is:${value} — unbekannt`);
  }
}

function handleHas(value: string, filters: SearchFilters, errors: string[]) {
  const v = value.trim().toLowerCase();
  if (v === "attachment" || v === "attachments" || v === "anhang" || v === "anhänge") {
    filters.hasAttachments = true;
    return;
  }
  errors.push(`has:${value} — unbekannt (versuch: has:attachments)`);
}

function handleIn(
  value: string,
  out: { folder: string | null },
  errors: string[],
) {
  const v = value.trim().toLowerCase();
  const canonical = FOLDER_ALIASES[v];
  if (canonical) {
    out.folder = canonical;
    return;
  }
  errors.push(`in:${value} — unbekannter Ordner`);
}

function handleDate(
  key: "since" | "before" | "on" | "after",
  value: string,
  filters: SearchFilters,
  errors: string[],
) {
  const date = parseAbsoluteDate(value);
  if (!date) {
    errors.push(`${key}:${value} — Datum nicht erkannt`);
    return;
  }
  const next = new Date(date);
  next.setDate(next.getDate() + 1);
  switch (key) {
    case "since":
    case "after":
      filters.since = date.toISOString();
      return;
    case "before":
      filters.before = date.toISOString();
      return;
    case "on":
      filters.since = date.toISOString();
      filters.before = next.toISOString();
      return;
  }
}

// ─── main entry ──────────────────────────────────────────────────────

export function parseSearchQuery(input: string): ParsedQuery {
  const filters: SearchFilters = {};
  const errors: string[] = [];
  const folderState = { folder: null as string | null };
  const ftsParts: string[] = [];

  // Pre-scan for multi-word relative phrases — they aren't `key:value`
  // tokens so the per-token loop wouldn't catch "last week" as one
  // unit. Order matters: longer phrases first, so "this week" wins
  // over a bare "this".
  const PHRASES: Array<{ phrase: string; tokens: string[] }> = [
    "last month",
    "letzter monat",
    "this month",
    "dieser monat",
    "this year",
    "dieses jahr",
    "last week",
    "letzte woche",
    "this week",
    "diese woche",
    "today",
    "heute",
    "yesterday",
    "gestern",
  ].map((p) => ({ phrase: p, tokens: p.split(/\s+/) }));

  // Working copy of input — we strip recognised relative phrases out
  // before tokenising so they don't end up in the FTS5 part.
  let working = input;
  for (const { phrase } of PHRASES) {
    const re = new RegExp(`(^|\\s)${escapeRegExp(phrase)}(?=$|\\s)`, "i");
    if (re.test(working)) {
      const range = parseRelative(phrase);
      if (range) {
        if (range.since) filters.since = range.since;
        if (range.before) filters.before = range.before;
        working = working.replace(re, " ");
      }
    }
  }

  for (const tok of tokenise(working)) {
    // `key:value` token. The colon must come before any quote so that
    // `subject:"foo bar"` parses cleanly.
    const colonIdx = firstUnquotedColon(tok);
    if (colonIdx > 0) {
      const key = tok.slice(0, colonIdx).toLowerCase();
      const value = unquote(tok.slice(colonIdx + 1));

      switch (key) {
        case "is":
          handleIs(value, filters, errors);
          continue;
        case "has":
          handleHas(value, filters, errors);
          continue;
        case "in":
          handleIn(value, folderState, errors);
          continue;
        case "since":
        case "after":
        case "before":
        case "on":
          handleDate(key, value, filters, errors);
          continue;
      }

      // FTS5 column alias — rewrite `from:` → `from_text:`. FTS5
      // accepts the column-prefix syntax natively, including with
      // quoted phrases.
      const ftsCol = FTS_FIELD_ALIAS[key];
      if (ftsCol) {
        ftsParts.push(`${ftsCol}:${quoteForFts(value)}`);
        continue;
      }

      // Unknown operator: pass the whole thing to FTS5 as plain text
      // so the user gets *something*, but flag it.
      errors.push(`${key}: — unbekannter Operator`);
      ftsParts.push(quoteForFts(tok));
      continue;
    }

    // Plain word / quoted phrase / negation — pass through. FTS5
    // handles `"phrase"` and `-word` natively, so no extra escaping.
    ftsParts.push(tok);
  }

  return {
    fts: ftsParts.join(" ").trim(),
    folderOverride: folderState.folder,
    filters,
    errors,
  };
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Find the first colon outside of a `"…"` span. Returns -1 if none. */
function firstUnquotedColon(s: string): number {
  let inQuote = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === '"') {
      inQuote = !inQuote;
      continue;
    }
    if (!inQuote && c === ":") return i;
  }
  return -1;
}

/** Wrap a value in double quotes for FTS5 if it contains anything
 *  FTS5 might interpret (whitespace, special chars). Already-quoted
 *  values pass through. */
function quoteForFts(s: string): string {
  if (!s) return "";
  if (s.startsWith('"') && s.endsWith('"')) return s;
  if (/[\s"():*-]/.test(s)) return `"${s.replace(/"/g, "")}"`;
  return s;
}
