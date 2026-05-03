-- `$Junk` IMAP keyword (RFC 5788) tracking. Distinct from "this mail is
-- in the Spam folder" — a `$Junk`-flagged mail in the Inbox is the
-- "server missed it, the user corrected it" signal we need for the
-- filter-builder feature. The flag survives between clients (any IMAP
-- client that reads $Junk will see the user's curation).
ALTER TABLE envelopes
  ADD COLUMN junk INTEGER NOT NULL DEFAULT 0;
