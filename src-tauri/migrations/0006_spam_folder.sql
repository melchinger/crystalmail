-- Canonical spam/junk folder path per account. Keeps the same pattern as
-- archive/sent/drafts/trash — user-overridable, with a sensible default that
-- covers Dovecot + most mainstream providers. Gmail overrides this via the
-- auto-discovery probe (`\Junk` SPECIAL-USE attribute).
ALTER TABLE accounts ADD COLUMN spam_folder TEXT NOT NULL DEFAULT 'Spam';

-- Unified view mirroring the other four. Same shape so the frontend can swap
-- folder keys without caring about which one is which.
CREATE VIEW unified_spam AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND f.name = a.spam_folder;
