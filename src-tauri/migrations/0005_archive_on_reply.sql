-- Per-account workflow toggle: when the user replies to a message from this
-- account's inbox, automatically move the parent to the configured archive
-- folder after the SMTP send succeeds + the \Answered flag is set.
--
-- Defaults to OFF so behavior doesn't change for existing accounts on upgrade.
ALTER TABLE accounts
  ADD COLUMN archive_on_reply INTEGER NOT NULL DEFAULT 0;
