-- RRULE expansion (Calendar Phase 3+): when an imported ICS event carries
-- a recurrence rule, we expand it on import into individual `commitments`
-- rows (Option A from the design discussion). Each occurrence gets a
-- synthetic UID `${masterUid}@${dtstartIso}` so the row is independently
-- addressable, and `series_uid` points back at the master's UID so we can
-- a) cascade-cancel the whole series, b) keep the IMAP-publish path from
-- spamming the calendar folder with one mail per occurrence.
--
-- NULL on this column means "stand-alone event" — both manually created
-- and singleton-imported events.

ALTER TABLE commitments ADD COLUMN series_uid TEXT NULL;

CREATE INDEX IF NOT EXISTS idx_commitments_series_uid
    ON commitments(series_uid)
    WHERE series_uid IS NOT NULL;
