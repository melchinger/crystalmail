-- Spam-filter rule set. Each row is one deterministic predicate that gets
-- matched against envelope features (sender, subject, body preview) during
-- the rule-engine pass. Rules are *user-owned* — pi proposes, the user
-- confirms, and from then on it's just regex.
--
-- Scope choice: rules can be per-account (account_id set) or global
-- (account_id NULL). `from_domain = "promo.xy.com"` typically global;
-- but if the user has a private and a work account and the pattern only
-- makes sense in the work context, they can scope it.
--
-- `pattern_type` is a closed enum validated in Rust at save time:
--   from_email       — full sender address, case-insensitive exact
--   from_domain      — domain portion of sender, case-insensitive exact
--   subject_contains — substring match in subject, case-insensitive
--   subject_regex    — regex::Regex::new() validated at save, applied to subject
--   body_contains    — substring match in plain-text body preview (first 500 chars)
--
-- `enabled` is the runtime toggle — disabled rules sit dormant for future use
-- without losing the history.
CREATE TABLE spam_rules (
  id           TEXT PRIMARY KEY,
  account_id   TEXT REFERENCES accounts(id) ON DELETE CASCADE,
  pattern_type TEXT NOT NULL,
  pattern      TEXT NOT NULL,
  enabled      INTEGER NOT NULL DEFAULT 1,
  confidence   REAL,              -- pi's estimate (0..1); NULL for hand-written rules
  reason       TEXT,              -- human-readable rationale, shown in UI tooltip
  created_at   TEXT NOT NULL,
  hit_count    INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_spam_rules_enabled ON spam_rules (enabled);
CREATE INDEX idx_spam_rules_account ON spam_rules (account_id);
