-- Per-account Sync-Modus: ob das Konto eine persistente IMAP-Verbindung
-- mit IDLE führt (live Push-Notifications), einen periodischen Polling-
-- Timer im Backend, oder beides parallel.
--
-- Default 'idle': frisch angelegte Accounts kriegen automatisch den
-- empfohlenen Modus, bestehende Accounts werden bei der Migration
-- ebenfalls auf 'idle' gesetzt. Wenn ein Provider IDLE nicht beherrscht
-- oder die Verbindung zickt, kann der User pro Konto auf 'polling' oder
-- 'idle_and_polling' umstellen.

ALTER TABLE accounts
  ADD COLUMN sync_mode TEXT NOT NULL DEFAULT 'idle'
    CHECK (sync_mode IN ('idle', 'polling', 'idle_and_polling'));
