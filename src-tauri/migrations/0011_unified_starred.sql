-- Cross-folder "starred" view. Unlike the other unified folders, this
-- one isn't anchored to a folder-name; it's a *flag filter* — every
-- envelope with `flagged = 1`, across all accounts, across all folders
-- except the two we explicitly don't want to surface:
--
--   * Trash — if you starred something before binning it, you probably
--     don't want it popping back up in "markiert".
--   * Spam — same rationale; once classified, it stays out of sight.
--
-- Archive, Sent, Drafts all remain visible: the whole point of starring
-- is "this matters across states, pull it up regardless of where it
-- ended up".
CREATE VIEW unified_starred AS
  SELECT e.*
    FROM envelopes e
    JOIN folders   f ON f.id = e.folder_id
    JOIN accounts  a ON a.id = e.account_id
   WHERE e.deleted = 0
     AND e.flagged = 1
     AND f.name   != a.trash_folder
     AND f.name   != a.spam_folder;
