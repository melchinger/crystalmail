-- Retroactive fix: subjects that were ingested before the IMAP
-- quoted-string unescape step carry literal `\"` / `\\` sequences. Undo
-- them here so existing inboxes display cleanly without a full re-sync.
--
-- Order matters: `\"` must be replaced *before* `\\` so that a legitimate
-- sequence like `\\\"` (one backslash followed by an escaped quote)
-- collapses to `\"` correctly.
--
-- SQL string literals in SQLite are raw (no C-style escapes). So '\"' is
-- the two bytes `\`, `"`; '\\' is the two bytes `\`, `\`. `instr` guards
-- the UPDATE against touching rows that don't need it.
UPDATE envelopes
   SET subject = replace(replace(subject, '\"', '"'), '\\', '\')
 WHERE instr(subject, '\') > 0;

-- from_json stores the first-seen address list as JSON. Per-name IMAP
-- escapes can smuggle through the same way subjects did, but because the
-- text is inside JSON the fix is not a safe blanket replace. Leave it
-- alone — the next sync over those envelopes will populate cleanly, and
-- the address strings on screen are usually generated from `email` when
-- the name is empty or mangled.
