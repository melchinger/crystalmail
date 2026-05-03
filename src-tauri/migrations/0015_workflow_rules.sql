-- Auto-trigger rules for workflows. Each row says "when a mail matches
-- these predicates, run workflow <workflow_id> in <mode>". Predicates
-- are AND-verknüpft inside one rule; multiple rules per workflow are
-- OR-verknüpft (any match fires).
--
-- Predicates are stored as a JSON array of tagged objects — same
-- shape as `Step` on the workflow itself. Closed Rust enum keeps the
-- schema in lockstep with the matcher: invent a predicate → add a
-- variant → the matcher function fails to compile until you handle
-- it. That's the point of the tagged enum.
--
-- Mode:
--   'auto'    — rule match triggers `apply_workflow` in the background,
--               no UI interruption. Result lands in an app log only.
--   'confirm' — rule match surfaces in a confirmation toast; user
--               decides whether to apply. Safer default for workflows
--               that touch the filesystem.
--
-- `account_id` nullable = "rule applies across accounts". Narrowing
-- to one account is the common case for "Steuerberater schickt mir
-- CSVs" — scoped to the private mail-box, not the work one.
CREATE TABLE workflow_rules (
  id              TEXT PRIMARY KEY,
  workflow_id     TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
  account_id      TEXT REFERENCES accounts(id) ON DELETE CASCADE,
  predicates_json TEXT NOT NULL,
  mode            TEXT NOT NULL CHECK (mode IN ('auto', 'confirm')),
  enabled         INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  created_at      TEXT NOT NULL,
  hit_count       INTEGER NOT NULL DEFAULT 0,
  last_hit_at     TEXT
);

CREATE INDEX idx_wf_rules_workflow ON workflow_rules(workflow_id);
CREATE INDEX idx_wf_rules_enabled  ON workflow_rules(enabled);
CREATE INDEX idx_wf_rules_account  ON workflow_rules(account_id);
