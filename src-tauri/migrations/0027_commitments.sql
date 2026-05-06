-- Phase 1 calendar: locally stored commitments.
--
-- Mirrors timeProtocol v0.1-MVP-Profile §1.3 commitment shape (3 core
-- objects: availability/hold/commitment — Phase 1 only stores commitment;
-- availability is offered-time-for-others, hold is internal/transient,
-- both belong to negotiation = Phase 3).
--
-- Identifier model:
--   * `id`  = local UUID, stable for the row's lifetime
--   * `uid` = RFC 5545 UID, stable across REPLY/UPDATE cycles, may collide
--             with foreign calendars when imported (UNIQUE here so that a
--             second import of the same invitation upserts in place rather
--             than producing duplicate rows)
--
-- Timestamps stored as RFC 3339 strings with explicit offset. The MVP
-- profile mandates this shape on the wire; we keep the same shape locally
-- so import/export are byte-identical for offset-anchored events.
--
-- `source_message_id` is intentionally NOT a foreign key to messages.id —
-- the user may delete the source mail later, and the commitment should
-- survive. The pointer is informational ("imported from this mail, click
-- to reopen") and a left-join at read time gracefully handles the missing
-- target.

CREATE TABLE commitments (
  id                 TEXT    PRIMARY KEY,           -- UUID
  uid                TEXT    NOT NULL UNIQUE,        -- RFC 5545 UID
  sequence           INTEGER NOT NULL DEFAULT 0,
  summary            TEXT,
  description        TEXT,
  location           TEXT,
  start_at           TEXT    NOT NULL,               -- RFC 3339 + offset
  end_at             TEXT    NOT NULL,               -- RFC 3339 + offset
  original_tzid      TEXT,                           -- e.g. "Europe/Berlin"
  organizer_email    TEXT,
  organizer_name     TEXT,
  source             TEXT    NOT NULL
                     CHECK (source IN ('manual', 'ics_import', 'negotiation')),
  source_message_id  TEXT,
  created_at         TEXT    NOT NULL,               -- ISO 8601 UTC
  updated_at         TEXT    NOT NULL                -- ISO 8601 UTC
);

-- Range queries ("events between X and Y") are the dominant access path
-- for the Calendar list view.
CREATE INDEX commitments_start_at ON commitments (start_at);

-- Imported events use UID as the natural key on re-import; UNIQUE constraint
-- already covers lookup, this is just to remind future readers.
-- (No additional index needed — the UNIQUE on uid creates one implicitly.)

-- Attendee rows. ON DELETE CASCADE so removing a commitment cleans up
-- attendee rows in one statement.
CREATE TABLE commitment_attendees (
  commitment_id  TEXT    NOT NULL REFERENCES commitments(id) ON DELETE CASCADE,
  email          TEXT    NOT NULL,
  display_name   TEXT,
  -- RFC 5545 PARTSTAT: NEEDS-ACTION, ACCEPTED, DECLINED, TENTATIVE,
  -- DELEGATED, COMPLETED, IN-PROCESS. NULL for attendees we have no
  -- status for (e.g. invitees of a manual event we created).
  partstat       TEXT,
  PRIMARY KEY (commitment_id, email)
);

CREATE INDEX commitment_attendees_by_commitment
  ON commitment_attendees (commitment_id);
