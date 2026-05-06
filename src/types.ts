// Shared types mirroring the Rust-side serde payloads (camelCase).

/** pi RPC configuration — mirrors state::PiConfig on the Rust side. */
export type PiConfig = {
  binPath: string;
  provider: string;
  model: string;
  sessionDir: string;
  sessionFile: string;
  tools: string;
  thinking: string;
  extraArgs: string[];
  showThinking: boolean;
  promptPrefix: string;
  /** Override provider for spam analysis only. Empty = reuse `provider`. */
  spamProvider: string;
  /** Override model for spam analysis only. Empty = reuse `model`. */
  spamModel: string;
  /**
   * Master AI kill-switch. When `false`, every backend command that
   * would talk to a pi process refuses with `"ai_disabled"` — the
   * frontend shows a friendly notice instead. Toggled via
   * `set_ai_enabled` (no full PiConfig roundtrip + no pi respawn).
   */
  enabled: boolean;
};

/** One entry from `pi models` — what's available on the user's machine. */
export type PiModel = {
  name: string;
  provider: string;
  /** pi's own default/current model, shown highlighted but not enforced. */
  active: boolean;
};

/** Streaming delta emitted by pi as a `chat-stream` Tauri event. */
export type PiStreamChunk = {
  content: string;
  done: boolean;
};


export type EnvelopeSummary = {
  id: string;
  accountId: string;
  accountColor: string;
  folderId: string;
  subject: string;
  fromFirst: string;
  date: string;
  seen: boolean;
  answered: boolean;
  flagged: boolean;
  forwarded: boolean;
  /** `$Junk` IMAP keyword. True = this mail was marked as spam. */
  junk: boolean;
  bodyCached: boolean;
  /** Drives the paperclip glyph in the inbox list. Heuristic at sync
   *  time (top-level `multipart/mixed` Content-Type), authoritative
   *  once the body has been fetched and the MIME tree parsed. Inline
   *  parts (cid: images) don't count. */
  hasAttachments: boolean;
  /** Optional ScheduledAction-Tag — eine Workflow-Rule hat eine geplante
   *  Aktion auf diese Mail gemerkt. `null` wenn keine Rule getaggt hat.
   *  Frontend rendert daraus den Auto-Rule-Marker mit Hover-Tooltip
   *  ("Aktion fällig in 4 Tagen → Archiv durch Regel 'Newsletter'"). */
  scheduled: ScheduledActionTag | null;
};

// ─── Workflow-Rule Scheduling ───────────────────────────────────────

/** Mögliche Aktionen, die eine Workflow-Rule beim Match ausführen kann.
 *  `run_workflow` ist die alte Bedeutung (führt einen mehrstufigen
 *  Workflow aus); die anderen drei sind die neuen Direkt-Aktionen, mit
 *  denen einfache "diese Mail soll weg"-Regeln keinen leeren Workflow
 *  als Vehikel mehr brauchen. */
export type RuleAction = "run_workflow" | "archive" | "delete" | "move";

export type RuleActionResult = "ok" | "skipped" | "failed";

/** Snapshot der geplanten Aktion auf einer Envelope-Row. */
export type ScheduledActionTag = {
  /** ISO 8601, UTC. Wenn ≤ now → Sweeper packt sie beim nächsten Tick. */
  scheduledAt: string;
  action: RuleAction;
  /** Nur für `move` gesetzt. */
  actionDest: string | null;
  ruleId: string | null;
  ruleName: string;
  /** Nur für `run_workflow` gesetzt — auf welchen Workflow zeigt der Tag. */
  workflowId: string | null;
  /** Trockenmodus: Tag wurde gesetzt, aber der Sweeper überspringt. */
  dryRun: boolean;
};

export type RuleActionLogEntry = {
  id: string;
  ruleId: string | null;
  ruleName: string;
  action: RuleAction;
  actionDest: string | null;
  workflowId: string | null;
  messageId: string;
  subjectSnapshot: string;
  senderSnapshot: string;
  result: RuleActionResult;
  errorMessage: string | null;
  ranAt: string;
};

export type RuleSweepReport = {
  ok: number;
  skipped: number;
  failed: number;
};

export type AccountAlias = {
  id: string;
  accountId: string;
  email: string;
  fromName: string;
};

/** Wire-Format-Variante für SyncMode aus dem Backend. snake_case auf
 *  beiden Seiten (serde `rename_all = "snake_case"`). */
export type SyncMode = "idle" | "polling" | "idle_and_polling";

export type AccountSummary = {
  id: string;
  displayName: string;
  address: string;
  fromName: string;
  color: string;
  signature: string | null;
  signatureHtml: string | null;
  archiveFolder: string;
  sentFolder: string;
  draftsFolder: string;
  trashFolder: string;
  spamFolder: string;
  /** Workflow toggle: auto-archive parent after a reply is sent. */
  archiveOnReply: boolean;
  /** Background body-prefetch window in days. 0 = disabled. */
  prefetchDays: number;
  /** IDLE / Polling / beides — Background-Sync-Strategie pro Konto. */
  syncMode: SyncMode;
  /** Server speichert gesendete Mails automatisch im Sent-Ordner.
   *  Wird beim Account-Setup via Probe-Mail ermittelt; bei `true`
   *  skippt der SMTP-Send-Pfad seinen eigenen IMAP-APPEND. */
  serverStoresSent: boolean;
  imapHost: string;
  imapPort: number;
  imapTls: boolean;
  smtpHost: string;
  smtpPort: number;
  smtpTls: boolean;
  aliases: AccountAlias[];
};

export type AliasForm = {
  email: string;
  fromName: string;
};

export type UpdateAccountForm = {
  id: string;
  displayName: string;
  address: string;
  fromName: string;
  color: string;
  signature: string | null;
  signatureHtml: string | null;
  imapHost: string;
  imapPort: number;
  imapTls: boolean;
  smtpHost: string;
  smtpPort: number;
  smtpTls: boolean;
  archiveFolder: string;
  sentFolder: string;
  draftsFolder: string;
  trashFolder: string;
  spamFolder: string;
  archiveOnReply: boolean;
  prefetchDays: number;
  syncMode: SyncMode;
  /** Direkt-Wert beim Edit (anders als bei NewAccountForm — kein Probe). */
  serverStoresSent: boolean;
  aliases: AliasForm[];
  /** null or empty string → keep existing secret */
  password: string | null;
  skipTest?: boolean;
};

export type NewAccountForm = {
  displayName: string;
  address: string;
  fromName: string;
  color: string;
  signature: string | null;
  signatureHtml: string | null;
  imapHost: string;
  imapPort: number;
  imapTls: boolean;
  smtpHost: string;
  smtpPort: number;
  smtpTls: boolean;
  archiveFolder: string;
  sentFolder: string;
  draftsFolder: string;
  trashFolder: string;
  spamFolder: string;
  archiveOnReply: boolean;
  prefetchDays: number;
  /** Default `idle` wenn weggelassen; entspricht Backend-Default. */
  syncMode?: SyncMode;
  /** Override für die Server-Auto-Save-Detection. `null` (oder
   *  weggelassen) → Backend führt eine Probe-Mail durch und ermittelt
   *  den Wert automatisch. Explizit `true`/`false` → Probe wird
   *  geskippt und der Wert direkt übernommen (für Test-Setups oder
   *  Provider die der User schon kennt). */
  serverStoresSent?: boolean | null;
  aliases: AliasForm[];
  password: string;
  skipTest?: boolean;
};

export type VerboseStep = {
  elapsedMs: number;
  kind: "info" | "ok" | "err";
  message: string;
};

export type VerboseReport = {
  ok: boolean;
  totalMs: number;
  steps: VerboseStep[];
};

export type SpamPatternType =
  | "from_email"
  | "from_domain"
  | "subject_contains"
  | "subject_regex"
  | "body_contains"
  | "header_contains";

export type SpamRule = {
  id: string;
  accountId: string | null;
  patternType: SpamPatternType;
  pattern: string;
  enabled: boolean;
  confidence: number | null;
  reason: string | null;
  createdAt: string;
  hitCount: number;
};

export type RuleDraft = {
  accountId?: string | null;
  patternType: SpamPatternType;
  pattern: string;
  confidence?: number | null;
  reason?: string | null;
};

export type RuleMatch = {
  messageId: string;
  subject: string;
  fromEmail: string;
  folderName: string;
  accountId: string;
};

export type ApplyRuleResult = {
  rule: SpamRule;
  /** Wie viele Mails das Pattern erkannt hat — inkl. derer, die schon
   *  im Spam-Ordner liegen. */
  matched: number;
  /** Tatsächlich aus Inbox/Archive in den Spam-Ordner verschoben. */
  moved: number;
  /** Vom Pattern erkannt, aber schon im Spam-Ordner — kein Move nötig.
   *  Damit "0 von 0 verschoben" nicht mehr suggestiert, die Regel hätte
   *  gar nichts erkannt. */
  alreadyInSpam: number;
  movedRows: RuleMatch[];
};

export type CandidateFeatures = {
  messageId: string;
  fromEmail: string;
  fromDomain: string;
  subject: string;
  relevantHeaders: string[];
  bodyPreview: string | null;
};

export type SuggestResult = {
  drafts: RuleDraft[];
  features: CandidateFeatures[];
  rawResponse: string;
};

/** Live progress tick emitted by the Rust sync task. */
export type SyncProgress = {
  accountId: string;
  accountName: string;
  /** Empty string when `done === true`. */
  folder: string;
  fetched: number;
  total: number;
  done: boolean;
  /** Count of *newly inserted* INBOX rows during this whole sync.
   *  `fetched` double-counts re-syncs of UIDs we already have, so
   *  it can't drive the new-mail chime — `newInInbox` is the value
   *  to gate on. Always 0 on intermediate ticks; the cumulative
   *  number only lands on the final `done === true` event. */
  newInInbox: number;
};

export type UnifiedUnreadCount = {
  /** "inbox" | "archive" | "sent" | "drafts" | "trash" | "spam" */
  folder: string;
  unread: number;
};

export type FolderSummary = {
  id: string;
  accountId: string;
  /** IMAP folder path exactly as the server returned it (e.g. "INBOX.Sent"). */
  name: string;
  total: number;
  unread: number;
  /** Per-folder sync toggle. Default true. User can opt out in settings. */
  syncEnabled: boolean;
};

export type DiscoveredFolders = {
  archive: string | null;
  sent: string | null;
  drafts: string | null;
  trash: string | null;
  spam: string | null;
  all: string[];
};

export type SyncReport = {
  folder: string;
  fetched: number;
  stored: number;
  durationMs: number;
};

export type Address = {
  name: string | null;
  email: string;
};

export type EnvelopeDetail = {
  id: string;
  accountId: string;
  folderId: string;
  folderName: string;
  imapUid: number;
  messageIdHeader: string | null;
  subject: string;
  date: string;
  from: Address[];
  to: Address[];
  cc: Address[];
  seen: boolean;
  answered: boolean;
  flagged: boolean;
  forwarded: boolean;
  /** `$Junk` IMAP keyword — marked as spam by user or server filter. */
  junk: boolean;
  bodyCached: boolean;
};

export type Flags = {
  seen: boolean;
  answered: boolean;
  flagged: boolean;
  forwarded: boolean;
  junk: boolean;
  draft: boolean;
  deleted: boolean;
};

export type FlagChanges = {
  seen?: boolean;
  answered?: boolean;
  flagged?: boolean;
  forwarded?: boolean;
  junk?: boolean;
};

export type AttachmentMeta = {
  partIdx: number;
  filename: string;
  mimeType: string;
  sizeBytes: number;
  contentId: string | null;
  isInline: boolean;
};

// ─── Calendar (timeProtocol bounded context) ─────────────────────────────

export type IcsParticipant = {
  email: string;
  displayName: string | null;
};

export type ParsedIcsEvent = {
  /** VCALENDAR-level METHOD ("REQUEST", "REPLY", "CANCEL", …). null when the
   *  ICS has no METHOD property — typically a calendar publication rather
   *  than an invitation. */
  method: string | null;
  uid: string;
  sequence: number;
  summary: string | null;
  description: string | null;
  location: string | null;
  /** Raw RFC 5545 timestamp string. Frontend formats for display. */
  dtstart: string | null;
  dtend: string | null;
  organizer: IcsParticipant | null;
  attendees: IcsParticipant[];
  /** True when the event has at least one attendee — drives the visibility
   *  of the Annehmen/Vielleicht/Ablehnen buttons. */
  isInvitation: boolean;
};

export type InvitationResponse = "accepted" | "tentative" | "declined";

export type InvitationReplyDraft = {
  response: InvitationResponse;
  eventSummary: string | null;
  eventDtstart: string | null;
  recipientEmail: string;
  recipientDisplayName: string | null;
  attachmentPath: string;
  attachmentFilename: string;
  attachmentSizeBytes: number;
};

// ─── Phase 1: locally stored commitments ─────────────────────────────────

export type CommitmentSource = "manual" | "ics_import" | "negotiation";

/** RFC 5545 STATUS values mirrored locally. ADR-0011 §3 mandates STATUS
 *  on the wire; we mirror it on the row so cancellation can be a normal
 *  mutation (SEQUENCE+1, status=CANCELLED) rather than a hard delete. */
export type CommitmentStatus = "CONFIRMED" | "CANCELLED" | "TENTATIVE";

export type CommitmentAttendee = {
  email: string;
  displayName: string | null;
  /** RFC 5545 PARTSTAT (`ACCEPTED`, `DECLINED`, `TENTATIVE`, …). `null`
   *  for attendees we have no status for (typically participants the
   *  user manually added to a self-created event). */
  partstat: string | null;
};

/** One stored commitment row. `id` is our local UUID, `uid` is the
 *  RFC 5545 cross-system UID — they are intentionally distinct so a
 *  re-import of the same invitation upserts in place without breaking
 *  any UI selection state pointing at the local id. */
export type Commitment = {
  id: string;
  uid: string;
  sequence: number;
  summary: string | null;
  description: string | null;
  location: string | null;
  /** RFC 3339 with explicit offset, e.g. `2026-04-23T09:00:00+02:00`. */
  startAt: string;
  endAt: string;
  /** Original RFC 5545 TZID, kept for ICS round-trip on export. */
  originalTzid: string | null;
  organizer: IcsParticipant | null;
  attendees: CommitmentAttendee[];
  source: CommitmentSource;
  /** Lifecycle state. The list view filters CANCELLED out by default. */
  status: CommitmentStatus;
  /** When `source === "ics_import"`: the message the event was imported
   *  from. Useful to deep-link "view source mail". */
  sourceMessageId: string | null;
  /** ISO 8601 UTC. */
  createdAt: string;
  updatedAt: string;
};

/** Form payload for create/update. UID, id, sequence, source, and timestamps
 *  are managed by the backend; the form only carries user-editable fields. */
export type CommitmentDraft = {
  summary: string | null;
  description: string | null;
  location: string | null;
  startAt: string;
  endAt: string;
  originalTzid: string | null;
  organizer: IcsParticipant | null;
  attendees: CommitmentAttendee[];
};

export type ExportedIcs = {
  content: string;
  filename: string;
  /** Set when the export call also wrote the blob to a path on disk. */
  writtenTo: string | null;
};

// ─── Phase 2: IMAP-Folder-Sync per ADR-0011 ──────────────────────────────

/** Calendar IMAP-sync configuration. `enabled === false` keeps the
 *  calendar in Phase-1 local-only mode. When enabled, `accountId` is
 *  required; the sync command refuses without it. */
export type CalendarConfig = {
  enabled: boolean;
  accountId: string | null;
  /** Raw IMAP path. Default `INBOX/TimeProtocol/Calendar`. Cyrus-style
   *  servers using `.` as the hierarchy delimiter need an override
   *  (e.g. `INBOX.TimeProtocol.Calendar`) per ADR-0011 §2 — but doing so
   *  forfeits cross-implementation interop guarantees. */
  folderPath: string;
  /** Background-task interval in seconds. Floor 60. 0 = disabled (only
   *  manual / IDLE / mutation triggers run). Default 300 (5 min). */
  autoSyncIntervalSeconds: number;
  /** Long-lived IMAP-IDLE session that triggers a sync on every server
   *  push notification. Lower latency than polling for typical home
   *  servers; pull cost = one persistent IMAP slot. Default true. */
  idleEnabled: boolean;
  /** Fire a fire-and-forget background publish after every local CRUD
   *  so the user doesn't have to click Sync after every edit. Default
   *  true. */
  syncOnMutation: boolean;
  /** Run a compaction pass after every successful sync that moves
   *  superseded ICS messages into `<folder>/Archive`. Default true. */
  compactionEnabled: boolean;
};

/** Result of `cal_sync_imap`. Surfaced to the UI so the sync button can
 *  show "X imported, Y published, Z unchanged" without re-fetching.
 *  Distinct from the mail-side `SyncReport` (folder/fetched/stored). */
export type CalendarSyncReport = {
  imported: number;
  published: number;
  unchanged: number;
  /** Number of superseded ICS messages moved to `<folder>/Archive`
   *  during the post-sync compaction pass. 0 when compaction is off. */
  compacted: number;
  /** UIDs detected as server-side hard-deleted (present in our local
   *  state at last sync, gone now, no local mutation since). Marked
   *  STATUS:CANCELLED locally; not republished. */
  remoteDeleted: number;
  errors: string[];
};

export type MessageDetail = {
  envelope: EnvelopeDetail;
  plainText: string | null;
  htmlText: string | null;
  attachments: AttachmentMeta[];
};

// ─── Contacts (Adressbuch) ────────────────────────────────────────────

export type ContactOrigin = "user" | "extracted";

export type Tag = {
  id: string;
  name: string;
  color: string | null;
  createdAt: string;
};

export type Contact = {
  id: string;
  displayName: string;
  organization: string | null;
  jobTitle: string | null;
  phone: string | null;
  mobile: string | null;
  street: string | null;
  zip: string | null;
  city: string | null;
  country: string | null;
  website: string | null;
  notes: string;
  origin: ContactOrigin;
  pinned: boolean;
  lastExtractedEnvelopeId: string | null;
  createdAt: string;
  updatedAt: string;
};

export type ContactEmail = {
  id: number;
  contactId: string;
  email: string;
  isPrimary: boolean;
};

/** ContactDetail mit eingebetteten Adressen + Mail-Stats. Backend
 *  flattens den Contact via #[serde(flatten)], deshalb kommen die
 *  Stamm-Felder alle direkt rein. */
export type ContactDetail = Contact & {
  emails: ContactEmail[];
  tags: Tag[];
  messageCount: number;
  lastMessageAt: string | null;
};

export type ContactSummary = {
  id: string;
  displayName: string;
  organization: string | null;
  city: string | null;
  primaryEmail: string | null;
  pinned: boolean;
  messageCount: number;
  lastMessageAt: string | null;
};

export type ContactForm = {
  displayName: string;
  organization: string | null;
  jobTitle: string | null;
  phone: string | null;
  mobile: string | null;
  street: string | null;
  zip: string | null;
  city: string | null;
  country: string | null;
  website: string | null;
  notes: string;
  pinned: boolean;
};

/** Reader-Header-Lookup-Result. Tagged union mit `kind`. */
export type ContactLookup =
  | { kind: "contact"; contact: Contact }
  | {
      kind: "history_only";
      displayName: string | null;
      sendCount: number;
      recvCount: number;
    }
  | { kind: "unknown" };

/** Auto-Extraction-Ergebnis. */
export type ExtractedFields = {
  name: string;
  organization: string;
  jobTitle: string;
  phone: string;
  mobile: string;
  street: string;
  zip: string;
  city: string;
  country: string;
  website: string;
};

export type ExtractionResult =
  | { kind: "created"; contactId: string; fields: ExtractedFields }
  | { kind: "empty" }
  | { kind: "already_exists"; contactId: string }
  | { kind: "not_applicable"; reason: string }
  | { kind: "skipped"; reason: string };

/** Compose-Autocomplete-Item aus dem Backend.
 *  Wenn `contactId` gesetzt ist, gibt es einen kuratierten Kontakt
 *  für diese Adresse — `contactDisplayName` ist dann der vom User
 *  gepflegte Name (überschreibt den Mail-Header-Namen). */
export type AddressCompletion = {
  email: string;
  displayName: string | null;
  contactId: string | null;
  contactDisplayName: string | null;
  sendCount: number;
  recvCount: number;
  lastSeenAt: string;
};

export type ComposeAttachment = {
  /** Local identifier for UI list keys. */
  clientId: string;
  /** Absolute path on disk — passed straight through to the Rust side. */
  path: string;
  filename: string;
  sizeBytes: number;
  mimeType?: string;
};

// ─── Workflows ────────────────────────────────────────────────────────
// Tagged union matching `domain::workflow::Step` on the Rust side.
// camelCase on the wire, `type` is the discriminant (serde `#[serde(tag)]`).

export type WorkflowStep =
  | { type: "saveAttachments"; targetDir: string; filter?: string | null }
  | { type: "saveBody"; path: string; format: "md" | "txt" | "eml" }
  | { type: "runScript"; script: string; parameters: ScriptParam[] };

export type ScriptParameterKind = "positional" | "option" | "flag";
export type ScriptValueType =
  | "string"
  | "number"
  | "boolean"
  | "choice"
  | "path";

/** Where the runtime value of a parameter comes from. */
export type ParamSource =
  | { kind: "fixed"; value: string }
  | { kind: "template"; var: string }
  | { kind: "firstAttachment"; extension: string }
  /** User wird beim Workflow-Apply gefragt. Optionaler
   *  `defaultTemplate` (literal oder $var) wird im Dialog vorgeblendet,
   *  user kann ändern. Bei Auto-Trigger ohne Default schlägt der Run
   *  mit Fehler auf — der `defaultTemplate` ist dann der einzige Weg
   *  durch. */
  | { kind: "prompt"; defaultTemplate: string | null };

/** Extensions the guided editor offers for `FirstAttachment`.
 *  Users can pick any of these; the backend matches the suffix
 *  case-insensitively so compound suffixes like `tar.gz` work too. */
export const WORKFLOW_ATTACHMENT_EXTENSIONS = [
  "csv",
  "pdf",
  "xml",
  "json",
  "zip",
  "tar.gz",
] as const;

export type WorkflowAttachmentExtension =
  (typeof WORKFLOW_ATTACHMENT_EXTENSIONS)[number];

export type ScriptParam = {
  key: string;
  cliName: string;
  kind: ScriptParameterKind;
  label: string;
  valueType: ScriptValueType;
  choices: string[];
  helpText: string | null;
  required: boolean;
  defaultValue: string | null;
  source: ParamSource;
  order: number;
  enabled: boolean;
};

export type Workflow = {
  id: string;
  name: string;
  hotkey: string | null;
  steps: WorkflowStep[];
  enabled: boolean;
  /** Move the message into the account's archive folder after the
   *  workflow ran through with all steps ok. Partial failures don't
   *  archive. */
  archiveAfterSuccess: boolean;
  createdAt: string;
  runCount: number;
  lastRunAt: string | null;
};

export type WorkflowDraft = {
  name: string;
  hotkey: string | null;
  steps: WorkflowStep[];
  enabled: boolean;
  archiveAfterSuccess: boolean;
};

export type WorkflowConfig = {
  /** Allow-listed directory that houses all Python scripts. */
  scriptDir: string;
  /** Python interpreter command or absolute path. Default per-OS. */
  pythonBin: string;
};

export type WorkflowStepResult = {
  stepIndex: number;
  stepType: "saveAttachments" | "saveBody" | "runScript";
  ok: boolean;
  message: string;
  detail: string | null;
};

export type WorkflowRunResult = {
  workflowId: string;
  messageId: string;
  steps: WorkflowStepResult[];
  allOk: boolean;
};

// ─── Workflow rules (auto-trigger) ───────────────────────────────────

export type RulePredicate =
  | { kind: "fromEmail"; value: string }
  | { kind: "fromDomain"; value: string }
  | { kind: "fromDomainIn"; values: string[] }
  | { kind: "subjectContains"; value: string }
  | { kind: "hasAttachmentExtension"; extension: string };

export type RuleMode = "auto" | "confirm";

export type WorkflowRule = {
  id: string;
  /** Display label, used in the auto-rule marker tooltip and the rules
   *  list. Required for new rules; older rows may be empty (UI falls
   *  back to the workflow name in that case). */
  name: string;
  /** Required when `action === "run_workflow"`. Null for direct-action
   *  rules (Archive/Delete/Move). */
  workflowId: string | null;
  accountId: string | null;
  /** Optional IMAP folder name. null = all folders. Case-sensitive
   *  exact match, same semantics as IMAP itself. */
  folderName: string | null;
  predicates: RulePredicate[];
  mode: RuleMode;
  /** Was beim Match passieren soll. Default `run_workflow` (alte
   *  Semantik). Direkt-Actions verzichten auf den Workflow-Umweg. */
  action: RuleAction;
  /** Nur für `move` — Zielordner. */
  actionDest: string | null;
  /** 0 = sofort, >0 = `mail.date + delayMinutes` ist Fälligkeitsdatum.
   *  Minuten als Einheit deckt 'in 10 Min weg' bis 'in 30 Tagen weg'
   *  (43200 Min) mit einem einzigen Feld ab. */
  delayMinutes: number;
  /** Tag setzen, aber Sweeper überspringt — UX-Schutz beim Anlegen. */
  dryRun: boolean;
  enabled: boolean;
  createdAt: string;
  hitCount: number;
  lastHitAt: string | null;
};

export type WorkflowRuleDraft = {
  name: string;
  workflowId: string | null;
  accountId: string | null;
  folderName: string | null;
  predicates: RulePredicate[];
  mode: RuleMode;
  action: RuleAction;
  actionDest: string | null;
  delayMinutes: number;
  dryRun: boolean;
  enabled: boolean;
};

/** Payload of the `workflow-rule-match` Tauri event emitted by the
 *  matcher for confirm-mode hits. */
export type RuleMatchEvent = {
  ruleId: string;
  workflowId: string;
  workflowName: string;
  messageId: string;
  fromEmail: string;
  subject: string;
};

// ─── Workflow training (pi-based rule learner) ──────────────────────

export type WorkflowTrainingCandidate = {
  messageId: string;
  subject: string;
  fromEmail: string;
  fromDomain: string;
  folderName: string;
  accountId: string;
  addedAt: string;
};

export type TrainingFeatures = {
  messageId: string;
  fromEmail: string;
  fromDomain: string;
  subject: string;
  folderName: string;
  accountDisplayName: string;
  accountAddress: string;
  attachmentExtensions: string[];
  bodyPreview: string | null;
};

export type RuleProposal = {
  predicates: RulePredicate[];
  folderName: string | null;
  accountId: string | null;
  mode: RuleMode;
  /** pi schlägt eine Action vor — typischerweise `run_workflow` (alte
   *  Semantik), darf aber auch `archive`/`delete`/`move` sein wenn die
   *  Trainings-Beispiele nach simpler Direkt-Aktion aussehen. */
  action: RuleAction;
  actionDest: string | null;
  /** Frist in Tagen ab Mail-Datum, bevor die Action greift. */
  delayMinutes: number;
  /** Sicherheits-Empfehlung. pi setzt das per Default auf `true` bei
   *  vorgeschlagenen Direkt-Aktionen, damit der User Treffsicherheit
   *  beobachten kann, bevor Mails verschwinden. */
  dryRun: boolean;
  reason: string | null;
};

export type WorkflowTrainingResult = {
  proposal: RuleProposal;
  features: TrainingFeatures[];
  rawResponse: string;
};

export const WORKFLOW_TRAINING_CANCELLED = "cancelled_by_user";

/** Known template variables the user can bind parameters to.
 *  Reihenfolge entspricht der Sortierung im Template-Picker —
 *  meistgenutztes oben (`subject`/`from`), dann Date-Gruppe, dann
 *  die dynamisch materialisierten Pfad-Vars. */
export const WORKFLOW_TEMPLATE_VARS = [
  "subject",
  "from",
  // RFC3339-Komplettzeitstempel ($date) bleibt für Backward-Compat,
  // ist aber selten was der User wirklich will.
  "date",
  // Saubere ISO-Datums-Form: YYYY-MM-DD. Filename-safe.
  "date_iso",
  // Deutsches Anzeige-Format: DD.MM.YYYY.
  "date_de",
  // YYYY-MM-DD HH:MM — was Clockodo / die meisten APIs als
  // Datum+Zeit-Argument verlangen.
  "datetime",
  // Sekunden- bzw. ISO-T-Variante für Spezialfälle.
  "datetime_seconds",
  "datetime_iso",
  // Filename-freundlicher Komplettzeitstempel: 20260430-1430.
  "datetime_compact",
  // 24h-Zeit, lokal.
  "time",
  "time_seconds",
  // Einzelteile als Bausteine.
  "year",
  "month",
  "day",
  // Dynamisch — werden erst zur Apply-Zeit materialisiert.
  "attachments_dir",
  "csv",
  "body_md",
] as const;

export type WorkflowTemplateVar = (typeof WORKFLOW_TEMPLATE_VARS)[number];

export type ComposeDraft = {
  accountId?: string;
  /** Optional override for the From-identity (= "Account::email" key in
   *  Compose). Used by the undo-send path to restore the exact identity
   *  the user picked, including alias. */
  identityKey?: string;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
  /** Optional full HTML snapshot of the editor body. When present, Compose
   *  uses it verbatim as the editor's initial content and skips the usual
   *  signature/quote re-injection. Set by the undo-send round-trip so a
   *  cancelled send re-opens with byte-identical body, signature, and
   *  attachments — no reformatting surprises. */
  bodyHtml?: string;
  /** Pre-seeded HTML block appended after the user's own body when sending.
   *  Set by reply/forward builders with the quoted original; the user never
   *  sees or edits this — they only edit the plain body above. */
  quotedHtml?: string;
  /** Plain-text version of the same quote, already concatenated into `body`. */
  quotedPlain?: string;
  /** RFC Message-Id header of the parent (for SMTP threading). */
  inReplyToHeader?: string;
  /** Internal message id of the parent — used to mark it after send. */
  parentMessageId?: string;
  /** "answered" for replies, "forwarded" for forwards. */
  parentMode?: "answered" | "forwarded";
  references?: string[];
  /** Seeded attachments (currently only used when Compose is re-opened with
   *  a draft that had pre-attached files; UI additions live in local state). */
  attachments?: ComposeAttachment[];
  /** Wenn gesetzt, ist das Compose-Fenster eine Bearbeitung eines bereits
   *  auf dem Server liegenden Drafts. Nach erfolgreichem Senden ODER
   *  erneutem Save-Draft wird die Original-Mail mit dieser ID gelöscht
   *  (Best-Effort) — sonst sammeln sich Duplikate im Drafts-Ordner.
   *  Doppelklick auf Drafts setzt das Feld via `buildEditDraft`. */
  replacesDraftMessageId?: string;
};

/** Vom Backend-`draft_import`-Modul gelieferter Roh-Draft. Wird aus
 *  einem externen CLI-Trigger (`crystalmail.exe --draft-from-template …`)
 *  erzeugt: Markdown-Template + Frontmatter werden geparsed, Variablen
 *  substituiert, Anhänge mit Metadaten ausgestattet. Vom Frontend in
 *  einen `ComposeDraft` umgesetzt und in den Composer geladen. */
export type PreparedImportDraft = {
  /** Mail-Adresse des From-Accounts laut Frontmatter. Frontend mappt
   *  auf die passende `AccountSummary`; matched nichts oder ist leer,
   *  fällt der Composer auf den Default-Account zurück. */
  accountEmail: string | null;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
  attachments: ComposeAttachment[];
  /** Roh-Pfad des Templates — rein für UI-Anzeige bzw. Logging. */
  sourceTemplate: string;
};
