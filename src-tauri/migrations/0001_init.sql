-- CrystalMail initial schema.
-- Stored with embed via include_str!; applied by infrastructure::migrations
-- when the DB's user_version is less than the migration's index + 1.

-- Accounts are the root entity. `credential_entry` is a pointer into the OS
-- keyring (Win Credential Manager / macOS Keychain / Secret Service); the
-- actual password/OAuth token never lives in this DB.
CREATE TABLE accounts (
  id                TEXT    PRIMARY KEY,
  display_name      TEXT    NOT NULL,
  address           TEXT    NOT NULL UNIQUE,
  from_name         TEXT    NOT NULL,
  color             TEXT    NOT NULL,
  signature         TEXT,
  imap_host         TEXT    NOT NULL,
  imap_port         INTEGER NOT NULL,
  imap_tls          INTEGER NOT NULL CHECK (imap_tls IN (0, 1)),
  smtp_host         TEXT    NOT NULL,
  smtp_port         INTEGER NOT NULL,
  smtp_tls          INTEGER NOT NULL CHECK (smtp_tls IN (0, 1)),
  credential_kind   TEXT    NOT NULL CHECK (credential_kind IN ('password', 'oauth2')),
  credential_entry  TEXT    NOT NULL,
  archive_folder    TEXT    NOT NULL,
  created_at        TEXT    NOT NULL
);

-- One row per IMAP folder (mailbox) on the server. `uid_validity`/`uid_next`
-- are tracked for incremental sync. If `uid_validity` changes on the server,
-- all envelopes under the folder must be invalidated and re-fetched.
CREATE TABLE folders (
  id            TEXT    PRIMARY KEY,
  account_id    TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name          TEXT    NOT NULL,
  uid_validity  INTEGER NOT NULL DEFAULT 0,
  uid_next      INTEGER NOT NULL DEFAULT 0,
  last_sync_ts  TEXT,
  UNIQUE (account_id, name)
);

CREATE INDEX idx_folders_account ON folders (account_id);

-- Envelope = everything we can show in a list view without downloading the
-- body. `rowid` is implicit and used to join against `fts_envelopes`.
-- `thread_root` points to the envelope.id at the root of the JWZ-threaded
-- conversation (self-references allowed; NULL until threader has run).
CREATE TABLE envelopes (
  id                TEXT    PRIMARY KEY,
  account_id        TEXT    NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  folder_id         TEXT    NOT NULL REFERENCES folders(id)  ON DELETE CASCADE,
  imap_uid          INTEGER NOT NULL,
  message_id_header TEXT,
  subject           TEXT    NOT NULL DEFAULT '',
  date_utc          TEXT    NOT NULL,
  size_bytes        INTEGER NOT NULL DEFAULT 0,
  seen              INTEGER NOT NULL DEFAULT 0 CHECK (seen     IN (0, 1)),
  answered          INTEGER NOT NULL DEFAULT 0 CHECK (answered IN (0, 1)),
  flagged           INTEGER NOT NULL DEFAULT 0 CHECK (flagged  IN (0, 1)),
  draft             INTEGER NOT NULL DEFAULT 0 CHECK (draft    IN (0, 1)),
  deleted           INTEGER NOT NULL DEFAULT 0 CHECK (deleted  IN (0, 1)),
  from_json         TEXT    NOT NULL DEFAULT '[]',
  to_json           TEXT    NOT NULL DEFAULT '[]',
  cc_json           TEXT    NOT NULL DEFAULT '[]',
  references_json   TEXT    NOT NULL DEFAULT '[]',
  body_cached       INTEGER NOT NULL DEFAULT 0 CHECK (body_cached IN (0, 1)),
  thread_root       TEXT,
  UNIQUE (folder_id, imap_uid)
);

CREATE INDEX idx_envelopes_folder          ON envelopes (folder_id);
CREATE INDEX idx_envelopes_account_date    ON envelopes (account_id, date_utc DESC);
CREATE INDEX idx_envelopes_thread          ON envelopes (thread_root);
CREATE INDEX idx_envelopes_message_id      ON envelopes (message_id_header) WHERE message_id_header IS NOT NULL;

-- Bodies live in a separate table because AD #3 says they are lazy-loaded.
-- A missing row here means "not yet downloaded"; presence implies body_cached = 1.
CREATE TABLE bodies (
  envelope_id    TEXT  PRIMARY KEY REFERENCES envelopes(id) ON DELETE CASCADE,
  raw_rfc822     BLOB,
  plain_text     TEXT,
  html_text      TEXT,
  downloaded_at  TEXT  NOT NULL
);

-- Contentless FTS5 index. Writes happen explicitly from the sync pipeline in
-- the same transaction as the envelopes row — no triggers, so control of the
-- write path stays in Rust. Column order mirrors user expectations in a
-- search bar (subject first, then correspondents, then body).
CREATE VIRTUAL TABLE fts_envelopes USING fts5 (
  subject,
  from_text,
  to_text,
  body_text,
  content=''
);

-- Simple unified-inbox view. Archived/deleted are excluded; sorted newest first.
CREATE VIEW unified_inbox AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
   WHERE e.deleted = 0
     AND f.name   <> (SELECT archive_folder FROM accounts WHERE id = e.account_id);
