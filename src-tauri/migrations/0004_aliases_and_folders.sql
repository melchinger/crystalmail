-- Per-account HTML signature (plain `signature` column kept as-is for
-- backwards compat; the UI treats signature_html as authoritative when set).
ALTER TABLE accounts ADD COLUMN signature_html TEXT;

-- Canonical IMAP folder paths per account. Most providers agree on "Sent",
-- "Drafts", "Trash"; user can override per-account. Archive stays in its
-- original column (already shipped in 0001).
ALTER TABLE accounts ADD COLUMN sent_folder   TEXT NOT NULL DEFAULT 'Sent';
ALTER TABLE accounts ADD COLUMN drafts_folder TEXT NOT NULL DEFAULT 'Drafts';
ALTER TABLE accounts ADD COLUMN trash_folder  TEXT NOT NULL DEFAULT 'Trash';

-- Aliases = additional "From" identities belonging to the same mailbox.
-- The main address stays on `accounts.address`; this table holds extras like
-- info@firma.tld, support@…, sales@…, each with its own display name.
CREATE TABLE account_aliases (
  id          TEXT PRIMARY KEY,
  account_id  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  email       TEXT NOT NULL,
  from_name   TEXT NOT NULL,
  UNIQUE (account_id, email)
);
CREATE INDEX idx_aliases_account ON account_aliases (account_id);

-- Rebuild unified_inbox: key on INBOX name directly rather than on the
-- exclusion of the archive folder (which missed when multiple folders had
-- colliding names and became confusing once more unified views were added).
DROP VIEW IF EXISTS unified_inbox;
CREATE VIEW unified_inbox AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
   WHERE e.deleted = 0
     AND UPPER(f.name) = 'INBOX';

-- New unified views, one per canonical folder. Each joins accounts so the
-- folder path can differ per account.
CREATE VIEW unified_archive AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.archive_folder;

CREATE VIEW unified_sent AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.sent_folder;

CREATE VIEW unified_drafts AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.drafts_folder;

CREATE VIEW unified_trash AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.trash_folder;
