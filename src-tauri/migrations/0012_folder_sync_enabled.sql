-- Per-folder sync toggle. Default TRUE: every folder the IMAP server
-- advertises gets synced. User can opt out via account settings — the
-- sync pipeline then skips the folder in both the eager special-folder
-- pass and the lazy on-open fetch.
--
-- Why not handle this purely client-side? The sync runs on the Rust
-- side, has no idea which folders the user considers interesting, and
-- we want the opt-out persistent across sessions without round-tripping
-- to a JSON file.
ALTER TABLE folders
  ADD COLUMN sync_enabled INTEGER NOT NULL DEFAULT 1
    CHECK (sync_enabled IN (0, 1));
