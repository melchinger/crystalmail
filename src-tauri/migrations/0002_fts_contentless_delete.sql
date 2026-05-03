-- FTS5 contentless tables (content='') reject DELETE and UPDATE by default.
-- Adding `contentless_delete=1` allows DELETE-by-rowid, which is what our
-- upsert and purge paths need. UPDATE is still not supported — callers must
-- DELETE + INSERT to refresh a row.
--
-- We drop and recreate the virtual table. Any existing FTS rows are lost,
-- but the next account sync repopulates them from `envelopes` via
-- `WriteCmd::UpsertEnvelope`.

DROP TABLE IF EXISTS fts_envelopes;

CREATE VIRTUAL TABLE fts_envelopes USING fts5 (
  subject,
  from_text,
  to_text,
  body_text,
  content='',
  contentless_delete=1
);
