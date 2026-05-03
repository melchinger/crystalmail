// Recent-search history. Lives in localStorage so it survives reloads
// without round-tripping through the Rust DB — the value is purely a
// UX nicety, not data we'd ever sync or back up.
//
// Stored as a single JSON array of raw query strings (verbatim user
// input, including typos and trailing whitespace — clicking a chip
// re-runs *exactly* what the user typed last time). MRU-ordered:
// position 0 is the most recent.

const STORAGE_KEY = "cm:recentSearches";
const MAX_ENTRIES = 7; // matches the ~7 chips Spark shows

export function loadRecentSearches(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((s): s is string => typeof s === "string");
  } catch {
    return [];
  }
}

/** Push `query` to the front, dedupe, cap at MAX_ENTRIES. Empty / pure-
 *  whitespace queries are ignored — re-running an empty search would
 *  just replay "show all", which is the no-search default anyway. */
export function pushRecentSearch(query: string): string[] {
  const trimmed = query.trim();
  if (!trimmed) return loadRecentSearches();
  const current = loadRecentSearches();
  // Case-sensitive de-dupe so "Rechnung" and "rechnung" stay separate
  // (FTS5 is case-insensitive at match time but the chip is verbatim
  // user input — re-running both makes sense if the user typed both).
  const next = [trimmed, ...current.filter((s) => s !== trimmed)].slice(
    0,
    MAX_ENTRIES,
  );
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage quota / private mode — silent no-op. The list still
    // works for the rest of this session via the in-memory cache the
    // caller holds.
  }
  return next;
}

export function removeRecentSearch(query: string): string[] {
  const next = loadRecentSearches().filter((s) => s !== query);
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // see pushRecentSearch for rationale
  }
  return next;
}

export function clearRecentSearches(): void {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // see pushRecentSearch for rationale
  }
}
