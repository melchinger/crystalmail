/**
 * Client-side sort order for accounts.
 *
 * Stored as a plain `string[]` of account IDs in localStorage. On load
 * we read this array; new accounts (not in the stored list) are appended
 * at the end in their original server-side order. Disappeared IDs are
 * dropped when the list is next saved.
 *
 * No backend migration — this is purely a UI preference. Every site
 * that renders accounts runs through `sortAccounts()`; the
 * AccountSettings panel persists changes via Up/Down buttons.
 */

import type { AccountSummary } from "../types";

const STORAGE_KEY = "crystalmail:account-order";

export function loadOrder(): string[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((x): x is string => typeof x === "string");
  } catch {
    return [];
  }
}

export function saveOrder(ids: string[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(ids));
  } catch {
    // localStorage may be disabled / quota exceeded — sort reverts
    // to server-side order next load. Acceptable fallback.
  }
}

/**
 * Apply the stored preference to a fresh account list. Accounts not in
 * the stored order fall to the end in their input-relative position —
 * new accounts you just added don't randomly jump to the front.
 */
export function sortAccounts(accounts: AccountSummary[]): AccountSummary[] {
  if (accounts.length <= 1) return accounts;
  const order = loadOrder();
  if (order.length === 0) return accounts;

  const idToRank = new Map<string, number>();
  order.forEach((id, i) => idToRank.set(id, i));

  // Stable sort: use the input index as the tiebreaker for unknown IDs.
  return accounts
    .map((a, i) => ({ a, i }))
    .sort((x, y) => {
      const rx = idToRank.get(x.a.id);
      const ry = idToRank.get(y.a.id);
      if (rx !== undefined && ry !== undefined) return rx - ry;
      if (rx !== undefined) return -1;
      if (ry !== undefined) return 1;
      return x.i - y.i;
    })
    .map(({ a }) => a);
}

/**
 * Move one entry up or down in the order and persist. Takes the current
 * sorted list so the caller doesn't have to re-derive it. Returns the
 * new list for immediate state update.
 */
export function moveAccount(
  sorted: AccountSummary[],
  accountId: string,
  direction: "up" | "down",
): AccountSummary[] {
  const idx = sorted.findIndex((a) => a.id === accountId);
  if (idx < 0) return sorted;
  const target = direction === "up" ? idx - 1 : idx + 1;
  if (target < 0 || target >= sorted.length) return sorted;

  const next = sorted.slice();
  [next[idx], next[target]] = [next[target], next[idx]];
  saveOrder(next.map((a) => a.id));
  return next;
}
