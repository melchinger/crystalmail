-- Per-account background-prefetch window. After each sync (and once at
-- app start) a best-effort worker pulls uncached bodies for envelopes
-- whose date lies within the last N days. 0 disables prefetch entirely
-- for accounts where the user wants manual control (or where the mailbox
-- is huge and bandwidth is precious).
--
-- Default 2 matches the "schlanke Inbox" workflow: two days is typically
-- enough that freshly-arrived mail reads instantly, without holding GB
-- of body blobs for a decade of archive.
ALTER TABLE accounts
  ADD COLUMN prefetch_days INTEGER NOT NULL DEFAULT 2;
