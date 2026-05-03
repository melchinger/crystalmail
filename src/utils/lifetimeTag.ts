// Helpers für ScheduledAction-Tags: Tooltip-Text + Restzeit-Glyph.
// Datei-Name (lifetimeTag.ts) ist historisch — der Inhalt wurde mit
// dem v2-Refactor auf das unifizierte Workflow-Rule-Scheduling
// umgebogen. Liest aus `EnvelopeSummary.scheduled` (vorher `lifetime`).

import type { RuleAction, ScheduledActionTag } from "../types";

function actionVerb(action: RuleAction, dest: string | null): string {
  switch (action) {
    case "archive":
      return "wandert ins Archiv";
    case "delete":
      return "wandert in den Papierkorb";
    case "move":
      return dest ? `wird nach „${dest}" verschoben` : "wird verschoben";
    case "run_workflow":
      return "Workflow läuft";
  }
}

function humanRemaining(scheduledAtIso: string, now: Date = new Date()): string {
  const scheduledAt = new Date(scheduledAtIso);
  const diffMs = scheduledAt.getTime() - now.getTime();
  if (diffMs <= 0) {
    return "jetzt fällig";
  }
  const diffMin = Math.round(diffMs / 60_000);
  if (diffMin < 60) {
    return `in ${diffMin} min`;
  }
  const diffHrs = Math.round(diffMin / 60);
  if (diffHrs < 24) {
    return `in ${diffHrs} h`;
  }
  const diffDays = Math.round(diffHrs / 24);
  return diffDays === 1 ? "morgen" : `in ${diffDays} Tagen`;
}

/** Voller Tooltip-Text. Title-Attribut am Marker. */
export function scheduledTooltip(
  tag: ScheduledActionTag,
  now: Date = new Date(),
): string {
  const remaining = humanRemaining(tag.scheduledAt, now);
  const verb = actionVerb(tag.action, tag.actionDest);
  const ruleLabel = tag.ruleName ? `Regel „${tag.ruleName}"` : "Auto-Regel";
  const dryHint = tag.dryRun ? "  (Trockenmodus — wird nicht ausgeführt)" : "";
  return `${ruleLabel}: ${verb} ${remaining}${dryHint}`;
}

/** Glyph für den Marker, dreiteilig:
 *  - dry_run     → 👁  ("ich beobachte nur")
 *  - überfällig  → ⏰ ("Sweeper kommt jetzt gleich")
 *  - sonst       → ⏱  ("Standard-geplante Action") */
export function scheduledGlyph(
  tag: ScheduledActionTag,
  now: Date = new Date(),
): string {
  if (tag.dryRun) return "👁";
  const scheduledAt = new Date(tag.scheduledAt);
  if (scheduledAt.getTime() <= now.getTime()) return "⏰";
  return "⏱";
}
