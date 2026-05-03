-- Unify Lifetime-Rules unter Workflow-Rules. Eine Tabelle, eine
-- Match-Pipeline, ein Sweeper. Das ursprünglich in 0024 angelegte
-- Lifetime-System verschwindet; sein Funktionsumfang wandert vollständig
-- in workflow_rules + envelopes-Spalten.
--
-- Schritt 1: workflow_rules erweitern um Action-Achse + Delay + Trocken-
-- modus + Anzeigename. Action-Typ "run_workflow" ist Default → bestehende
-- Rows behalten exakt ihr altes Verhalten. Neue Rows können statt eines
-- Workflows direkt eine simple Action wählen (archive/delete/move).
--
-- Schritt 2: workflow_id auf NULL erlauben. SQLite hat keinen direkten
-- ALTER TABLE für NOT-NULL → NULL, also klassischer Tabellen-Rewrite:
-- Daten in Temp-Tabelle, Original droppen, frisch anlegen, zurückkopieren.
-- Foreign Key + Indizes werden mit neu aufgebaut.
--
-- Schritt 3: lifetime_rules + lifetime_actions_log abreißen, envelopes-
-- Spalten umbenennen (expires_* → scheduled_*) und um workflow-id
-- erweitern. Views neu bauen damit sie die neue Spaltenliste sehen.

-- ─── 1) workflow_rules: neue Spalten ─────────────────────────────────────
-- Default-Werte sorgen dafür, dass der Tabellen-Rewrite weiter unten alle
-- bestehenden Rows korrekt befüllt: action_type='run_workflow' bewahrt
-- die alte Semantik, delay_days=0 = sofort, dry_run=0 = live.
ALTER TABLE workflow_rules ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE workflow_rules ADD COLUMN action_type TEXT NOT NULL DEFAULT 'run_workflow'
  CHECK (action_type IN ('run_workflow', 'archive', 'delete', 'move'));
ALTER TABLE workflow_rules ADD COLUMN action_dest TEXT;
ALTER TABLE workflow_rules ADD COLUMN delay_days INTEGER NOT NULL DEFAULT 0
  CHECK (delay_days >= 0);
ALTER TABLE workflow_rules ADD COLUMN dry_run INTEGER NOT NULL DEFAULT 0
  CHECK (dry_run IN (0, 1));

-- ─── 2) workflow_id NULLABLE machen via Tabellen-Rewrite ───────────────
-- Indizes auf der alten Tabelle vorher entfernen, sonst kollidieren sie
-- beim Re-Create. PRAGMA foreign_keys während des Rewrites aus, damit der
-- Rename auf eine FK-haltige Tabelle nicht durch hängende Constraints
-- blockiert.
PRAGMA foreign_keys = OFF;

DROP INDEX IF EXISTS idx_wf_rules_workflow;
DROP INDEX IF EXISTS idx_wf_rules_enabled;
DROP INDEX IF EXISTS idx_wf_rules_account;

ALTER TABLE workflow_rules RENAME TO _wfr_old;

CREATE TABLE workflow_rules (
  id              TEXT PRIMARY KEY,
  workflow_id     TEXT REFERENCES workflows(id) ON DELETE CASCADE,
  account_id      TEXT REFERENCES accounts(id) ON DELETE CASCADE,
  predicates_json TEXT NOT NULL,
  mode            TEXT NOT NULL CHECK (mode IN ('auto', 'confirm')),
  enabled         INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  created_at      TEXT NOT NULL,
  hit_count       INTEGER NOT NULL DEFAULT 0,
  last_hit_at     TEXT,
  folder_name     TEXT,
  name            TEXT NOT NULL DEFAULT '',
  action_type     TEXT NOT NULL DEFAULT 'run_workflow'
    CHECK (action_type IN ('run_workflow', 'archive', 'delete', 'move')),
  action_dest     TEXT,
  delay_days      INTEGER NOT NULL DEFAULT 0 CHECK (delay_days >= 0),
  dry_run         INTEGER NOT NULL DEFAULT 0 CHECK (dry_run IN (0, 1))
);

INSERT INTO workflow_rules (
  id, workflow_id, account_id, predicates_json, mode, enabled,
  created_at, hit_count, last_hit_at, folder_name,
  name, action_type, action_dest, delay_days, dry_run
)
SELECT
  id, workflow_id, account_id, predicates_json, mode, enabled,
  created_at, hit_count, last_hit_at, folder_name,
  name, action_type, action_dest, delay_days, dry_run
  FROM _wfr_old;

DROP TABLE _wfr_old;

CREATE INDEX idx_wf_rules_workflow ON workflow_rules(workflow_id);
CREATE INDEX idx_wf_rules_enabled  ON workflow_rules(enabled);
CREATE INDEX idx_wf_rules_account  ON workflow_rules(account_id);

PRAGMA foreign_keys = ON;

-- ─── 3) lifetime_* abreißen ─────────────────────────────────────────────
-- Aus 0024 stammten diese Tabellen + Spalten. Da das Lifetime-Feature in
-- der UI noch keinen Punkt hatte, an dem User Daten erzeugen konnten
-- (kein Settings-CRUD), ist das ein verlustfreier Drop.
DROP TABLE IF EXISTS lifetime_rules;
DROP TABLE IF EXISTS lifetime_actions_log;

-- envelopes-Spalten aus 0024 entfernen. SQLite ≥ 3.35 hat DROP COLUMN.
-- SQLCipher ≥ 4.5 (was wir bundeln) ist drauf, also direkt nutzen.
-- Index zuerst weg (sonst meckert der Drop).
DROP INDEX IF EXISTS idx_envelopes_expires_at;
ALTER TABLE envelopes DROP COLUMN expires_at;
ALTER TABLE envelopes DROP COLUMN expires_action;
ALTER TABLE envelopes DROP COLUMN expires_action_dest;
ALTER TABLE envelopes DROP COLUMN expires_rule_id;
ALTER TABLE envelopes DROP COLUMN expires_rule_name;
ALTER TABLE envelopes DROP COLUMN expires_dry_run;

-- ─── 4) envelopes-Snapshot fürs zeitversetzte Action-Dispatching ───────
-- Spaltennamen-Schema: scheduled_<feld> statt expires_<feld>. Das ist
-- semantisch ehrlicher: die Mail hat nicht "Verfallsdatum", sondern eine
-- "geplante Action" — die kann auch direkt sofort fällig sein (delay_days=0).
-- workflow_id-Snapshot hilft dem Sweeper bei action_type='run_workflow':
-- die Rule selbst könnte zwischen Tagging und Sweep gelöscht worden sein,
-- den Snapshot fragen wir trotzdem an um den Workflow-Lookup zu haben.
ALTER TABLE envelopes ADD COLUMN scheduled_at TEXT;
ALTER TABLE envelopes ADD COLUMN scheduled_action_type TEXT;
ALTER TABLE envelopes ADD COLUMN scheduled_action_dest TEXT;
ALTER TABLE envelopes ADD COLUMN scheduled_rule_id TEXT;
ALTER TABLE envelopes ADD COLUMN scheduled_rule_name TEXT;
ALTER TABLE envelopes ADD COLUMN scheduled_workflow_id TEXT;
ALTER TABLE envelopes ADD COLUMN scheduled_dry_run INTEGER NOT NULL DEFAULT 0
  CHECK (scheduled_dry_run IN (0, 1));

CREATE INDEX idx_envelopes_scheduled_at ON envelopes (scheduled_at)
  WHERE scheduled_at IS NOT NULL;

-- ─── 5) Audit-Log: einheitliches workflow_rule_actions_log ────────────
-- Ersetzt das gedroppte lifetime_actions_log. Beide Action-Klassen
-- (direkt: archive/delete/move; mittelbar: run_workflow) loggen hier
-- ihren Run.
CREATE TABLE workflow_rule_actions_log (
  id              TEXT PRIMARY KEY,
  rule_id         TEXT,
  rule_name       TEXT NOT NULL,
  action_type     TEXT NOT NULL,
  action_dest     TEXT,
  workflow_id     TEXT,
  message_id      TEXT NOT NULL,
  subject_snapshot TEXT NOT NULL,
  sender_snapshot TEXT NOT NULL,
  result          TEXT NOT NULL CHECK (result IN ('ok', 'skipped', 'failed')),
  error_message   TEXT,
  ran_at          TEXT NOT NULL
);

CREATE INDEX idx_wfra_rule ON workflow_rule_actions_log (rule_id, ran_at DESC);
CREATE INDEX idx_wfra_ran  ON workflow_rule_actions_log (ran_at DESC);

-- ─── 6) Unified-Views neu bauen ─────────────────────────────────────────
-- Wir haben envelopes massiv umgebaut (sechs Spalten weg, sieben dazu);
-- die Views müssen ihre Spaltenliste auffrischen. Identische Definitionen
-- wie zuletzt — nur droppen und neu anlegen.
DROP VIEW IF EXISTS unified_inbox;
CREATE VIEW unified_inbox AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
   WHERE e.deleted = 0
     AND UPPER(f.name) = 'INBOX';

DROP VIEW IF EXISTS unified_archive;
CREATE VIEW unified_archive AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.archive_folder;

DROP VIEW IF EXISTS unified_sent;
CREATE VIEW unified_sent AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.sent_folder;

DROP VIEW IF EXISTS unified_drafts;
CREATE VIEW unified_drafts AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.drafts_folder;

DROP VIEW IF EXISTS unified_trash;
CREATE VIEW unified_trash AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.trash_folder;

DROP VIEW IF EXISTS unified_spam;
CREATE VIEW unified_spam AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.spam_folder;

DROP VIEW IF EXISTS unified_starred;
CREATE VIEW unified_starred AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND e.flagged = 1
     AND f.name   != a.trash_folder
     AND f.name   != a.spam_folder;
