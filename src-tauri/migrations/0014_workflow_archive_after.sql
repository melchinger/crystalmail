-- Per-workflow "archive on success" toggle. When set, a successful
-- workflow application (all_ok) is followed by an archive move of
-- the source message, same semantics as the `e` hotkey. Failed
-- workflows never archive — the user needs to see what broke first.
--
-- Default 0 keeps existing workflows' behaviour untouched: opt-in
-- only, you have to tick it in the editor.
ALTER TABLE workflows
  ADD COLUMN archive_after_success INTEGER NOT NULL DEFAULT 0
    CHECK (archive_after_success IN (0, 1));
