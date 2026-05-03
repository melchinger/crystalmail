-- Track whether a mail carries non-inline attachments — drives the
-- paperclip indicator in the inbox list. Two write paths populate it:
--   1. Sync: a cheap heuristic from the top-level Content-Type header
--      (multipart/mixed → likely; everything else → no). We can't see
--      the inner parts from BODY.PEEK[HEADER] alone, but the
--      multipart-mixed container is the standard MIME wrapper for
--      "main content + attachments" and gets us close to right.
--   2. Body fetch: definitive — `attachments::parse_metas` walks the
--      decoded MIME tree and counts non-inline parts. Overrides the
--      sync heuristic on store_body.
--
-- Default 0 means existing rows show no clip until either path
-- updates them. The body-fetch path corrects within a prefetch cycle.
ALTER TABLE envelopes
  ADD COLUMN has_attachments INTEGER NOT NULL DEFAULT 0
  CHECK (has_attachments IN (0, 1));
