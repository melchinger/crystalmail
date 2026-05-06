-- Phase 1.1: status column on commitments.
--
-- Per ADR-0011 §3 the calendar-state IMAP carriage profile carries a
-- VEVENT STATUS that is normally CONFIRMED but flips to CANCELLED when
-- the user removes a commitment. The ADR explicitly leaves cancellation
-- semantics open (Open Question 1); CrystalMail's interpretation, agreed
-- with the timeBank team, is "cancellation is just another mutation
-- that bumps SEQUENCE and sets STATUS:CANCELLED" — i.e. tombstone
-- rather than hard delete.
--
-- Storing the status locally keeps the SQLite row alive even after a
-- user-driven cancel, so when Phase 2 writes mutations into IMAP we can
-- emit the cancellation mail with the correct UID/SEQUENCE+1 instead
-- of having lost track. The list-view filters CANCELLED rows out by
-- default; a future "show cancelled" toggle can opt in.

ALTER TABLE commitments
  ADD COLUMN status TEXT NOT NULL DEFAULT 'CONFIRMED'
  CHECK (status IN ('CONFIRMED', 'CANCELLED', 'TENTATIVE'));

-- Range queries already use start_at; add a status-aware composite so
-- the common "give me confirmed events in this range" path stays a
-- single index lookup.
CREATE INDEX commitments_status_start_at
  ON commitments (status, start_at);
