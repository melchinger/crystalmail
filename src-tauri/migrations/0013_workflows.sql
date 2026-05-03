-- Workflows: user-defined named action sequences that can be applied
-- to a message (Stage 1: manual) and later auto-triggered by rules
-- (Stage 2). Each workflow owns an ordered list of "steps" serialised
-- as JSON — closed enum on the Rust side, so schema migrations for a
-- new step type touch application code, not this table.
--
-- Layout notes:
--   * `hotkey` is optional: a single printable char or a +-joined combo
--     like "Ctrl+1". Uniqueness is enforced at the app layer, not here,
--     because conflicts across workflows are resolved via the same
--     Settings UI that resolves spam-rule/hotkey conflicts.
--   * `steps_json` is a JSON array of tagged objects; see
--     `domain::workflow::Step` for the schema. Migration of the step
--     schema is always backwards-compatible: new variants are simply
--     ignored by older rule code paths.
--   * No `account_id` scoping in Stage 1 — workflows are global to
--     the user. Per-account scoping can be added in a later migration
--     once Stage 2 (auto-trigger rules) lands and the need emerges.
CREATE TABLE workflows (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  hotkey      TEXT,                    -- NULL = no hotkey bound
  steps_json  TEXT NOT NULL,           -- JSON array of Step objects
  enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  created_at  TEXT NOT NULL,
  run_count   INTEGER NOT NULL DEFAULT 0,
  last_run_at TEXT
);

CREATE INDEX idx_workflows_enabled ON workflows (enabled);
