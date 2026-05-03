-- Training candidates for the pi-based workflow-rule learner.
-- When the user clicks "für Training markieren" on a mail, we add
-- the envelope id here — NOT as an IMAP flag. Keeping this local:
--   * IMAP doesn't have a semantic flag for "this is a workflow
--     example", and we'd rather not abuse \Flagged (user-visible as
--     the star icon).
--   * Training is a local, AI-feature bookkeeping — it has no
--     business leaving the laptop. No server-side round-trip means
--     the toggle is instant and offline-safe.
--
-- FK cascade: if the envelope gets deleted (user trashed it, or
-- the mail got expunged on the server), its training marker goes
-- with it — stale IDs can't poison later training runs.
CREATE TABLE workflow_training_candidates (
  envelope_id TEXT PRIMARY KEY REFERENCES envelopes(id) ON DELETE CASCADE,
  added_at    TEXT NOT NULL
);

CREATE INDEX idx_wf_training_added ON workflow_training_candidates(added_at);
