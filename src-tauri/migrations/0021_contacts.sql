-- Adressbuch-Schema in zwei Schichten:
--
--  A. address_history — pure Recency+Frequency-Tabelle. Einziger Zweck:
--     Compose-Autocomplete. Auto-populiert aus jedem synchronisierten
--     Envelope (writer-actor side-effect). Eigene Account-Adressen
--     werden bewusst NICHT von dort befüllt (sonst schlägt der User
--     sich selbst als häufigste "Adresse" vor); werden aber durchaus
--     aufgenommen wenn der User explizit eine Mail an sich selbst
--     schickt (Compose-Send-Path filtert nicht).
--
--  B. contacts + contact_emails — kuratierte, strukturierte Personen
--     mit Adressdaten. Können manuell vom User angelegt werden ODER
--     per pi-Prompt aus einer Mail-Signatur extrahiert werden.
--     `extraction_misses` merkt sich Adressen die schon mal probiert
--     wurden, damit pi nicht jedes Mal beim Mail-Öffnen denselben
--     Body durchkaut.

-- ─── Layer A: Address History ──────────────────────────────────────────

CREATE TABLE address_history (
  email          TEXT    PRIMARY KEY,            -- lowercase, normalisiert
  display_name   TEXT,                           -- letzter gesehener Header-Name
  first_seen_at  TEXT    NOT NULL,               -- ISO8601 UTC
  last_seen_at   TEXT    NOT NULL,
  send_count     INTEGER NOT NULL DEFAULT 0,     -- Mails bei denen der User der Absender war
  recv_count     INTEGER NOT NULL DEFAULT 0,     -- Mails bei denen der User Empfänger war
  is_role        INTEGER NOT NULL DEFAULT 0      -- noreply/bounces/list-Heuristik → aus Autocomplete
                  CHECK (is_role IN (0, 1))
);

CREATE INDEX address_history_recency
  ON address_history (is_role, last_seen_at DESC);

-- ─── Layer B: Contacts ─────────────────────────────────────────────────

CREATE TABLE contacts (
  id             TEXT    PRIMARY KEY,            -- UUID
  display_name   TEXT    NOT NULL,
  organization   TEXT,
  job_title      TEXT,
  phone          TEXT,
  mobile         TEXT,
  street         TEXT,
  zip            TEXT,
  city           TEXT,
  country        TEXT,
  website        TEXT,
  notes          TEXT    NOT NULL DEFAULT '',
  -- 'user' = manuell angelegt; 'extracted' = pi-Auto-Extraction.
  origin         TEXT    NOT NULL DEFAULT 'user'
                  CHECK (origin IN ('user', 'extracted')),
  pinned         INTEGER NOT NULL DEFAULT 0
                  CHECK (pinned IN (0, 1)),
  -- Cache-Invalidation für Re-Extraction. Wenn eine neue Mail von
  -- einer der zugeordneten Adressen mit höherer envelope-Datum
  -- reinkommt, kann die UI ein Refresh anbieten. NULL = noch nie
  -- extrahiert (z.B. manuell angelegt).
  last_extracted_envelope_id TEXT,
  created_at     TEXT    NOT NULL,
  updated_at     TEXT    NOT NULL
);

CREATE INDEX contacts_pinned        ON contacts (pinned DESC, display_name COLLATE NOCASE);
CREATE INDEX contacts_display_name  ON contacts (display_name COLLATE NOCASE);

-- 1:N — ein Contact kann mehrere E-Mail-Adressen haben (privat + arbeit
-- + alte Domain). UNIQUE auf email weil die History 1:1 abgleicht und
-- ein Contact-Membership nicht doppelt sein darf.
CREATE TABLE contact_emails (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  contact_id  TEXT    NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
  email       TEXT    NOT NULL UNIQUE,
  is_primary  INTEGER NOT NULL DEFAULT 0
                CHECK (is_primary IN (0, 1))
);

CREATE INDEX contact_emails_by_contact ON contact_emails (contact_id);

-- "pi hat schon mal versucht aus dieser Adresse einen Kontakt zu
-- extrahieren — da war nichts brauchbares". Verhindert dass jeder
-- Mail-Open einen neuen pi-Call triggert. Wird invalidiert sobald
-- eine neuere Mail als die letzte versuchte reinkommt.
CREATE TABLE extraction_misses (
  email                      TEXT    PRIMARY KEY,
  last_attempted_envelope_id TEXT    NOT NULL,
  attempted_at               TEXT    NOT NULL
);

-- ─── FTS-Index über Contacts ───────────────────────────────────────────
CREATE VIRTUAL TABLE fts_contacts USING fts5 (
  contact_id    UNINDEXED,
  display_name,
  organization,
  job_title,
  phone,
  city,
  notes,
  tokenize = 'unicode61 remove_diacritics 2'
);

-- ─── Backfill: existierende Envelopes scannen ──────────────────────────
--
-- Pure SQL via json_each. Eigene Account-Adressen + Aliase werden via
-- NOT IN-Filter ausgeschlossen — sonst schlägt der User sich selbst als
-- häufigste Adresse vor.
--
-- Role-Heuristik via LIKE (SQLite hat kein eingebautes REGEXP). Konservativ
-- gehalten: nur unstrittig automatisierte Absender-Lokalparts. info@,
-- admin@, support@ landen normal in der History — sind oft echte
-- Personen-Postfächer.
--
-- WICHTIG: From-Pass markiert recv_count, weil "ich habe vom Absender
-- empfangen" → ich BIN der Empfänger. To/Cc-Pass ebenfalls recv_count
-- (der User stand mit drin). Der send_count wird im writer-side-effect
-- gepflegt wenn wir wissen dass die Mail OUTGOING war (envelope.folder
-- = sent_folder); Backfill setzt das hier nicht, weil's per JSON-Group
-- nur über einen Folder-Lookup pro Row ginge und die Cost steht in
-- keinem Verhältnis zum Nutzen — die echten send_counts kommen mit der
-- Zeit über den Writer dazu.

-- From-Pass.
INSERT INTO address_history (
  email, display_name, first_seen_at, last_seen_at, recv_count, is_role
)
SELECT
  lower(json_extract(addr.value, '$.email')),
  max(json_extract(addr.value, '$.name')),
  min(e.date_utc),
  max(e.date_utc),
  count(*),
  CASE
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'noreply@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'no-reply@%'        THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donotreply@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donot-reply@%'     THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'do-not-reply@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'mailer-daemon@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'postmaster@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounce@%'          THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounces@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE '%-bounces@%'       THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notification@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notifications@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'newsletter@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'reply+%@%'         THEN 1
    ELSE 0
  END
FROM envelopes e, json_each(e.from_json) addr
WHERE json_extract(addr.value, '$.email') IS NOT NULL
  AND json_extract(addr.value, '$.email') <> ''
  AND lower(json_extract(addr.value, '$.email')) NOT IN (
        SELECT lower(address) FROM accounts
        UNION ALL
        SELECT lower(email) FROM account_aliases
      )
GROUP BY lower(json_extract(addr.value, '$.email'))
ON CONFLICT(email) DO UPDATE SET
  display_name  = COALESCE(excluded.display_name, address_history.display_name),
  first_seen_at = MIN(address_history.first_seen_at, excluded.first_seen_at),
  last_seen_at  = MAX(address_history.last_seen_at, excluded.last_seen_at),
  recv_count    = address_history.recv_count + excluded.recv_count;

-- To-Pass (eigene Adressen sind hier dominant — die NOT-IN-Subquery
-- holt sie raus).
INSERT INTO address_history (
  email, display_name, first_seen_at, last_seen_at, recv_count, is_role
)
SELECT
  lower(json_extract(addr.value, '$.email')),
  max(json_extract(addr.value, '$.name')),
  min(e.date_utc),
  max(e.date_utc),
  count(*),
  CASE
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'noreply@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'no-reply@%'        THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donotreply@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donot-reply@%'     THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'do-not-reply@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'mailer-daemon@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'postmaster@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounce@%'          THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounces@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE '%-bounces@%'       THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notification@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notifications@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'newsletter@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'reply+%@%'         THEN 1
    ELSE 0
  END
FROM envelopes e, json_each(e.to_json) addr
WHERE json_extract(addr.value, '$.email') IS NOT NULL
  AND json_extract(addr.value, '$.email') <> ''
  AND lower(json_extract(addr.value, '$.email')) NOT IN (
        SELECT lower(address) FROM accounts
        UNION ALL
        SELECT lower(email) FROM account_aliases
      )
GROUP BY lower(json_extract(addr.value, '$.email'))
ON CONFLICT(email) DO UPDATE SET
  display_name  = COALESCE(excluded.display_name, address_history.display_name),
  first_seen_at = MIN(address_history.first_seen_at, excluded.first_seen_at),
  last_seen_at  = MAX(address_history.last_seen_at, excluded.last_seen_at),
  recv_count    = address_history.recv_count + excluded.recv_count;

-- Cc-Pass.
INSERT INTO address_history (
  email, display_name, first_seen_at, last_seen_at, recv_count, is_role
)
SELECT
  lower(json_extract(addr.value, '$.email')),
  max(json_extract(addr.value, '$.name')),
  min(e.date_utc),
  max(e.date_utc),
  count(*),
  CASE
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'noreply@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'no-reply@%'        THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donotreply@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donot-reply@%'     THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'do-not-reply@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'mailer-daemon@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'postmaster@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounce@%'          THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounces@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE '%-bounces@%'       THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notification@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notifications@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'newsletter@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'reply+%@%'         THEN 1
    ELSE 0
  END
FROM envelopes e, json_each(e.cc_json) addr
WHERE json_extract(addr.value, '$.email') IS NOT NULL
  AND json_extract(addr.value, '$.email') <> ''
  AND lower(json_extract(addr.value, '$.email')) NOT IN (
        SELECT lower(address) FROM accounts
        UNION ALL
        SELECT lower(email) FROM account_aliases
      )
GROUP BY lower(json_extract(addr.value, '$.email'))
ON CONFLICT(email) DO UPDATE SET
  display_name  = COALESCE(excluded.display_name, address_history.display_name),
  first_seen_at = MIN(address_history.first_seen_at, excluded.first_seen_at),
  last_seen_at  = MAX(address_history.last_seen_at, excluded.last_seen_at),
  recv_count    = address_history.recv_count + excluded.recv_count;
