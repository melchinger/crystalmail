// Storage layer: one shared SQLite DB, encrypted at rest via SQLCipher.
//
// Concurrency model (see plan AD #6):
//   * Account-Actors never touch the DB directly.
//   * They send `WriteCmd`s via a `tokio::mpsc` to a single `db_writer` task,
//     which owns the write connection and batches commands into transactions.
//   * UI reads acquire a connection from an r2d2 read-only pool. WAL mode
//     guarantees readers don't block the writer and vice versa.
//
// Encryption model:
//   * 256-bit master key in OS keyring (`crystalmail::db_master`).
//   * Generated once on first launch via the OS RNG; never written to disk.
//   * `PRAGMA key = "x'...'"` raw-key form on every connection open
//     (skips SQLCipher's PBKDF — we already have uniformly-random bytes).
//   * `PRAGMA cipher_compatibility = 4` pins the format so future SQLCipher
//     upgrades don't silently rewrite the file.
//
// First-run migration of an existing plaintext DB (see `migrate_plaintext_to_encrypted`):
// detect by trying a key-less open + `SELECT 1 FROM sqlite_master`; if that
// works, the file is plaintext, and we use SQLCipher's `sqlcipher_export`
// to copy everything into a fresh encrypted file, then atomically swap.
// The original plaintext file is renamed to `*.plaintext.bak` as a safety
// net — the user can delete it after a successful first-encrypted run.

use std::path::Path;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use tokio::sync::{mpsc, oneshot};

use crate::domain::account::{Account, AccountAlias, AccountId};
use crate::domain::folder::FolderId;
use crate::domain::message::{Envelope, Flags, MessageId};

use super::db_ops::*;
use super::migrations;

pub type ReadPool = Pool<SqliteConnectionManager>;

#[derive(Debug)]
pub enum WriteCmd {
    AddAccount {
        account: Account,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Full replacement of the non-identifier columns. Credential entry is
    /// kept in sync because keyring entries embed the id and don't rotate.
    UpdateAccount {
        account: Account,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteAccount {
        id: AccountId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Look up a folder by (account_id, name); insert a fresh row if none
    /// exists. Returns the `FolderId` the sync pipeline can use for envelopes.
    EnsureFolder {
        account_id: AccountId,
        name: String,
        ack: oneshot::Sender<Result<FolderId, DbError>>,
    },
    /// Remove a folder row and every envelope / body / FTS row that
    /// hangs off it. Called after a successful IMAP DELETE so the UI
    /// reflects the server state immediately — no stale sidebar entry.
    /// FK CASCADE handles envelope + body rows; FTS5 isn't FK-aware,
    /// so we clean that explicitly in the same transaction.
    DeleteFolderTree {
        folder_id: FolderId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Update uid_validity / uid_next / last_sync_ts for an existing folder
    /// row. Called after SELECT reveals the server-side values.
    UpdateFolderSyncState {
        folder_id: FolderId,
        uid_validity: u32,
        uid_next: u32,
        last_sync_ts: chrono::DateTime<chrono::Utc>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Flip the per-folder `sync_enabled` toggle (Phase 1 opt-out).
    /// Both the eager and lazy sync paths consult this flag.
    SetFolderSyncEnabled {
        folder_id: FolderId,
        enabled: bool,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    UpsertEnvelope {
        envelope: Envelope,
        /// Optional extracted plain text fed into FTS5 together with the envelope.
        /// If `None`, the FTS row is inserted with empty body_text (body_cached=0).
        body_text: Option<String>,
        /// Ack carries `true` when this upsert created a brand-new row,
        /// `false` when it updated an existing one (re-sync of a UID we
        /// already have). The sync loop uses this to count "actually new
        /// mail" — distinct from `fetched`, which double-counts known
        /// envelopes that fall inside the SINCE-30d window.
        ack: oneshot::Sender<Result<bool, DbError>>,
    },
    UpdateFlags {
        message_id: MessageId,
        flags: Flags,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    StoreBody {
        message_id: MessageId,
        raw_rfc822: Vec<u8>,
        plain_text: Option<String>,
        html_text: Option<String>,
        /// Authoritative attachment indicator derived from
        /// `attachments::parse_metas` after parsing the freshly-decoded
        /// MIME tree. Replaces whatever sync-time heuristic landed in the
        /// envelopes row earlier.
        has_attachments: bool,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteEnvelopes {
        folder_id: FolderId,
        imap_uids: Vec<u32>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Replace all aliases for an account with the given list (atomic — the
    /// writer deletes existing rows and inserts the new set inside one
    /// transaction). Empty list clears aliases.
    ReplaceAliases {
        account_id: AccountId,
        aliases: Vec<AccountAlias>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    InsertSpamRule {
        rule: crate::domain::spam_rule::SpamRule,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    SetSpamRuleEnabled {
        rule_id: crate::domain::spam_rule::SpamRuleId,
        enabled: bool,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteSpamRule {
        rule_id: crate::domain::spam_rule::SpamRuleId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    IncrementSpamRuleHits {
        rule_id: crate::domain::spam_rule::SpamRuleId,
        delta: i64,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    InsertWorkflow {
        workflow: crate::domain::workflow::Workflow,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    UpdateWorkflow {
        workflow: crate::domain::workflow::Workflow,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteWorkflow {
        workflow_id: crate::domain::workflow::WorkflowId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Bump `run_count` and set `last_run_at = now`. Called after a
    /// successful apply; failures don't record (keeps the counter an
    /// honest "how often did this actually help" metric).
    RecordWorkflowRun {
        workflow_id: crate::domain::workflow::WorkflowId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    InsertWorkflowRule {
        rule: crate::domain::workflow::WorkflowRule,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    UpdateWorkflowRule {
        rule: crate::domain::workflow::WorkflowRule,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteWorkflowRule {
        rule_id: crate::domain::workflow::WorkflowRuleId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    SetWorkflowRuleEnabled {
        rule_id: crate::domain::workflow::WorkflowRuleId,
        enabled: bool,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Bump hit_count and update last_hit_at. Called after the matcher
    /// actually fires a rule (in auto mode: after apply; in confirm
    /// mode: *not* counted until the user confirms — we only count
    /// "real" hits, not proposals).
    IncrementWorkflowRuleHit {
        rule_id: crate::domain::workflow::WorkflowRuleId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Mark a message as a training candidate for the pi-based rule
    /// learner. Idempotent: re-marking an existing candidate is a
    /// no-op (INSERT OR IGNORE).
    AddWorkflowTraining {
        message_ids: Vec<MessageId>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    RemoveWorkflowTraining {
        message_ids: Vec<MessageId>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    ClearWorkflowTraining {
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Atomarer Settings-Import: alle Accounts, Aliase, Spam-Regeln,
    /// Workflows und Workflow-Rules in einer einzigen Transaktion. Bei
    /// einem Fehler an irgendeiner Stelle Rollback der gesamten Operation,
    /// sodass der User keinen halb-importierten Zustand erbt. Die zugehörigen
    /// Keyring- und JSON-Sidecar-Schreibvorgänge laufen außerhalb des Writers
    /// (siehe `application::backup::apply`).
    ImportBundle {
        plan: crate::application::backup::ImportPlan,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// No-op-Probe für die Liveness-Prüfung des Writer-Actors. Schreibt
    /// nichts in die DB — `db_ping` will nur wissen, ob die Channel/Task
    /// noch leben. Vorgänger-Implementierung hat dafür einen kaputten
    /// `UpsertFolder` mit Nil-Account-FK abgesetzt; das ergab eine fehl-
    /// geschlagene Schreib-Transaktion pro Liveness-Tick und unnötiges
    /// Tracing-Rauschen.
    Ping {
        ack: oneshot::Sender<Result<(), DbError>>,
    },

    // ─── Workflow-Rule-Scheduling (v2) ─────────────────────────────────
    /// Per-Rule Trockenmodus-Toggle. Eigene Variante (statt
    /// `UpdateWorkflowRule` zu missbrauchen) damit das Settings-UI einen
    /// einfachen Switch ohne kompletten Draft-Roundtrip bauen kann.
    SetWorkflowRuleDryRun {
        rule_id: crate::domain::workflow::WorkflowRuleId,
        dry_run: bool,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Mail in der Envelope-Tabelle als "geplante Action fällig am X"
    /// markieren. Wird vom Sync-Hook gefeuert (delay_minutes > 0) und vom
    /// Backfill auf bestehende Mails. Snapshot-Semantik: Werte bleiben
    /// fest, auch wenn die Rule danach geändert wird.
    TagEnvelopeScheduled {
        message_id: MessageId,
        tag: crate::domain::workflow::ScheduledActionTag,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Schedule-Tag löschen — wenn der User die Mail manuell verschiebt
    /// oder eine Rule weggeklickt wird und der zugehörige Tag obsolet
    /// werden soll. Idempotent.
    ClearEnvelopeScheduled {
        message_id: MessageId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Audit-Eintrag persistieren — pro Sweep-Versuch genau ein Row.
    /// Treibt das Settings-Panel "was hat die Automation gemacht".
    InsertRuleActionLog {
        entry: crate::domain::workflow::RuleActionLogEntry,
        ack: oneshot::Sender<Result<(), DbError>>,
    },

    // ─── Contacts ─────────────────────────────────────────────────────

    /// Manuelle Erstellung ODER Auto-Extraction-Persist. Optional kann
    /// eine Initial-E-Mail-Adresse mitgegeben werden (häufigster Fall:
    /// "leg Kontakt für diese Adresse an"). Bei UNIQUE-Verletzung auf
    /// dem contact_emails-Insert wird der Fehler weitergereicht — Caller
    /// muss vorher `get_contact_for_email` checken oder mit dem Fehler
    /// umgehen können.
    CreateContact {
        contact: crate::domain::contact::Contact,
        initial_email: Option<String>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Stammdaten-Update — E-Mail-Liste wird separat verwaltet.
    UpdateContact {
        contact: crate::domain::contact::Contact,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    DeleteContact {
        contact_id: crate::domain::contact::ContactId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Adresse einem Kontakt zuordnen (Adresse darf noch keinem anderen
    /// Kontakt gehören — UNIQUE-Constraint sorgt dafür).
    AddContactEmail {
        contact_id: crate::domain::contact::ContactId,
        email: String,
        is_primary: bool,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    RemoveContactEmail {
        contact_id: crate::domain::contact::ContactId,
        email: String,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Genau eine Adresse als primary markieren (alle anderen werden
    /// im selben Tx auf 0 gesetzt). Frontend nutzt das fürs "Mail
    /// schreiben"-Default-Empfänger-Verhalten.
    SetPrimaryContactEmail {
        contact_id: crate::domain::contact::ContactId,
        email: String,
        ack: oneshot::Sender<Result<(), DbError>>,
    },

    /// Auto-Extraction-Pipeline: pi hat aus einer Mail nichts brauchbares
    /// gefunden. Eintrag in `extraction_misses` damit wir denselben Body
    /// nicht beim nächsten Mail-Open nochmal an pi werfen.
    RecordExtractionMiss {
        email: String,
        envelope_id: crate::domain::message::MessageId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },

    /// Compose-Send-Side-Effect: typed-in Empfänger des Users sofort in
    /// die address_history aufnehmen damit das Autocomplete sie kennt
    /// — ohne auf den nächsten IMAP-Sync der Sent-Mail warten zu müssen.
    /// Im Gegensatz zum Sync-Side-Effect filtert dieser Pfad eigene
    /// Adressen NICHT raus (User darf an sich selbst schreiben).
    RecordOutgoingAddresses {
        addresses: Vec<crate::domain::message::Address>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },

    /// Tag erstellen ODER per upsert-by-name liefern (case-insensitive
    /// match auf `name`). Liefert die ID des bestehenden bzw. neu
    /// angelegten Tags zurück, sodass Caller direkt `replace_contact_tags`
    /// füttern können ohne eine zusätzliche Read-Query.
    UpsertTag {
        name: String,
        color: Option<String>,
        ack: oneshot::Sender<Result<crate::domain::contact::TagId, DbError>>,
    },
    /// Reine Stamm-Update (Name/Color), ohne Membership-Änderungen.
    UpdateTag {
        tag: crate::domain::contact::Tag,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Globaler Delete: Tag ist überall weg, contact_tags cascadet.
    DeleteTag {
        tag_id: crate::domain::contact::TagId,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Atomarer Replace der Tag-Membership eines Contacts. Caller schickt
    /// die GEWÜNSCHTE Liste; alte Verknüpfungen die nicht in der neuen
    /// Liste sind werden gelöscht, neue dazu. Vermeidet add/remove-
    /// Roundtrip-Räderwerk im Frontend.
    ReplaceContactTags {
        contact_id: crate::domain::contact::ContactId,
        tag_ids: Vec<crate::domain::contact::TagId>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Phase-1 calendar writes. SQL implementation lives in
    /// `crate::timeprotocol::store` rather than `db_ops` so the calendar
    /// bounded context owns its own persistence — see the architecture
    /// note in `timeprotocol/mod.rs`.
    ///
    /// Cancellation is also dispatched as `UpsertCommitment` — per
    /// ADR-0011 §3 / Variante B, a cancellation is a normal mutation
    /// that bumps SEQUENCE and sets STATUS:CANCELLED. Hard delete
    /// (purge) is intentionally not exposed in Phase 1.
    UpsertCommitment {
        commitment: crate::timeprotocol::domain::Commitment,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
    /// Cascade-delete every row whose `series_uid` matches — "ganze
    /// Serie absagen" on an RRULE-expanded occurrence. Series rows are
    /// excluded from IMAP-publish anyway (sync filter on `series_uid IS
    /// NOT NULL`), so this is a hard delete with no envelope to emit.
    /// `ack` returns the row count actually removed.
    DeleteSeries {
        series_uid: String,
        ack: oneshot::Sender<Result<usize, DbError>>,
    },
    /// Phase-3 negotiation writes. One atomic operation that upserts
    /// the negotiation row, replaces its slot set, and (optionally)
    /// appends a new envelope to the message log. Idempotent on the
    /// envelope's `message_id` — duplicate-delivery is treated as a
    /// no-op per spec §7.1.
    ApplyNegotiationUpdate {
        negotiation: crate::timeprotocol::domain::Negotiation,
        new_message: Option<crate::timeprotocol::domain::NegotiationMessage>,
        ack: oneshot::Sender<Result<(), DbError>>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("pool: {0}")]
    Pool(#[from] r2d2::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    /// The writer-actor channel was closed before the caller's command
    /// was processed. Currently unused — every WriteCmd path turns this
    /// failure mode into a `String` ("writer channel closed") on its
    /// own. Kept as a typed variant so a future migration to typed
    /// errors at the boundary doesn't have to touch the enum shape.
    #[allow(dead_code)]
    #[error("writer channel closed")]
    WriterGone,
}

#[derive(Clone)]
pub struct DbHandle {
    pub writer: mpsc::Sender<WriteCmd>,
    pub reads: ReadPool,
}

/// Apply all PRAGMAs that have to fire on every freshly-opened SQLCipher
/// connection. Order matters: the `key` PRAGMA must come BEFORE any other
/// statement, otherwise SQLCipher reads the (encrypted) file with no key,
/// fails to decrypt the page header, and reports "file is not a database".
///
/// Raw-key form `x'<hex>'`: SQLCipher takes the 32 bytes literally and
/// skips its built-in PBKDF2. We feed it 32 bytes from the OS RNG so the
/// added key-stretching wouldn't buy us anything — and skipping it makes
/// connection-open ~80ms faster on cold pools, which adds up on the
/// read-pool size we use (4).
fn apply_pragmas(conn: &Connection, cipher_key: &str) -> rusqlite::Result<()> {
    // SQLCipher pragmas. `pragma_update` issues `PRAGMA <name> = <value>`
    // with proper quoting — the value goes through `to_sql`, so we have
    // to bake the `x'...'` literal into the *name slot* via execute().
    // Hex-key with the `x'<hex>'` syntax bypasses PBKDF2.
    conn.execute_batch(&format!(
        "PRAGMA key = \"x'{cipher_key}'\";\nPRAGMA cipher_compatibility = 4;"
    ))?;

    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

/// Liest den DB-Master-Key aus dem OS-Keyring. Existiert keiner, generiert
/// einer aus dem OS-RNG (32 Bytes ⇒ 256 bit) und legt ihn ab. Liefert den
/// Key als hex-String, weil das das Format ist, das SQLCipher's `x'<hex>'`
/// Form direkt frisst.
///
/// **Fail-closed**: Schlägt der Keyring-Zugriff fehl (kein Provider verfügbar,
/// User-Account-Restriction, Backend-Crash), brechen wir hart ab, statt auf
/// einen Default-Key zurückzufallen. Das hardcoded `__unencrypted_dev__` aus
/// der alten Code-Basis ist *Bug*, nicht Feature — Plaintext-Persistenz darf
/// niemals automatisch passieren.
pub fn open_cipher_key() -> Result<String, String> {
    const SERVICE: &str = "crystalmail";
    const ENTRY: &str = "db_master";

    let entry = keyring::Entry::new(SERVICE, ENTRY)
        .map_err(|e| format!("keyring::Entry::new({SERVICE}, {ENTRY}): {e}"))?;

    match entry.get_password() {
        Ok(hex) if hex.len() == 64 => Ok(hex),
        Ok(other) => Err(format!(
            "DB-Master-Key im Keyring hat unerwartete Länge: {} (erwartet 64 hex-chars). \
             Falls das ein migrierter alter Eintrag ist, manuell löschen und App neu starten.",
            other.len()
        )),
        Err(keyring::Error::NoEntry) => {
            // Erstaufruf — frischen Key generieren und persistieren.
            let mut bytes = [0u8; 32];
            getrandom::getrandom(&mut bytes)
                .map_err(|e| format!("getrandom für DB-Master-Key: {e}"))?;
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            entry
                .set_password(&hex)
                .map_err(|e| format!("Keyring set_password (Erstaufruf): {e}"))?;
            tracing::info!("DB-Master-Key beim Erstaufruf erzeugt und im Keyring abgelegt");
            Ok(hex)
        }
        Err(e) => Err(format!("Keyring get_password: {e}")),
    }
}

/// Open the store, apply migrations, and spawn the writer actor.
///
/// `cipher_key` ist ein 64-stelliger Hex-String (256 bit) — wird über
/// `PRAGMA key = "x'<hex>'"` an SQLCipher gegeben.
pub fn open(db_path: &Path, cipher_key: &str) -> Result<DbHandle, DbError> {
    // Erst-Migration: Wenn die Datei existiert und im Klartext lesbar ist,
    // wandeln wir sie um, BEVOR der Writer und der Read-Pool dranfassen.
    if db_path.exists() {
        if let Some(plaintext) = is_plaintext_db(db_path) {
            if plaintext {
                tracing::warn!(
                    path = %db_path.display(),
                    "Plaintext-DB erkannt — migriere zu SQLCipher-verschlüsseltem Format"
                );
                migrate_plaintext_to_encrypted(db_path, cipher_key)
                    .map_err(|e| DbError::Sqlite(rusqlite::Error::InvalidParameterName(
                        format!("Plaintext→Encrypted Migration: {e}"),
                    )))?;
                tracing::info!("Plaintext-DB erfolgreich verschlüsselt");
            }
        }
    }

    let mut write_conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    apply_pragmas(&write_conn, cipher_key)?;
    migrations::apply(&mut write_conn)?;

    let key_for_init = cipher_key.to_string();
    let manager = SqliteConnectionManager::file(db_path)
        .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_init(move |c| apply_pragmas(c, &key_for_init));
    let reads = Pool::builder().max_size(4).build(manager)?;

    let (tx, rx) = mpsc::channel::<WriteCmd>(256);
    tokio::spawn(run_writer(write_conn, rx));

    Ok(DbHandle { writer: tx, reads })
}

/// Probe: ist die Datei eine *unverschlüsselte* SQLite-DB?
///
/// Wir öffnen ohne Key und versuchen einen trivialen `SELECT` auf
/// `sqlite_master`. Klappt das, ist die Datei Klartext. Wirft SQLite einen
/// "file is not a database"-Fehler, ist sie schon verschlüsselt (oder
/// korrupt — beide Fälle behandelt der Caller gleich: keine Migration).
///
/// `Some(true)` = Plaintext, `Some(false)` = bereits verschlüsselt,
/// `None` = konnte nicht öffnen (z.B. Permission-Problem). Fail-safe:
/// keine Migration auslösen, der nachfolgende Open wird einen klaren
/// SQLCipher-Fehler werfen.
fn is_plaintext_db(path: &Path) -> Option<bool> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let result: rusqlite::Result<i64> =
        conn.query_row("SELECT 1 FROM sqlite_master LIMIT 1", [], |r| r.get(0));
    match result {
        Ok(_) => Some(true),
        // "file is not a database" = encrypted oder corrupt — beide Fälle
        // wollen wir NICHT migrieren, also Some(false).
        Err(rusqlite::Error::SqliteFailure(_, _)) => Some(false),
        Err(_) => None,
    }
}

/// Wandelt eine Klartext-SQLite-DB in eine SQLCipher-verschlüsselte um.
///
/// Ablauf:
/// 1. Original-DB als `main` öffnen (ohne Key).
/// 2. Frische DB unter `<original>.tmp.enc` als `encrypted` ATTACHen,
///    mit Key.
/// 3. `SELECT sqlcipher_export('encrypted')` kopiert Schema+Daten.
/// 4. DETACH, schließen.
/// 5. Original-Datei nach `<original>.plaintext.bak` umbenennen
///    (Safety-Net — der User kann das nach einem erfolgreichen
///    ersten Encrypted-Run löschen).
/// 6. `<original>.tmp.enc` nach `<original>` umbenennen.
///
/// Bei Fehler vor Schritt 5: Original ist unangetastet, tmp-File wird
/// best-effort entfernt. Bei Fehler zwischen 5 und 6: Original ist
/// als `.plaintext.bak` da, der User wird durch die App-Fehlermeldung
/// auf das manuelle Rollback hingewiesen.
fn migrate_plaintext_to_encrypted(
    plaintext_path: &Path,
    cipher_key: &str,
) -> Result<(), String> {
    let tmp_path = plaintext_path.with_extension("tmp.enc");
    let bak_path = plaintext_path.with_extension("plaintext.bak");

    // Falls eine Vorgänger-Migration abgebrochen wurde, alten tmp-File entsorgen.
    let _ = std::fs::remove_file(&tmp_path);

    // CREATE-Flag ist hier wichtig: ATTACH erbt die Open-Flags der
    // Hauptverbindung. Ohne CREATE würde ATTACH die noch nicht existente
    // tmp.enc nicht anlegen können — Symptom: "unable to open database".
    // Backslashes im Windows-Pfad muss SQLite selbst sauber durchreichen
    // (single-quoted strings im SQL-Literal sind nicht escaping-fähig).
    let plaintext_conn = Connection::open_with_flags(
        plaintext_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|e| format!("open plaintext DB: {e}"))?;

    // user_version aus der Plaintext-DB rüberretten — `sqlcipher_export`
    // kopiert nur Schema und Daten, keine PRAGMAs. Ohne diesen Schritt
    // würde die verschlüsselte DB bei user_version=0 starten und den
    // Migrations-Lauf 0001 erneut ausführen, was an "table accounts
    // already exists" kracheln würde.
    let user_version: i64 = plaintext_conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|e| format!("read plaintext user_version: {e}"))?;

    // ATTACH-Pfad muss als SQL-String-Literal inline kommen (Bindings
    // funktionieren für ATTACH PATH/KEY nicht). Pfad single-quoten und
    // einfache Quotes durch Verdoppelung escapen. Der Hex-Key wandert
    // in der `x'...'`-Form rein — deshalb das doppelte Anführungszeichen
    // außenrum, damit SQLite das `'` im Inneren als Literal-Zeichen
    // erkennt und nicht als String-Ende.
    let tmp_path_str = tmp_path
        .to_str()
        .ok_or_else(|| "tmp path not UTF-8".to_string())?;
    let escaped_tmp = tmp_path_str.replace('\'', "''");
    plaintext_conn
        .execute_batch(&format!(
            "ATTACH DATABASE '{escaped_tmp}' AS encrypted KEY \"x'{cipher_key}'\";\n\
             PRAGMA encrypted.cipher_compatibility = 4;\n\
             SELECT sqlcipher_export('encrypted');\n\
             PRAGMA encrypted.user_version = {user_version};\n\
             DETACH DATABASE encrypted;",
        ))
        .map_err(|e| {
            // Tmp-File aufräumen damit ein Retry sauber startet.
            let _ = std::fs::remove_file(&tmp_path);
            format!("sqlcipher_export: {e}")
        })?;
    drop(plaintext_conn);

    // Atomarer Swap: erst Original wegsichern, dann tmp einsetzen.
    // Zwei separate Renames statt copy+delete — Rename ist atomar
    // innerhalb desselben Filesystems, was hier garantiert ist.
    if bak_path.exists() {
        // Vorhergehende abgebrochene Migration hatte schon ein .bak
        // hinterlassen — wir überschreiben es nicht stillschweigend.
        return Err(format!(
            "Eine ältere Plaintext-Backup-Datei existiert bereits unter {}. \
             Bitte manuell entfernen oder umbenennen, dann App erneut starten.",
            bak_path.display()
        ));
    }
    std::fs::rename(plaintext_path, &bak_path)
        .map_err(|e| format!("rename original → bak: {e}"))?;
    std::fs::rename(&tmp_path, plaintext_path).map_err(|e| {
        // Halb-fertige Migration: Original liegt als .bak vor, tmp ist
        // noch da. Wir versuchen den Rollback, aber loggen prominent.
        let rollback = std::fs::rename(&bak_path, plaintext_path);
        format!(
            "rename tmp → original: {e}. Rollback der .bak {}.",
            if rollback.is_ok() { "erfolgreich" } else { "FEHLGESCHLAGEN" }
        )
    })?;

    tracing::info!(
        plaintext_backup = %bak_path.display(),
        "Plaintext-Backup unter '.plaintext.bak' aufgehoben — \
         nach erfolgreichem Smoke-Test der verschlüsselten DB löschbar"
    );

    Ok(())
}

async fn run_writer(mut conn: Connection, mut rx: mpsc::Receiver<WriteCmd>) {
    while let Some(cmd) = rx.recv().await {
        dispatch(&mut conn, cmd);
    }
}

fn dispatch(conn: &mut Connection, cmd: WriteCmd) {
    match cmd {
        WriteCmd::AddAccount { account, ack } => {
            let _ = ack.send(insert_account(conn, &account));
        }
        WriteCmd::UpdateAccount { account, ack } => {
            let _ = ack.send(update_account(conn, &account));
        }
        WriteCmd::DeleteAccount { id, ack } => {
            let _ = ack.send(delete_account(conn, &id));
        }
        WriteCmd::EnsureFolder {
            account_id,
            name,
            ack,
        } => {
            let _ = ack.send(ensure_folder(conn, &account_id, &name));
        }
        WriteCmd::DeleteFolderTree { folder_id, ack } => {
            let _ = ack.send(delete_folder_tree(conn, &folder_id));
        }
        WriteCmd::UpdateFolderSyncState {
            folder_id,
            uid_validity,
            uid_next,
            last_sync_ts,
            ack,
        } => {
            let _ = ack.send(update_folder_sync_state(
                conn,
                &folder_id,
                uid_validity,
                uid_next,
                &last_sync_ts,
            ));
        }
        WriteCmd::SetFolderSyncEnabled {
            folder_id,
            enabled,
            ack,
        } => {
            let _ = ack.send(set_folder_sync_enabled(conn, &folder_id, enabled));
        }
        WriteCmd::UpsertEnvelope {
            envelope,
            body_text,
            ack,
        } => {
            let _ = ack.send(upsert_envelope(conn, &envelope, body_text.as_deref()));
        }
        WriteCmd::UpdateFlags {
            message_id,
            flags,
            ack,
        } => {
            let _ = ack.send(update_flags(conn, &message_id, &flags));
        }
        WriteCmd::StoreBody {
            message_id,
            raw_rfc822,
            plain_text,
            html_text,
            has_attachments,
            ack,
        } => {
            let _ = ack.send(store_body(
                conn,
                &message_id,
                &raw_rfc822,
                plain_text.as_deref(),
                html_text.as_deref(),
                has_attachments,
            ));
        }
        WriteCmd::DeleteEnvelopes {
            folder_id,
            imap_uids,
            ack,
        } => {
            let _ = ack.send(delete_envelopes(conn, &folder_id, &imap_uids));
        }
        WriteCmd::ReplaceAliases {
            account_id,
            aliases,
            ack,
        } => {
            let _ = ack.send(replace_aliases(conn, &account_id, &aliases));
        }
        WriteCmd::InsertSpamRule { rule, ack } => {
            let _ = ack.send(insert_spam_rule(conn, &rule));
        }
        WriteCmd::SetSpamRuleEnabled {
            rule_id,
            enabled,
            ack,
        } => {
            let _ = ack.send(set_spam_rule_enabled(conn, &rule_id, enabled));
        }
        WriteCmd::DeleteSpamRule { rule_id, ack } => {
            let _ = ack.send(delete_spam_rule(conn, &rule_id));
        }
        WriteCmd::IncrementSpamRuleHits {
            rule_id,
            delta,
            ack,
        } => {
            let _ = ack.send(increment_spam_rule_hits(conn, &rule_id, delta));
        }
        WriteCmd::InsertWorkflow { workflow, ack } => {
            let _ = ack.send(insert_workflow(conn, &workflow));
        }
        WriteCmd::UpdateWorkflow { workflow, ack } => {
            let _ = ack.send(update_workflow(conn, &workflow));
        }
        WriteCmd::DeleteWorkflow { workflow_id, ack } => {
            let _ = ack.send(delete_workflow(conn, &workflow_id));
        }
        WriteCmd::RecordWorkflowRun { workflow_id, ack } => {
            let _ = ack.send(record_workflow_run(conn, &workflow_id));
        }
        WriteCmd::InsertWorkflowRule { rule, ack } => {
            let _ = ack.send(insert_workflow_rule(conn, &rule));
        }
        WriteCmd::UpdateWorkflowRule { rule, ack } => {
            let _ = ack.send(update_workflow_rule(conn, &rule));
        }
        WriteCmd::DeleteWorkflowRule { rule_id, ack } => {
            let _ = ack.send(delete_workflow_rule(conn, &rule_id));
        }
        WriteCmd::SetWorkflowRuleEnabled {
            rule_id,
            enabled,
            ack,
        } => {
            let _ = ack.send(set_workflow_rule_enabled(conn, &rule_id, enabled));
        }
        WriteCmd::IncrementWorkflowRuleHit { rule_id, ack } => {
            let _ = ack.send(increment_workflow_rule_hit(conn, &rule_id));
        }
        WriteCmd::AddWorkflowTraining { message_ids, ack } => {
            let _ = ack.send(add_workflow_training(conn, &message_ids));
        }
        WriteCmd::RemoveWorkflowTraining { message_ids, ack } => {
            let _ = ack.send(remove_workflow_training(conn, &message_ids));
        }
        WriteCmd::ClearWorkflowTraining { ack } => {
            let _ = ack.send(clear_workflow_training(conn));
        }
        WriteCmd::ImportBundle { plan, ack } => {
            let _ = ack.send(import_bundle(conn, plan));
        }
        WriteCmd::Ping { ack } => {
            // Nichts in der DB anfassen — das Ack-Senden allein beweist,
            // dass die Receiver-Schleife läuft und die Channel offen ist.
            let _ = ack.send(Ok(()));
        }
        WriteCmd::SetWorkflowRuleDryRun {
            rule_id,
            dry_run,
            ack,
        } => {
            let _ = ack.send(set_workflow_rule_dry_run(conn, &rule_id, dry_run));
        }
        WriteCmd::TagEnvelopeScheduled {
            message_id,
            tag,
            ack,
        } => {
            let _ = ack.send(tag_envelope_scheduled(conn, &message_id, &tag));
        }
        WriteCmd::ClearEnvelopeScheduled { message_id, ack } => {
            let _ = ack.send(clear_envelope_scheduled(conn, &message_id));
        }
        WriteCmd::InsertRuleActionLog { entry, ack } => {
            let _ = ack.send(insert_rule_action_log(conn, &entry));
        }
        WriteCmd::CreateContact {
            contact,
            initial_email,
            ack,
        } => {
            let _ = ack.send(create_contact(conn, &contact, initial_email.as_deref()));
        }
        WriteCmd::UpdateContact { contact, ack } => {
            let _ = ack.send(update_contact(conn, &contact));
        }
        WriteCmd::DeleteContact { contact_id, ack } => {
            let _ = ack.send(delete_contact(conn, &contact_id));
        }
        WriteCmd::AddContactEmail {
            contact_id,
            email,
            is_primary,
            ack,
        } => {
            let _ = ack.send(add_contact_email(conn, &contact_id, &email, is_primary));
        }
        WriteCmd::RemoveContactEmail {
            contact_id,
            email,
            ack,
        } => {
            let _ = ack.send(remove_contact_email(conn, &contact_id, &email));
        }
        WriteCmd::SetPrimaryContactEmail {
            contact_id,
            email,
            ack,
        } => {
            let _ = ack.send(set_primary_contact_email(conn, &contact_id, &email));
        }
        WriteCmd::RecordExtractionMiss {
            email,
            envelope_id,
            ack,
        } => {
            let _ = ack.send(record_extraction_miss(conn, &email, &envelope_id));
        }
        WriteCmd::RecordOutgoingAddresses { addresses, ack } => {
            let _ = ack.send(record_outgoing_addresses(conn, &addresses));
        }
        WriteCmd::UpsertTag { name, color, ack } => {
            let _ = ack.send(upsert_tag(conn, &name, color.as_deref()));
        }
        WriteCmd::UpdateTag { tag, ack } => {
            let _ = ack.send(update_tag(conn, &tag));
        }
        WriteCmd::DeleteTag { tag_id, ack } => {
            let _ = ack.send(delete_tag(conn, &tag_id));
        }
        WriteCmd::ReplaceContactTags {
            contact_id,
            tag_ids,
            ack,
        } => {
            let _ = ack.send(replace_contact_tags(conn, &contact_id, &tag_ids));
        }
        WriteCmd::UpsertCommitment { commitment, ack } => {
            let _ = ack.send(crate::timeprotocol::store::upsert_commitment(
                conn,
                &commitment,
            ));
        }
        WriteCmd::DeleteSeries { series_uid, ack } => {
            let _ = ack.send(crate::timeprotocol::store::delete_series_by_uid(
                conn,
                &series_uid,
            ));
        }
        WriteCmd::ApplyNegotiationUpdate {
            negotiation,
            new_message,
            ack,
        } => {
            let _ = ack.send(
                crate::timeprotocol::negotiation_store::apply_negotiation_update(
                    conn,
                    &negotiation,
                    new_message.as_ref(),
                ),
            );
        }
    }
}

