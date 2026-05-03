// Settings-Backup: Export/Import für alle Config-Tische und JSON-Sidecar-Dateien.
//
// Was geht rein:
// * `accounts` + `account_aliases` (IMAP/SMTP-Server, Special-Folder, Aliase, Farbe…)
// * `spam_rules` + `workflows` + `workflow_rules`
// * `pi_config.json` + `workflow_config.json`
// * Optional: IMAP-Passwörter aus dem OS-Keyring, mit einer User-Passphrase
//   verschlüsselt (Argon2id → ChaCha20-Poly1305).
//
// Was bleibt draußen:
// * `envelopes` / `bodies` / `folders` — alles regenerierbar via IMAP-Sync.
// * `workflow_training_candidates` — laufende Trainingsdaten, kein User-Setting.
//
// Dateiformat: ein einzelnes UTF-8 JSON (`*.crystalmail-backup.json`),
// schemaversioniert. `schemaVersion = 1` ist die heutige Form; künftige
// inkompatible Änderungen erhöhen die Nummer und der Importer lehnt ab.
//
// Konflikt-Strategie beim Import: Account mit gleicher Adresse → Skip.
// Spam-Regeln und Workflows bekommen frische UUIDs, sodass mehrfache
// Importe duplizieren statt überschreiben (vorhersehbarer als Merge,
// und der User kann das nachträglich konsolidieren).

use std::collections::HashMap;
use std::path::PathBuf;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::domain::account::{
    Account, AccountAlias, AccountId, ImapEndpoint, SmtpEndpoint, SyncMode,
};
use crate::domain::auth::AuthCredential;
use crate::domain::spam_rule::{SpamRule, SpamRuleId};
use crate::domain::workflow::{Workflow, WorkflowId, WorkflowRule, WorkflowRuleId};
use crate::infrastructure::db::{DbError, DbHandle, WriteCmd};
use crate::infrastructure::queries;
use crate::state::{PiConfig, WorkflowConfig};

const KEYRING_SERVICE: &str = "crystalmail";
const SCHEMA_VERSION: u32 = 1;

// ────────────────────────────────────────────────────────────────────────────
// JSON-Wire-Format
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupBundle {
    /// Inkrementiert bei breaking changes am Format. Importer mit niedrigerer
    /// Version lehnt höheres Bundle ab.
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    pub crystalmail_version: String,

    pub accounts: Vec<BackupAccount>,
    pub spam_rules: Vec<SpamRule>,
    pub workflows: Vec<Workflow>,
    pub workflow_rules: Vec<WorkflowRule>,

    /// `pi_config.json` 1:1 — Pfad zur pi-Binary, Modell, RPC-Settings.
    pub pi_config: Option<PiConfig>,
    /// `workflow_config.json` 1:1 — Skript-Verzeichnis, Interpreter.
    pub workflow_config: Option<WorkflowConfig>,

    /// Verschlüsselter Passwort-Block oder `None`. Niemals klartext.
    pub encrypted_passwords: Option<EncryptedPasswords>,
}

/// Account mit eingebetteten Aliasen — flacher als zwei Listen mit FK-Joins.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupAccount {
    pub id: AccountId,
    pub display_name: String,
    pub address: String,
    pub from_name: String,
    pub color: String,
    pub signature: Option<String>,
    pub signature_html: Option<String>,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub archive_folder: String,
    pub sent_folder: String,
    pub drafts_folder: String,
    pub trash_folder: String,
    pub spam_folder: String,
    pub archive_on_reply: bool,
    pub prefetch_days: i64,
    /// Per-Konto Sync-Modus (IDLE / Polling / beides). Im Bundle als
    /// snake_case-String — `SyncMode::default()` falls die Variable in
    /// einem alten Bundle (vor Migration 0019) noch fehlt.
    #[serde(default)]
    pub sync_mode: SyncMode,
    /// Provider-Verhalten: speichert SMTP-Server gesendete Mails
    /// automatisch im Sent-Ordner. `#[serde(default)]` für Bundles
    /// vor Migration 0020 (Default false ist das pre-Fix-Verhalten).
    #[serde(default)]
    pub server_stores_sent: bool,
    pub aliases: Vec<AccountAlias>,
}

/// Argon2id(passphrase, salt) → 32-byte key, dann ChaCha20-Poly1305 über
/// die JSON-serialisierte Passwort-Map. Salt + Nonce wandern als base64
/// in dieselbe Datei — nichts davon ist geheim, wir brauchen sie nur für
/// die Reproduzierbarkeit beim Decrypt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedPasswords {
    pub kdf: Kdf,
    /// Festgenagelt auf "chacha20poly1305" damit künftige Algo-Wechsel
    /// erkennbar sind. Importer lehnt unbekannte Werte ab.
    pub cipher: String,
    /// 12 Bytes, base64. Random pro Export; niemals wiederverwendet.
    pub nonce_b64: String,
    /// Ciphertext + 16-Byte-Tag, base64.
    pub ciphertext_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kdf {
    /// "argon2id" — andere Werte ablehnen.
    pub algo: String,
    /// Argon2-Format-Version (0x13 = 19 = aktuell).
    pub version: u32,
    /// 16 Bytes random, base64.
    pub salt_b64: String,
    /// Memory cost in KiB. 19456 = OWASP-Default.
    pub memory_kib: u32,
    /// Time cost (iterations).
    pub iterations: u32,
    /// Parallelism (lanes).
    pub parallelism: u32,
}

// Argon2id-Parameter — OWASP "second choice" (m=19 MiB, t=2, p=1).
// Auf einem Desktop ergibt das ~50ms KDF-Zeit, akzeptabel für eine
// einmalige Export/Import-Operation und genug Material gegen GPU-Brute-Force.
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

// ────────────────────────────────────────────────────────────────────────────
// Build (Export)
// ────────────────────────────────────────────────────────────────────────────

/// Liest alle Config-Tische, JSON-Sidecars und (optional) IMAP-Passwörter
/// aus dem Keyring, packt sie in ein `BackupBundle`. Der Aufrufer schreibt
/// das Ergebnis als JSON-Datei.
pub async fn build(
    app: &AppHandle,
    db: &DbHandle,
    passphrase: Option<&str>,
) -> Result<BackupBundle, String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;

    let summaries = queries::list_accounts(&conn).map_err(|e| e.to_string())?;
    let accounts: Vec<BackupAccount> = summaries
        .into_iter()
        .map(|s| BackupAccount {
            id: s.id,
            display_name: s.display_name,
            address: s.address,
            from_name: s.from_name,
            color: s.color,
            signature: s.signature,
            signature_html: s.signature_html,
            imap_host: s.imap_host,
            imap_port: s.imap_port,
            imap_tls: s.imap_tls,
            smtp_host: s.smtp_host,
            smtp_port: s.smtp_port,
            smtp_tls: s.smtp_tls,
            archive_folder: s.archive_folder,
            sent_folder: s.sent_folder,
            drafts_folder: s.drafts_folder,
            trash_folder: s.trash_folder,
            spam_folder: s.spam_folder,
            archive_on_reply: s.archive_on_reply,
            prefetch_days: s.prefetch_days,
            sync_mode: s.sync_mode,
            server_stores_sent: s.server_stores_sent,
            aliases: s.aliases,
        })
        .collect();

    let spam_rules = queries::list_spam_rules(&conn).map_err(|e| e.to_string())?;
    let workflows = queries::list_workflows(&conn).map_err(|e| e.to_string())?;
    let workflow_rules =
        queries::list_workflow_rules(&conn).map_err(|e| e.to_string())?;

    drop(conn);

    let pi_config = crate::commands::pi::load_persisted(app);
    let workflow_config = crate::commands::workflows::load_persisted(app);

    // Passwort-Sammlung — nur wenn der User aktiv eine Passphrase angegeben
    // hat. Wir sammeln aus dem Keyring; fehlende Einträge (z.B. nach einem
    // Account-Edit ohne Passwort-Change) werden stillschweigend ausgelassen.
    let encrypted_passwords = if let Some(phrase) = passphrase {
        if phrase.is_empty() {
            return Err("Passphrase darf nicht leer sein.".into());
        }
        let mut pw_map: HashMap<String, String> = HashMap::new();
        for a in &accounts {
            let entry_name = format!("imap::{}", a.id.0);
            if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &entry_name) {
                if let Ok(pw) = entry.get_password() {
                    pw_map.insert(a.id.0.to_string(), pw);
                }
            }
        }
        let plain = serde_json::to_vec(&pw_map).map_err(|e| e.to_string())?;
        Some(encrypt_blob(&plain, phrase)?)
    } else {
        None
    };

    Ok(BackupBundle {
        schema_version: SCHEMA_VERSION,
        exported_at: Utc::now(),
        crystalmail_version: env!("CARGO_PKG_VERSION").to_string(),
        accounts,
        spam_rules,
        workflows,
        workflow_rules,
        pi_config,
        workflow_config,
        encrypted_passwords,
    })
}

/// Übersicht über die enthaltenen Items — für die Confirm-Anzeige im UI,
/// bevor wir tatsächlich importieren. Dekodiert nur die Zähler, nicht die
/// Passwörter (die wandern erst beim eigentlichen Import durch die KDF).
///
/// Die `conflictingAddresses`-Liste markiert die Stelle, an der Disaster-
/// Recovery in einen Sonderfall kippt: Bundle hat ein Konto mit Adresse X
/// und UUID `aaa`, die Ziel-DB hat schon eines mit X aber UUID `ccc`.
/// Beim Import wird der Bundle-Account aus der Adress-Konflikt-Logik
/// übersprungen, und Spam-/Workflow-Rules die auf `aaa` zeigen können
/// nicht über die FK aufgelöst werden — werden also auch geskippt.
/// Die UI zeigt diese Liste dem User vor dem Klick auf "Importieren",
/// damit er bewusst entscheiden kann: einfach proceedem und die
/// betroffenen Rules verlieren, oder das Ziel-Konto erst löschen
/// (Cascade räumt Cache + alte Rules) und dann re-importieren.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPreview {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    pub crystalmail_version: String,
    pub account_count: u32,
    pub alias_count: u32,
    pub spam_rule_count: u32,
    pub workflow_count: u32,
    pub workflow_rule_count: u32,
    pub has_pi_config: bool,
    pub has_workflow_config: bool,
    pub has_encrypted_passwords: bool,
    /// E-Mail-Adressen aus dem Bundle, die in der Ziel-DB unter einer
    /// anderen UUID schon existieren. Werden beim Import übersprungen.
    pub conflicting_addresses: Vec<String>,
}

/// Reine Bundle-Inspektion ohne DB-Lookup — gibt schon die Zähler her,
/// aber `conflicting_addresses` bleibt leer. `peek_backup_file` ruft
/// `enrich_with_conflicts` danach auf.
pub fn preview(bundle: &BackupBundle) -> BackupPreview {
    BackupPreview {
        schema_version: bundle.schema_version,
        exported_at: bundle.exported_at,
        crystalmail_version: bundle.crystalmail_version.clone(),
        account_count: bundle.accounts.len() as u32,
        alias_count: bundle
            .accounts
            .iter()
            .map(|a| a.aliases.len() as u32)
            .sum(),
        spam_rule_count: bundle.spam_rules.len() as u32,
        workflow_count: bundle.workflows.len() as u32,
        workflow_rule_count: bundle.workflow_rules.len() as u32,
        has_pi_config: bundle.pi_config.is_some(),
        has_workflow_config: bundle.workflow_config.is_some(),
        has_encrypted_passwords: bundle.encrypted_passwords.is_some(),
        conflicting_addresses: Vec::new(),
    }
}

/// Liest die existierenden Konten aus der DB und vergleicht mit den
/// Bundle-Konten: identische Adresse (case-insensitive), aber andere
/// UUID → Konflikt. Reine Adress-Duplikate mit gleicher UUID (z.B.
/// idempotenter Re-Import des selben Backups) werden NICHT als Konflikt
/// gewertet, weil dort die Rules-FKs sauber auflösen.
pub async fn compute_conflicts(
    db: &DbHandle,
    bundle: &BackupBundle,
) -> Result<Vec<String>, String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let existing: std::collections::HashMap<String, AccountId> =
        queries::list_accounts(&conn)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|a| (a.address.to_lowercase(), a.id))
            .collect();

    let conflicts = bundle
        .accounts
        .iter()
        .filter_map(|ba| {
            existing
                .get(&ba.address.to_lowercase())
                .filter(|existing_id| **existing_id != ba.id)
                .map(|_| ba.address.clone())
        })
        .collect();
    Ok(conflicts)
}

// ────────────────────────────────────────────────────────────────────────────
// Apply (Import)
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub accounts_added: u32,
    pub accounts_skipped: u32,
    pub aliases_added: u32,
    pub spam_rules_added: u32,
    /// Spam-Regeln, deren UUID schon in der DB war. Üblicher Fall:
    /// Backup eingespielt nachdem nur ein einzelnes Konto gelöscht wurde —
    /// die Regeln existieren noch, der Import würde sonst durch den
    /// PK-Constraint kracheln. Wir filtern stattdessen still vor.
    pub spam_rules_skipped: u32,
    /// Spam-Regeln, deren `account_id` weder in der existierenden DB
    /// noch im zu importierenden Plan steht — typisch beim Cross-
    /// Maschinen-Import wenn das Quell-Konto via Adress-Konflikt
    /// übersprungen wurde. Würde sonst FK-Violation → Rollback geben.
    pub spam_rules_skipped_unknown_account: u32,
    pub workflows_added: u32,
    pub workflows_skipped: u32,
    pub workflow_rules_added: u32,
    pub workflow_rules_skipped: u32,
    pub workflow_rules_skipped_unknown_account: u32,
    pub passwords_restored: u32,
    /// Wahr wenn `pi_config.json` aus dem Bundle geschrieben wurde —
    /// **überschreibt** etwaige zwischenzeitliche Änderungen, weil die
    /// Config ein Singleton ist (kein zeilenweises Mergen möglich).
    pub pi_config_restored: bool,
    pub workflow_config_restored: bool,
    /// Adressen die übersprungen wurden weil sie bereits existieren —
    /// dem User im UI als Hinweis anzeigen, damit er weiß warum nichts
    /// importiert wurde.
    pub skipped_addresses: Vec<String>,
    /// Hinweise/Warnungen die im Verlauf des Imports aufgetaucht sind
    /// (z.B. Keyring-Schreibfehler) — nicht-fatal, aber sichtbar machen.
    pub warnings: Vec<String>,
}

pub async fn apply(
    app: &AppHandle,
    db: &DbHandle,
    bundle: BackupBundle,
    passphrase: Option<&str>,
) -> Result<ImportReport, String> {
    if bundle.schema_version > SCHEMA_VERSION {
        return Err(format!(
            "Backup hat Schema-Version {} — diese Version unterstützt nur bis {}. \
             Bitte CrystalMail aktualisieren.",
            bundle.schema_version, SCHEMA_VERSION
        ));
    }

    // Passwörter (falls vorhanden) als allererstes entschlüsseln —
    // schlägt das fehl, brechen wir ab BEVOR irgendwas in die DB gelangt.
    let passwords: HashMap<String, String> =
        match (&bundle.encrypted_passwords, passphrase) {
            (Some(blob), Some(phrase)) => {
                let plain = decrypt_blob(blob, phrase)
                    .map_err(|e| format!("Passphrase falsch oder Datei beschädigt: {e}"))?;
                serde_json::from_slice(&plain).map_err(|e| format!("Passwort-JSON ungültig: {e}"))?
            }
            (Some(_), None) => {
                return Err(
                    "Backup enthält verschlüsselte Passwörter — bitte Passphrase angeben \
                     oder Backup ohne Passwörter neu erstellen."
                        .into(),
                );
            }
            _ => HashMap::new(),
        };

    // Bestehende IDs/Adressen einlesen, um Duplikate vor der Transaktion
    // herauszufiltern. Wenn wir das nicht täten, würde z.B. eine Spam-Regel
    // mit existierender UUID den Primary-Key-Constraint verletzen und die
    // gesamte Import-Transaktion zurückrollen — was insbesondere beim
    // teilweisen Restore (User hat nur ein Konto gelöscht, Rest unverändert)
    // ärgerlich ist. Adress-Vergleich ist case-insensitive, weil IMAP-Login
    // das ohnehin meist auch ist.
    //
    // `existing_account_ids` brauchen wir zusätzlich für den dangling-FK-
    // Filter weiter unten: Bundle-Rules, deren account_id weder in der
    // DB noch im Plan steht, würden sonst beim Insert kracheln.
    use std::collections::HashSet;
    let (
        existing_addresses,
        existing_account_ids,
        existing_spam_ids,
        existing_workflow_ids,
        existing_wfr_ids,
    ): (
        Vec<String>,
        HashSet<AccountId>,
        HashSet<crate::domain::spam_rule::SpamRuleId>,
        HashSet<WorkflowId>,
        HashSet<WorkflowRuleId>,
    ) = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let accounts = queries::list_accounts(&conn).map_err(|e| e.to_string())?;
        let addrs: Vec<String> = accounts
            .iter()
            .map(|a| a.address.to_lowercase())
            .collect();
        let account_ids: HashSet<AccountId> = accounts.iter().map(|a| a.id).collect();
        let spam_ids = queries::list_spam_rules(&conn)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.id)
            .collect();
        let wf_ids = queries::list_workflows(&conn)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|w| w.id)
            .collect();
        let wfr_ids = queries::list_workflow_rules(&conn)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|r| r.id)
            .collect();
        (addrs, account_ids, spam_ids, wf_ids, wfr_ids)
    };

    // Plan aufbauen — was kommt rein, was wird übersprungen. Spam-Regeln,
    // Workflows und Workflow-Rules werden by-UUID dedupliziert: existieren
    // sie schon, wird die Backup-Version übersprungen (kein Overwrite —
    // der User könnte zwischen Export und Import an seinen Regeln gefeilt
    // haben). Workflow-Rule-FK auf workflow_id bleibt valide, weil der
    // referenzierte Workflow entweder schon in der DB liegt oder gerade
    // mit dem Bundle reinkommt.
    let mut plan = ImportPlan {
        accounts: Vec::new(),
        spam_rules: Vec::new(),
        workflows: Vec::new(),
        workflow_rules: Vec::new(),
    };
    let mut report = ImportReport::default();

    let (kept_spam, skipped_spam) =
        dedup_by_id(bundle.spam_rules, &existing_spam_ids, |r| r.id);
    plan.spam_rules = kept_spam;
    report.spam_rules_skipped = skipped_spam;

    let (kept_wf, skipped_wf) =
        dedup_by_id(bundle.workflows, &existing_workflow_ids, |w| w.id);
    plan.workflows = kept_wf;
    report.workflows_skipped = skipped_wf;

    let (kept_wfr, skipped_wfr) =
        dedup_by_id(bundle.workflow_rules, &existing_wfr_ids, |r| r.id);
    plan.workflow_rules = kept_wfr;
    report.workflow_rules_skipped = skipped_wfr;

    // Dangling-FK-Filter: Rules referenzieren via `account_id` ein Konto
    // das weder schon in der DB ist noch im Plan zum Insert ansteht. Das
    // passiert beim Cross-Maschinen-Import wenn das Quell-Konto wegen
    // Adress-Konflikt übersprungen wurde (B hat das Konto unter anderer
    // UUID). Würden wir solche Rules drin lassen, kracht das in der
    // Insert-Transaktion an einem `accounts(id)`-FK und rollt alles
    // zurück. Globale Rules (`account_id = None`) bleiben unbehelligt.
    let valid_account_ids: HashSet<AccountId> = existing_account_ids
        .iter()
        .copied()
        .chain(plan.accounts.iter().map(|p| p.account.id))
        .collect();

    let total_spam = plan.spam_rules.len() as u32;
    plan.spam_rules.retain(|r| match r.account_id {
        None => true,
        Some(id) => valid_account_ids.contains(&id),
    });
    report.spam_rules_skipped_unknown_account =
        total_spam - plan.spam_rules.len() as u32;

    let total_wfr = plan.workflow_rules.len() as u32;
    plan.workflow_rules.retain(|r| match r.account_id {
        None => true,
        Some(id) => valid_account_ids.contains(&id),
    });
    report.workflow_rules_skipped_unknown_account =
        total_wfr - plan.workflow_rules.len() as u32;

    for ba in bundle.accounts {
        if existing_addresses.contains(&ba.address.to_lowercase()) {
            report.accounts_skipped += 1;
            report.skipped_addresses.push(ba.address.clone());
            continue;
        }
        // Auf Domain-Form konvertieren. `keyring_entry` ist deterministisch
        // an die Account-UUID gekoppelt, damit der bestehende SMTP/IMAP-Code
        // (der via `format!("imap::{}", id.0)` lookt) ohne Änderung weiter
        // funktioniert.
        let entry_name = format!("imap::{}", ba.id.0);
        let account = Account {
            id: ba.id,
            display_name: ba.display_name.clone(),
            address: ba.address.clone(),
            from_name: ba.from_name,
            color: ba.color,
            signature: ba.signature,
            signature_html: ba.signature_html,
            imap: ImapEndpoint {
                host: ba.imap_host,
                port: ba.imap_port,
                tls: ba.imap_tls,
            },
            smtp: SmtpEndpoint {
                host: ba.smtp_host,
                port: ba.smtp_port,
                tls: ba.smtp_tls,
            },
            credential: AuthCredential::Password { keyring_entry: entry_name },
            archive_folder: ba.archive_folder,
            sent_folder: ba.sent_folder,
            drafts_folder: ba.drafts_folder,
            trash_folder: ba.trash_folder,
            spam_folder: ba.spam_folder,
            archive_on_reply: ba.archive_on_reply,
            prefetch_days: ba.prefetch_days,
            sync_mode: ba.sync_mode,
            server_stores_sent: ba.server_stores_sent,
        };
        plan.accounts.push(PlannedAccount {
            account,
            aliases: ba.aliases,
        });
    }

    // Atomarer SQL-Import: ein einziger WriteCmd, ein einziger Tx im Writer.
    // Bei Fehler rollback der gesamten DB-Seite — Keyring/JSON-Dateien werden
    // erst danach geschrieben.
    let imported_account_ids: Vec<AccountId> =
        plan.accounts.iter().map(|p| p.account.id).collect();
    let alias_count: u32 = plan.accounts.iter().map(|p| p.aliases.len() as u32).sum();
    let spam_count = plan.spam_rules.len() as u32;
    let wf_count = plan.workflows.len() as u32;
    let wfr_count = plan.workflow_rules.len() as u32;

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::ImportBundle { plan, ack: tx })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e: DbError| format!("DB-Import fehlgeschlagen, Rollback: {e}"))?;

    report.accounts_added = imported_account_ids.len() as u32;
    report.aliases_added = alias_count;
    report.spam_rules_added = spam_count;
    report.workflows_added = wf_count;
    report.workflow_rules_added = wfr_count;

    // Ab hier: SQLite ist commited. Best-Effort-Schritte (Keyring + JSON-
    // Dateien); jeder Fehler wird als Warning protokolliert, nicht als
    // Fatal — der User hat seine Accounts schon, kann Passwörter manuell
    // nachtragen.
    for id in &imported_account_ids {
        if let Some(pw) = passwords.get(&id.0.to_string()) {
            let entry_name = format!("imap::{}", id.0);
            match keyring::Entry::new(KEYRING_SERVICE, &entry_name) {
                Ok(entry) => match entry.set_password(pw) {
                    Ok(_) => report.passwords_restored += 1,
                    Err(e) => report
                        .warnings
                        .push(format!("Keyring-Schreibfehler für {id:?}: {e}")),
                },
                Err(e) => report
                    .warnings
                    .push(format!("Keyring-Init fehlgeschlagen für {id:?}: {e}")),
            }
        }
    }

    if let Some(cfg) = bundle.pi_config {
        match write_sidecar(app, "pi_config.json", &cfg) {
            Ok(_) => report.pi_config_restored = true,
            Err(e) => report.warnings.push(format!("pi_config.json: {e}")),
        }
    }
    if let Some(cfg) = bundle.workflow_config {
        match write_sidecar(app, "workflow_config.json", &cfg) {
            Ok(_) => report.workflow_config_restored = true,
            Err(e) => report.warnings.push(format!("workflow_config.json: {e}")),
        }
    }

    // Background-Sync-Actors für die frisch importierten Konten starten.
    // Ohne diesen Hook würden die Accounts erst beim nächsten App-Start
    // einen Actor bekommen — der User muss sonst manuell neustarten,
    // damit IDLE/Polling für die Import-Konten anspringt.
    //
    // Erst alle Summaries einsammeln, conn droppen, DANN spawn-loop —
    // sonst halten wir die r2d2-Connection über die `await`-Punkte hinweg
    // und blockieren andere Reader.
    let summaries_to_spawn: Vec<crate::infrastructure::queries::AccountSummary> = {
        match db.reads.get() {
            Ok(conn) => {
                let mut out = Vec::new();
                for id in &imported_account_ids {
                    match queries::get_account(&conn, id) {
                        Ok(Some(s)) => out.push(s),
                        Ok(None) => report.warnings.push(format!(
                            "Actor-Spawn: importierter Account {id:?} nicht in DB findbar"
                        )),
                        Err(e) => report
                            .warnings
                            .push(format!("Actor-Spawn: get_account({id:?}): {e}")),
                    }
                }
                out
            }
            Err(e) => {
                report
                    .warnings
                    .push(format!("Actor-Spawn nach Import: read pool: {e}"));
                Vec::new()
            }
        }
    };

    if !summaries_to_spawn.is_empty() {
        use tauri::Manager;
        let state = app.state::<crate::state::AppState>();
        for s in summaries_to_spawn {
            crate::application::actor::spawn_one(
                app.clone(),
                db.clone(),
                &state.actor_handles,
                crate::application::actor::account_from_summary(s),
            )
            .await;
        }
    }

    Ok(report)
}

#[derive(Debug)]
pub struct ImportPlan {
    pub accounts: Vec<PlannedAccount>,
    pub spam_rules: Vec<SpamRule>,
    pub workflows: Vec<Workflow>,
    pub workflow_rules: Vec<WorkflowRule>,
}

#[derive(Debug)]
pub struct PlannedAccount {
    pub account: Account,
    pub aliases: Vec<AccountAlias>,
}

fn write_sidecar<T: Serialize>(
    app: &AppHandle,
    filename: &str,
    value: &T,
) -> Result<(), String> {
    let dir: PathBuf = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.join(filename);
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, bytes).map_err(|e| format!("write: {e}"))
}

// Type-Sicherheits-Helper — eine ID aus einem String parsen, der hoffentlich
// eine UUID ist. Nicht öffentlich, weil Bundle-IDs schon `Uuid` sind.
#[allow(dead_code)]
fn account_id_from_str(s: &str) -> Result<AccountId, String> {
    Uuid::parse_str(s)
        .map(AccountId)
        .map_err(|e| format!("invalid account id: {e}"))
}

// `SpamRuleId` / `WorkflowId` / `WorkflowRuleId` werden als `Uuid` durch JSON
// gereicht — Serde-Default reicht. Wir referenzieren die Typen nur, um die
// Imports nicht ungenutzt zu lassen.
#[allow(dead_code)]
fn _phantom_ids() -> (SpamRuleId, WorkflowId, WorkflowRuleId) {
    (
        SpamRuleId(Uuid::nil()),
        WorkflowId(Uuid::nil()),
        WorkflowRuleId(Uuid::nil()),
    )
}

/// Behält aus `items` nur die, deren via `id_of` extrahierte ID *nicht* in
/// `existing` ist; gibt zusätzlich die Anzahl der übersprungenen zurück.
/// Aus `apply()` ausgegliedert, damit das partial-restore-Verhalten ohne
/// DB-Fixture testbar ist.
fn dedup_by_id<T, ID, F>(
    items: Vec<T>,
    existing: &std::collections::HashSet<ID>,
    id_of: F,
) -> (Vec<T>, u32)
where
    ID: std::hash::Hash + Eq,
    F: Fn(&T) -> ID,
{
    let total = items.len();
    let kept: Vec<T> = items
        .into_iter()
        .filter(|i| !existing.contains(&id_of(i)))
        .collect();
    let skipped = (total - kept.len()) as u32;
    (kept, skipped)
}

// ────────────────────────────────────────────────────────────────────────────
// Crypto
// ────────────────────────────────────────────────────────────────────────────

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).map_err(|e| format!("getrandom: {e}"))?;
    Ok(buf)
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        Some(32),
    )
    .map_err(|e| format!("argon2 params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| format!("argon2 hash: {e}"))?;
    Ok(key)
}

fn encrypt_blob(plain: &[u8], passphrase: &str) -> Result<EncryptedPasswords, String> {
    let salt = random_bytes::<16>()?;
    let key = derive_key(passphrase, &salt)?;
    let nonce_bytes = random_bytes::<12>()?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain)
        .map_err(|e| format!("encrypt: {e}"))?;
    Ok(EncryptedPasswords {
        kdf: Kdf {
            algo: "argon2id".into(),
            version: 0x13,
            salt_b64: B64.encode(salt),
            memory_kib: ARGON2_MEMORY_KIB,
            iterations: ARGON2_ITERATIONS,
            parallelism: ARGON2_PARALLELISM,
        },
        cipher: "chacha20poly1305".into(),
        nonce_b64: B64.encode(nonce_bytes),
        ciphertext_b64: B64.encode(ct),
    })
}

fn decrypt_blob(blob: &EncryptedPasswords, passphrase: &str) -> Result<Vec<u8>, String> {
    if blob.kdf.algo != "argon2id" {
        return Err(format!("unsupported KDF: {}", blob.kdf.algo));
    }
    if blob.cipher != "chacha20poly1305" {
        return Err(format!("unsupported cipher: {}", blob.cipher));
    }
    let salt = B64
        .decode(&blob.kdf.salt_b64)
        .map_err(|e| format!("salt b64: {e}"))?;
    let nonce_bytes = B64
        .decode(&blob.nonce_b64)
        .map_err(|e| format!("nonce b64: {e}"))?;
    let ct = B64
        .decode(&blob.ciphertext_b64)
        .map_err(|e| format!("ciphertext b64: {e}"))?;
    if nonce_bytes.len() != 12 {
        return Err(format!("nonce length: {} (expected 12)", nonce_bytes.len()));
    }

    // KDF-Parameter aus dem Header (statt fest verdrahtet) übernehmen, damit
    // ältere Backups mit anderen Parametern noch lesbar sind.
    let params = Params::new(
        blob.kdf.memory_kib,
        blob.kdf.iterations,
        blob.kdf.parallelism,
        Some(32),
    )
    .map_err(|e| format!("argon2 params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
        .map_err(|e| format!("argon2 hash: {e}"))?;

    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), ct.as_ref())
        .map_err(|e| format!("decrypt (Tag-Mismatch → Passphrase falsch?): {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let plain = br#"{"a":"hunter2","b":"correcthorsebatterystaple"}"#;
        let blob = encrypt_blob(plain, "test-passphrase-123").unwrap();
        let recovered = decrypt_blob(&blob, "test-passphrase-123").unwrap();
        assert_eq!(plain.to_vec(), recovered);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let plain = b"secret";
        let blob = encrypt_blob(plain, "right").unwrap();
        let err = decrypt_blob(&blob, "wrong").unwrap_err();
        assert!(err.contains("decrypt"));
    }

    #[test]
    fn unsupported_algo_rejected() {
        let mut blob = encrypt_blob(b"x", "p").unwrap();
        blob.kdf.algo = "scrypt".into();
        let err = decrypt_blob(&blob, "p").unwrap_err();
        assert!(err.contains("unsupported KDF"));
    }

    /// Partial-restore-Szenario aus dem User-Feedback: 3 Items im Backup,
    /// 2 davon existieren bereits in der Ziel-DB. Erwartung: 1 wird
    /// importiert, 2 übersprungen — kein PK-Conflict, kein Rollback.
    #[test]
    fn dedup_partial_restore() {
        use std::collections::HashSet;
        // Tuple-Items mit einer ID-Komponente — minimale Stand-ins für
        // Spam-Rule / Workflow ohne den ganzen Domain-Layer zu mocken.
        let bundle = vec![("a", 1u32), ("b", 2), ("c", 3)];
        let existing: HashSet<u32> = [1, 3].into_iter().collect();
        let (kept, skipped) = dedup_by_id(bundle, &existing, |t| t.1);
        assert_eq!(skipped, 2);
        assert_eq!(kept, vec![("b", 2)]);
    }

    #[test]
    fn dedup_empty_existing_keeps_all() {
        use std::collections::HashSet;
        let bundle = vec![("x", 10), ("y", 20)];
        let existing: HashSet<u32> = HashSet::new();
        let (kept, skipped) = dedup_by_id(bundle.clone(), &existing, |t| t.1);
        assert_eq!(skipped, 0);
        assert_eq!(kept, bundle);
    }

    #[test]
    fn dedup_all_existing_keeps_none() {
        use std::collections::HashSet;
        let bundle = vec![("x", 10), ("y", 20)];
        let existing: HashSet<u32> = [10, 20].into_iter().collect();
        let (kept, skipped) = dedup_by_id(bundle, &existing, |t| t.1);
        assert_eq!(skipped, 2);
        assert!(kept.is_empty());
    }
}
