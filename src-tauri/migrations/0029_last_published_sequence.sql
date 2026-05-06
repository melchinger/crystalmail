-- Phase 2.5.1: track per-commitment "last seen on IMAP" SEQUENCE.
--
-- ADR-0011 resolves cancellation as a writer-side operation (append a
-- new mutation with STATUS:CANCELLED, bump SEQUENCE). It does not say
-- what a reader should do when an active-folder message for a known UID
-- simply disappears — for example, when the user manually deletes the
-- ICS mail from a web mail client, or a server-side rule purges
-- something. Without local state tracking, a sync that sees "remote
-- doesn't have UID X, local does" cannot tell whether:
--   (a) we have a brand-new local commitment that needs initial publish
--   (b) we used to have it on the server, but the server just lost it
--
-- (a) wants to publish; (b) wants to acknowledge the deletion locally
-- without re-publishing (otherwise the user's manual delete is silently
-- undone, which is what one user reported on first smoke-test).
--
-- The fix: track the highest SEQUENCE we have ever seen for this UID
-- in the IMAP folder (`last_published_sequence`). NULL means "never
-- observed" (case a). A non-NULL value means "we know it was up there
-- with at least this sequence" (case b applies if local SEQUENCE has
-- not advanced past this value; otherwise the user has edited locally
-- and we should publish the edit, not the cancellation).
--
-- Set on every successful publish and every successful import. NULL
-- for existing Phase-1 rows on the first Phase-2 sync, which is the
-- correct treat-as-initial-publish behavior.

ALTER TABLE commitments
  ADD COLUMN last_published_sequence INTEGER;
