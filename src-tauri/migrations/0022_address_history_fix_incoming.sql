-- Korrektur des Backfills aus 0021. Ursprüngliche Annahme war, dass bei
-- eingehenden Mails auch die Co-Empfänger (To/Cc) ins Adressbuch sollen.
-- Falsch — wenn jemand uns auf einen 50-Empfänger-Verteiler cc'd, sind
-- das alles Leute, an die der User nie aktiv geschrieben hat. Die würden
-- das Compose-Autocomplete fluten.
--
-- Fix: nur der **Absender** eingehender Mails landet im Adressbuch.
-- Bei AUSGEHENDEN Mails (Folder == account.sent_folder) sind To+Cc die
-- vom User aktiv ausgewählten Empfänger und kommen wie bisher rein.
--
-- Strategie: address_history komplett platt machen und neu befüllen.
-- Die Tabelle ist 100% derived data (rebuilt aus envelopes), Verlust
-- ist unkritisch. Live-Side-Effect in db_ops::record_address_history
-- wurde parallel angepasst.

DELETE FROM address_history;

-- ─── From-Pass: alle Envelopes ─────────────────────────────────────────
-- Aus eingehenden Mails kommt der echte Absender; bei ausgehenden Mails
-- wäre From = eigene Adresse → wird vom NOT-IN-Filter eh ausgeschlossen.
INSERT INTO address_history (
  email, display_name, first_seen_at, last_seen_at, recv_count, is_role
)
SELECT
  lower(json_extract(addr.value, '$.email')),
  max(json_extract(addr.value, '$.name')),
  min(e.date_utc),
  max(e.date_utc),
  count(*),
  CASE
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'noreply@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'no-reply@%'        THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donotreply@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donot-reply@%'     THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'do-not-reply@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'mailer-daemon@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'postmaster@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounce@%'          THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounces@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE '%-bounces@%'       THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notification@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notifications@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'newsletter@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'reply+%@%'         THEN 1
    ELSE 0
  END
FROM envelopes e, json_each(e.from_json) addr
WHERE json_extract(addr.value, '$.email') IS NOT NULL
  AND json_extract(addr.value, '$.email') <> ''
  AND lower(json_extract(addr.value, '$.email')) NOT IN (
        SELECT lower(address) FROM accounts
        UNION ALL
        SELECT lower(email) FROM account_aliases
      )
GROUP BY lower(json_extract(addr.value, '$.email'))
ON CONFLICT(email) DO UPDATE SET
  display_name  = COALESCE(excluded.display_name, address_history.display_name),
  first_seen_at = MIN(address_history.first_seen_at, excluded.first_seen_at),
  last_seen_at  = MAX(address_history.last_seen_at, excluded.last_seen_at),
  recv_count    = address_history.recv_count + excluded.recv_count;

-- ─── To-Pass: NUR ausgehende Envelopes ─────────────────────────────────
-- Outgoing-Detection: folder.name == account.sent_folder. Nutzen
-- send_count statt recv_count, weil User aktiv adressiert hat.
INSERT INTO address_history (
  email, display_name, first_seen_at, last_seen_at, send_count, recv_count, is_role
)
SELECT
  lower(json_extract(addr.value, '$.email')),
  max(json_extract(addr.value, '$.name')),
  min(e.date_utc),
  max(e.date_utc),
  count(*),
  0,
  CASE
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'noreply@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'no-reply@%'        THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donotreply@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donot-reply@%'     THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'do-not-reply@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'mailer-daemon@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'postmaster@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounce@%'          THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounces@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE '%-bounces@%'       THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notification@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notifications@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'newsletter@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'reply+%@%'         THEN 1
    ELSE 0
  END
FROM envelopes e, json_each(e.to_json) addr
WHERE json_extract(addr.value, '$.email') IS NOT NULL
  AND json_extract(addr.value, '$.email') <> ''
  AND lower(json_extract(addr.value, '$.email')) NOT IN (
        SELECT lower(address) FROM accounts
        UNION ALL
        SELECT lower(email) FROM account_aliases
      )
  AND EXISTS (
        SELECT 1 FROM folders f
        JOIN accounts a ON a.id = f.account_id
        WHERE f.id = e.folder_id
          AND f.name = a.sent_folder
          AND a.sent_folder <> ''
      )
GROUP BY lower(json_extract(addr.value, '$.email'))
ON CONFLICT(email) DO UPDATE SET
  display_name  = COALESCE(excluded.display_name, address_history.display_name),
  first_seen_at = MIN(address_history.first_seen_at, excluded.first_seen_at),
  last_seen_at  = MAX(address_history.last_seen_at, excluded.last_seen_at),
  send_count    = address_history.send_count + excluded.send_count;

-- ─── Cc-Pass: NUR ausgehende Envelopes ─────────────────────────────────
INSERT INTO address_history (
  email, display_name, first_seen_at, last_seen_at, send_count, recv_count, is_role
)
SELECT
  lower(json_extract(addr.value, '$.email')),
  max(json_extract(addr.value, '$.name')),
  min(e.date_utc),
  max(e.date_utc),
  count(*),
  0,
  CASE
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'noreply@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'no-reply@%'        THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donotreply@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'donot-reply@%'     THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'do-not-reply@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'mailer-daemon@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'postmaster@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounce@%'          THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'bounces@%'         THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE '%-bounces@%'       THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notification@%'    THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'notifications@%'   THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'newsletter@%'      THEN 1
    WHEN lower(json_extract(addr.value, '$.email')) LIKE 'reply+%@%'         THEN 1
    ELSE 0
  END
FROM envelopes e, json_each(e.cc_json) addr
WHERE json_extract(addr.value, '$.email') IS NOT NULL
  AND json_extract(addr.value, '$.email') <> ''
  AND lower(json_extract(addr.value, '$.email')) NOT IN (
        SELECT lower(address) FROM accounts
        UNION ALL
        SELECT lower(email) FROM account_aliases
      )
  AND EXISTS (
        SELECT 1 FROM folders f
        JOIN accounts a ON a.id = f.account_id
        WHERE f.id = e.folder_id
          AND f.name = a.sent_folder
          AND a.sent_folder <> ''
      )
GROUP BY lower(json_extract(addr.value, '$.email'))
ON CONFLICT(email) DO UPDATE SET
  display_name  = COALESCE(excluded.display_name, address_history.display_name),
  first_seen_at = MIN(address_history.first_seen_at, excluded.first_seen_at),
  last_seen_at  = MAX(address_history.last_seen_at, excluded.last_seen_at),
  send_count    = address_history.send_count + excluded.send_count;
