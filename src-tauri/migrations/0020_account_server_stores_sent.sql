-- Provider-Verhaltens-Flag: speichert der SMTP-Server eingehende
-- Submissions automatisch im IMAP-Sent-Ordner ab? Manche Anbieter
-- (Gmail, Office 365, Zoho.eu in der Praxis) tun das, andere nicht
-- (Zoho.com, Fastmail-Standard, die meisten selbst-gehosteten).
--
-- Wenn `server_stores_sent = 1`, skippt unser SMTP-Pfad die zusätzliche
-- IMAP-APPEND-Operation, sonst landet die gesendete Mail doppelt im
-- Sent-Ordner (eine Kopie vom Server, eine von uns).
--
-- Default 0: bei einer Migration bestehender Konten gehen wir auf
-- "kein Auto-Save" → APPEND läuft weiter wie bisher. Das ist das
-- pre-Fix-Verhalten und ändert daher nichts an existierenden Setups.
-- Frische Konten kriegen den Wert via Probe-Mail bei der Anlage gesetzt.

ALTER TABLE accounts
  ADD COLUMN server_stores_sent INTEGER NOT NULL DEFAULT 0
    CHECK (server_stores_sent IN (0, 1));
