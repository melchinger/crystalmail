-- Verzögerung jetzt minuten-genau statt tage-genau. Damit deckt die
-- Regel-Engine auch "in 10 Minuten ins Archiv" ab — Newsletter, die
-- der User in dem Zeitfenster nicht angefasst hat, gelten als „heute
-- nicht relevant" und wandern weg.
--
-- Bestandsdaten: Wert × 1440 (Minuten/Tag) → semantisch identisch.
-- SQLite kennt kein RENAME COLUMN mit Konversion in einem Schritt; wir
-- legen die neue Spalte an, kopieren Werte rüber, droppen die alte.
-- Beide CHECKs (>= 0) bleiben.

-- 1) Neue Spalte
ALTER TABLE workflow_rules ADD COLUMN delay_minutes INTEGER NOT NULL DEFAULT 0
  CHECK (delay_minutes >= 0);

-- 2) Bestehende Werte umrechnen. NULLs gibt es nicht (CHECK + DEFAULT 0
--    auf der alten Spalte) — der Faktor zieht für jede Row.
UPDATE workflow_rules SET delay_minutes = delay_days * 1440;

-- 3) Alte Spalte abreißen.
ALTER TABLE workflow_rules DROP COLUMN delay_days;
