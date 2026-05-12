import { useEffect, useState } from "react";

/**
 * Re-renders the caller on a steady tick so a countdown / "now"-relative
 * display can update without each consumer wiring its own setInterval.
 *
 * Default cadence is 1 s — fine for human-perceptible UI like "noch 3 Min".
 * Pass a larger value (e.g. 30_000) for cheaper periodic refreshes.
 *
 * The hook itself is allocation-light: a single `Date` per tick, dropped on
 * the next tick. Consumers should still keep their reactive work inside a
 * `useMemo` so the per-second re-render doesn't cascade into expensive
 * downstream computation.
 */
export function useNow(intervalMs: number = 1000): Date {
  const [now, setNow] = useState<Date>(() => new Date());
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), intervalMs);
    return () => window.clearInterval(id);
  }, [intervalMs]);
  return now;
}
