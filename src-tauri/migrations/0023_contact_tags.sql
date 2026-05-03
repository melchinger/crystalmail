-- Tags für Contacts. Kontrolliertes Vokabular: User legt Tags an
-- (entweder explizit über die UI oder implizit durch Vergabe an
-- einem Contact), pi schlägt beim Auto-Extract passende Tags aus
-- der bestehenden Liste vor.
--
-- Design-Entscheidungen:
--   * `name` ist UNIQUE und case-insensitive (COLLATE NOCASE).
--     "Kunde" und "kunde" sind dasselbe, sonst entstehen schnell
--     Duplikat-Tags.
--   * Optionale `color` für die UI-Chip-Hintergrundfarbe.
--   * Cascade-Delete: löscht der User einen Tag, fliegen die
--     contact_tags-Verknüpfungen automatisch mit. Der Contact
--     selbst bleibt natürlich.

CREATE TABLE tags (
  id          TEXT    PRIMARY KEY,            -- UUID
  name        TEXT    NOT NULL COLLATE NOCASE,
  color       TEXT,                            -- Hex-Wert oder NULL für Default
  created_at  TEXT    NOT NULL,
  UNIQUE (name COLLATE NOCASE)
);

CREATE INDEX tags_name_lower ON tags (lower(name));

CREATE TABLE contact_tags (
  contact_id  TEXT    NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
  tag_id      TEXT    NOT NULL REFERENCES tags(id)     ON DELETE CASCADE,
  PRIMARY KEY (contact_id, tag_id)
);

CREATE INDEX contact_tags_by_tag ON contact_tags (tag_id);
