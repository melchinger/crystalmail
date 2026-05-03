-- Lifetime-Rules: Mails kriegen beim Sync ein Verfallsdatum verpasst,
-- der Sweeper räumt sie auf. Architektur ist bewusst parallel zu
-- spam_rules (Migration 0010) — gleiche Pattern-Felder, gleicher
-- "match-at-sync"-Hook, gleicher confidence/reason-Slot für eine
-- spätere pi-Vorschlags-Phase.
--
-- Warum eigene Tabelle statt spam_rules erweitern: Spam-Regeln liefern
-- ein binäres Urteil (ist/ist nicht Spam). Lifetime-Regeln tragen drei
-- zusätzliche Achsen (Frist + Aktion + Zielordner), und ihre Action
-- läuft asynchron vom Match — ein Sweeper, kein Sync-Hook. Die zwei
-- Lebenszyklen sauber zu trennen ist langfristig billiger als ein
-- generischer "rules"-Container, der beide Use-Cases abdecken müsste.
--
-- pattern_type-Werte (validiert in Rust beim Save):
--   from_email        — exakter Absender, case-insensitive
--   from_domain       — Domain nach @, case-insensitive
--   subject_contains  — Substring im Subject, case-insensitive
--   subject_regex     — regex::Regex an Subject
--
-- action-Werte:
--   archive  — message_ops::archive (in den Account-Archive-Ordner)
--   delete   — message_ops::delete  (in den Account-Trash)
--   move     — message_ops::move_to (Zielordner aus action_dest)
--
-- Bewusste NICHT-Aktionen:
--   * permanent_delete — zu unwiderruflich für Pattern-Match-Regeln.
--     Wer permanent löschen will, leert manuell den Trash.
--
-- dry_run = 1 lässt die Regel beim Sync zwar matchen (Envelope kriegt
-- expires_at), aber der Sweeper überspringt die Ausführung. Dafür ist
-- der Marker im Frontend trotzdem da → User kann Treffer beobachten,
-- ohne dass Mails verschwinden. Nach 1-2 Wochen Vertrauensbildung
-- kippt der User dry_run auf 0.
CREATE TABLE lifetime_rules (
  id           TEXT PRIMARY KEY,
  account_id   TEXT REFERENCES accounts(id) ON DELETE CASCADE,
  name         TEXT NOT NULL,
  pattern_type TEXT NOT NULL,
  pattern      TEXT NOT NULL,
  grace_days   INTEGER NOT NULL CHECK (grace_days >= 0),
  action       TEXT NOT NULL CHECK (action IN ('archive', 'delete', 'move')),
  action_dest  TEXT,
  enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  dry_run      INTEGER NOT NULL DEFAULT 1 CHECK (dry_run IN (0, 1)),
  confidence   REAL,
  reason       TEXT,
  created_at   TEXT NOT NULL,
  hit_count    INTEGER NOT NULL DEFAULT 0,
  last_hit_at  TEXT
);

CREATE INDEX idx_lifetime_rules_enabled ON lifetime_rules (enabled);
CREATE INDEX idx_lifetime_rules_account ON lifetime_rules (account_id);

-- Tagging der Envelopes. Vier Spalten zusammen reichen, damit der
-- Sweeper ohne Join auf lifetime_rules entscheiden kann (Regel könnte
-- inzwischen gelöscht sein — die Action der bereits-getaggten Mail
-- soll dann immer noch laufen, gemäß User-Entscheidung zum
-- Tagging-Zeitpunkt).
--
-- expires_at: ISO-8601 UTC. NULL = keine Lifetime-Regel angewendet.
-- expires_rule_name: Snapshot des Regel-Namens beim Tagging — wird im
--   Hover-Tooltip angezeigt ("Newsletter-Cleanup" archiviert in 4 Tagen).
--   Snapshot weil die Regel beim Sweep schon umbenannt sein könnte.
-- expires_dry_run: ebenfalls Snapshot — falls der User die Regel nach
--   Tagging auf live umschaltet, sollen bereits-getaggte Mails NICHT
--   plötzlich gelöscht werden, sondern bei der ursprünglichen Intention
--   bleiben. Anders herum genauso: Regel auf dry_run zurückkippen
--   während getaggte Mails ablaufen würde sonst Daten retten, die der
--   User glaubt schon weg zu haben — bessere Semantik: Snapshot.
ALTER TABLE envelopes ADD COLUMN expires_at TEXT;
ALTER TABLE envelopes ADD COLUMN expires_action TEXT;
ALTER TABLE envelopes ADD COLUMN expires_action_dest TEXT;
ALTER TABLE envelopes ADD COLUMN expires_rule_id TEXT;
ALTER TABLE envelopes ADD COLUMN expires_rule_name TEXT;
ALTER TABLE envelopes ADD COLUMN expires_dry_run INTEGER NOT NULL DEFAULT 0
  CHECK (expires_dry_run IN (0, 1));

-- Sweeper sucht über expires_at. Partial-Index: rows ohne Verfallsdatum
-- sind die große Mehrheit, kein Sinn die mit zu indizieren.
CREATE INDEX idx_envelopes_expires_at ON envelopes (expires_at)
  WHERE expires_at IS NOT NULL;

-- Lifecycle-Log: jede tatsächlich ausgeführte Sweep-Action wird hier
-- archiviert. Kein Vertrauen ohne Transparenz — Settings-Panel rendert
-- daraus "Regel X hat in den letzten 30 Tagen Y Mails behandelt".
--
-- Bewusst KEIN FK auf envelopes(id): die Quell-Envelope ist nach der
-- Action lokal weg. Wir konservieren stattdessen subject + sender
-- als snapshot, damit der User in der Audit-Liste sieht, was passiert
-- ist, auch wenn die Mail im Archive/Trash schon weiter weg ist.
--
-- result-Werte:
--   ok        — Action lief durch, Mail ist verschoben/gelöscht
--   skipped   — Skip-Bedingung griff (flagged/answered/in anderem Ordner)
--   failed    — Action warf einen Fehler (IMAP weg, etc.)
CREATE TABLE lifetime_actions_log (
  id              TEXT PRIMARY KEY,
  rule_id         TEXT,
  rule_name       TEXT NOT NULL,
  action          TEXT NOT NULL,
  action_dest     TEXT,
  message_id      TEXT NOT NULL,
  subject_snapshot TEXT NOT NULL,
  sender_snapshot TEXT NOT NULL,
  result          TEXT NOT NULL CHECK (result IN ('ok', 'skipped', 'failed')),
  error_message   TEXT,
  ran_at          TEXT NOT NULL
);

CREATE INDEX idx_lifetime_actions_log_rule ON lifetime_actions_log (rule_id, ran_at DESC);
CREATE INDEX idx_lifetime_actions_log_ran  ON lifetime_actions_log (ran_at DESC);

-- Unified-Views neu bauen. Hintergrund: SQLite friert bei
-- `CREATE VIEW … AS SELECT e.*` die Spaltenliste in der View-Definition
-- ein. Ein nachträgliches `ALTER TABLE envelopes ADD COLUMN` macht die
-- neue Spalte zwar in der Tabelle sichtbar, aber NICHT in den Views, die
-- per `*` darauf gebaut wurden. Beim ersten Read über `unified_inbox`
-- knallt es dann mit "no such column: e.expires_at".
--
-- Workaround: jede View einmal droppen und identisch wieder anlegen.
-- Die Definition wird unverändert übernommen — der Rebuild dient nur
-- dem Re-Snapshot der Spaltenliste. Das ist die Standard-SQLite-
-- Antwort auf das Problem (siehe https://www.sqlite.org/lang_altertable.html
-- Abschnitt zu Views).
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
