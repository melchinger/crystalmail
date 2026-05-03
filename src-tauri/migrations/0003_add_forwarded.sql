-- `$Forwarded` is a widely supported IMAP keyword (Thunderbird, Apple Mail,
-- Outlook, Gmail) used to mark a message whose user-facing counterpart has
-- been forwarded. Track it locally so the inbox list and reader can surface
-- the state without another round-trip.

ALTER TABLE envelopes
  ADD COLUMN forwarded INTEGER NOT NULL DEFAULT 0
  CHECK (forwarded IN (0, 1));
