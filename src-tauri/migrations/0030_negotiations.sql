-- Phase 3 calendar negotiation: persistent state for in-flight
-- timeProtocol-envelope exchanges per spec/v0.1.md §5 + the v0.1-MVP
-- profile.
--
-- Three tables for one bounded context — keeps the per-row state small
-- enough that lookups stay simple:
--
--   1. `negotiations` — one row per logical negotiation thread
--      (identified by `negotiation_id` = `node_id:local_id`). Tracks
--      the high-level state machine plus the constraints that drove
--      the original request. Multi-slot semantics live in the child
--      tables, not here.
--
--   2. `negotiation_slots` — one row per proposed time slot in a
--      thread. The MVP profile allows multiple parallel slots in
--      one negotiation; only one slot can become `confirmed`, the
--      rest go `inactive`. `slot_id` is the proposer-owned compound
--      identifier per ADR-0011 §2.
--
--   3. `negotiation_messages` — append-only log of every envelope
--      received or sent in the thread. Idempotency for incoming
--      messages keys off `message_id` (UNIQUE constraint catches
--      dupes per spec §7.1). The full envelope JSON survives so a
--      future debug-view can replay the thread end-to-end without
--      having to reconstruct from state.
--
-- Per-thread purge cascades: deleting a row from `negotiations`
-- cleans both child tables via the FK. We don't expose a "delete
-- thread" command in Phase 3 v1; the rows live forever for now and
-- compaction is post-MVP.

CREATE TABLE negotiations (
  id                       TEXT    PRIMARY KEY,        -- our local UUID
  -- Spec identifier: `<node_id>:<local_id>`. Unique across all our
  -- threads — cross-party collisions are theoretically possible but
  -- the initiating-node scoping makes it practically a non-issue.
  negotiation_id           TEXT    NOT NULL UNIQUE,
  -- Did *we* initiate the thread or are we responding to someone
  -- else's request? Drives the UI: an initiator sees "waiting for
  -- proposals" after sending a request, a responder sees "Alice
  -- needs you to propose slots".
  thread_role              TEXT    NOT NULL
                           CHECK (thread_role IN ('initiator', 'responder')),
  state                    TEXT    NOT NULL
                           CHECK (state IN (
                             'requested', 'proposed', 'held',
                             'confirmed', 'released', 'expired'
                           )),
  -- ISO 8601 duration (`PT45M`, `PT1H30M`). Carried verbatim from the
  -- request's payload — no parsing here, we let the UI render.
  duration_iso             TEXT,
  -- Constraints object from the original request, JSON-serialised:
  -- `{ "latest": "...", "preferred_time": "morning",
  --    "minimum_notice": "PT2H" }`.
  constraints_json         TEXT,
  -- The other party. For Phase 3 v1 `node_id` = email address per
  -- our convention (see ADR-0011 §8 and the spec's "implementations
  -- must configure endpoints out-of-band").
  counterparty_email       TEXT    NOT NULL,
  counterparty_name        TEXT,
  -- When state transitions to `confirmed`, link to the materialized
  -- commitment row in `commitments`. Not an FK because we want the
  -- negotiation history to outlive a hypothetical commitment-purge.
  confirmed_commitment_id  TEXT,
  -- Subject extracted from the first inbound mail of the thread, or
  -- "Termin mit X" for outbound-initiated threads. Purely UI-purpose;
  -- not part of the protocol.
  display_summary          TEXT,
  created_at               TEXT    NOT NULL,           -- ISO 8601 UTC
  updated_at               TEXT    NOT NULL
);

CREATE INDEX negotiations_state ON negotiations (state);
CREATE INDEX negotiations_counterparty ON negotiations (counterparty_email);
CREATE INDEX negotiations_updated_at ON negotiations (updated_at DESC);

CREATE TABLE negotiation_slots (
  id                  INTEGER PRIMARY KEY AUTOINCREMENT,
  negotiation_id      TEXT    NOT NULL REFERENCES negotiations(negotiation_id) ON DELETE CASCADE,
  -- Proposer-owned `<node_id>:<local_id>`. UNIQUE per negotiation —
  -- two `propose`-messages with the same `slot_id` from the same
  -- proposer is a duplicate (idempotency through the messages table)
  -- but two different slots from different proposers in the same
  -- thread is fine.
  slot_id             TEXT    NOT NULL,
  proposer_node_id    TEXT    NOT NULL,
  -- RFC 3339 with offset, same shape as `commitments.start_at`.
  start_at            TEXT    NOT NULL,
  end_at              TEXT    NOT NULL,
  status              TEXT    NOT NULL DEFAULT 'active'
                      CHECK (status IN ('active', 'inactive', 'confirmed', 'released')),
  proposed_at         TEXT    NOT NULL,                -- ISO 8601 UTC
  UNIQUE (negotiation_id, slot_id)
);

CREATE INDEX negotiation_slots_by_negotiation
  ON negotiation_slots (negotiation_id);

CREATE TABLE negotiation_messages (
  id                  INTEGER PRIMARY KEY AUTOINCREMENT,
  negotiation_id      TEXT    NOT NULL REFERENCES negotiations(negotiation_id) ON DELETE CASCADE,
  -- Envelope-owned `<node_id>:<local_id>`. UNIQUE globally — receivers
  -- treat duplicate message_ids as already-processed (spec §7.1
  -- "Duplicate envelopes must not create additional state transitions").
  message_id          TEXT    NOT NULL UNIQUE,
  direction           TEXT    NOT NULL
                      CHECK (direction IN ('inbound', 'outbound')),
  action              TEXT    NOT NULL
                      CHECK (action IN (
                        'request', 'propose', 'counter_propose',
                        'confirm', 'release', 'hold'
                      )),
  -- Full envelope JSON for audit / replay. Small enough (typically
  -- < 1 KB) that storing verbatim is cheaper than re-deriving.
  envelope_json       TEXT    NOT NULL,
  -- For inbound messages: link to the `messages.id` of the mail
  -- the envelope arrived in, so the UI can deep-link "view source
  -- mail". NULL for outbound (we sent it; the IMAP-APPEND-to-Sent
  -- copy is referenced via a different path).
  source_message_id   TEXT,
  received_at         TEXT    NOT NULL                 -- ISO 8601 UTC
);

CREATE INDEX negotiation_messages_by_negotiation
  ON negotiation_messages (negotiation_id, received_at);
