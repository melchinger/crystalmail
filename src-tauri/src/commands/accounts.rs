// Account management commands. The Tauri boundary deliberately separates the
// plaintext password in `NewAccountForm` (only ever held in memory for the
// duration of the add call) from what's persisted: the password goes into the
// OS keyring, the DB stores only a pointer.

use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::domain::account::{Account, AccountAlias, AccountId, ImapEndpoint, SmtpEndpoint};
use crate::domain::auth::AuthCredential;
use crate::infrastructure::db::WriteCmd;
use crate::infrastructure::imap_client::{self, DiscoveredFolders, VerboseReport};
use crate::infrastructure::queries::{self, AccountSummary};
use crate::state::AppState;

const KEYRING_SERVICE: &str = "crystalmail";

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AliasForm {
    pub email: String,
    pub from_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAccountForm {
    pub display_name: String,
    pub address: String,
    pub from_name: String,
    pub color: String,
    pub signature: Option<String>,
    #[serde(default)]
    pub signature_html: Option<String>,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub archive_folder: String,
    #[serde(default = "default_sent_folder")]
    pub sent_folder: String,
    #[serde(default = "default_drafts_folder")]
    pub drafts_folder: String,
    #[serde(default = "default_trash_folder")]
    pub trash_folder: String,
    #[serde(default = "default_spam_folder")]
    pub spam_folder: String,
    #[serde(default)]
    pub archive_on_reply: bool,
    #[serde(default = "default_prefetch_days")]
    pub prefetch_days: i64,
    #[serde(default)]
    pub sync_mode: crate::domain::account::SyncMode,
    /// Optional override: wenn None, ermittelt `add_account` den Wert
    /// per Probe-Mail. Wenn Some(x), wird der Probe geskippt und x
    /// direkt übernommen — für Test-Setups oder User die das Verhalten
    /// schon kennen.
    #[serde(default)]
    pub server_stores_sent: Option<bool>,
    #[serde(default)]
    pub aliases: Vec<AliasForm>,
    pub password: String,
    /// When true, skip the IMAP verify step and save the account as-is
    /// (useful for draft entries when the server is offline or creds are
    /// incomplete). The account can be verified later from its detail view.
    #[serde(default)]
    pub skip_test: bool,
}

fn default_sent_folder() -> String {
    "Sent".into()
}
fn default_drafts_folder() -> String {
    "Drafts".into()
}
fn default_trash_folder() -> String {
    "Trash".into()
}
fn default_spam_folder() -> String {
    "Spam".into()
}
fn default_prefetch_days() -> i64 {
    2
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountForm {
    pub id: AccountId,
    pub display_name: String,
    pub address: String,
    pub from_name: String,
    pub color: String,
    pub signature: Option<String>,
    #[serde(default)]
    pub signature_html: Option<String>,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub archive_folder: String,
    #[serde(default = "default_sent_folder")]
    pub sent_folder: String,
    #[serde(default = "default_drafts_folder")]
    pub drafts_folder: String,
    #[serde(default = "default_trash_folder")]
    pub trash_folder: String,
    #[serde(default = "default_spam_folder")]
    pub spam_folder: String,
    #[serde(default)]
    pub archive_on_reply: bool,
    #[serde(default = "default_prefetch_days")]
    pub prefetch_days: i64,
    #[serde(default)]
    pub sync_mode: crate::domain::account::SyncMode,
    /// User-Override für das Provider-Verhalten beim Sent-Ordner.
    /// Beim Edit (anders als beim ersten Setup) gibt's keinen erneuten
    /// automatischen Probe — der User schlägt den Wert direkt selbst um.
    #[serde(default)]
    pub server_stores_sent: bool,
    #[serde(default)]
    pub aliases: Vec<AliasForm>,
    /// `None` / empty = keep existing keyring secret; `Some("...")` = replace it.
    pub password: Option<String>,
    #[serde(default)]
    pub skip_test: bool,
}

#[tauri::command]
pub async fn test_imap(
    host: String,
    port: u16,
    user: String,
    password: String,
) -> Result<String, String> {
    imap_client::test_login(&host, port, &user, &password).await?;
    Ok(format!("OK — {host}:{port} akzeptiert den Login"))
}

#[tauri::command]
pub async fn test_imap_verbose(
    host: String,
    port: u16,
    user: String,
    password: String,
) -> VerboseReport {
    imap_client::test_login_verbose(&host, port, &user, &password).await
}

/// Probe the IMAP server for its folder layout. Used by the Account Dialog
/// to auto-fill the Archive/Sent/Drafts/Trash field values — providers use
/// all sorts of names (`Sent`, `Gesendete Objekte`, `[Gmail]/Sent Mail`,
/// `INBOX.Sent` …) and forcing the user to guess means empty unified views.
///
/// When editing an existing account, `password` may be empty; we then fall
/// back to the keyring secret.
#[tauri::command]
pub async fn discover_folders(
    app: AppHandle,
    host: String,
    port: u16,
    user: String,
    password: String,
    account_id: Option<AccountId>,
) -> Result<DiscoveredFolders, String> {
    let resolved_password = if !password.is_empty() {
        password
    } else if let Some(id) = account_id {
        let entry_name = format!("imap::{}", id.0);
        keyring::Entry::new(KEYRING_SERVICE, &entry_name)
            .map_err(|e| format!("keyring::Entry::new: {e}"))?
            .get_password()
            .map_err(|e| format!("keyring get: {e}"))?
    } else {
        return Err("Passwort fehlt — für Auto-Erkennung bitte eingeben.".into());
    };
    let _ = app; // AppHandle is accepted for future extensibility; not used yet.
    imap_client::discover_folders(&host, port, &user, &resolved_password).await
}

#[tauri::command]
pub async fn add_account(app: AppHandle, form: NewAccountForm) -> Result<AccountSummary, String> {
    // 1. Verify the credentials actually work — unless the caller explicitly
    //    chose to save a draft.
    if !form.skip_test {
        imap_client::test_login(&form.imap_host, form.imap_port, &form.address, &form.password)
            .await?;
    }

    // 1b. Provider-Verhalten ermitteln: speichert der SMTP-Server gesendete
    //     Mails automatisch im Sent-Ordner? Wenn ja, muss unser eigener
    //     APPEND nach jedem Send wegfallen, sonst gibt's Duplikate.
    //     Drei Quellen für den Wert (in absteigender Priorität):
    //       a) Form-Override durch User → wir glauben ihm und proben nicht
    //       b) skip_test = true (Draft-Mode) → kein Probe möglich, Default false
    //       c) Echte Probe-Mail an sich selbst → Beobachtung
    let server_stores_sent = if let Some(explicit) = form.server_stores_sent {
        explicit
    } else if form.skip_test {
        // Draft-Mode — Probe braucht funktionierenden Server, also überspringen.
        // User kann's später in den Settings manuell setzen.
        false
    } else {
        // Synthetisches AccountSummary nur für die Probe — die ID wird nicht
        // benutzt, der Probe-Helper liest nur Hosts/Ports/sent_folder.
        let probe_account = crate::infrastructure::queries::AccountSummary {
            id: AccountId(Uuid::nil()),
            display_name: form.display_name.clone(),
            address: form.address.clone(),
            from_name: form.from_name.clone(),
            color: form.color.clone(),
            signature: None,
            signature_html: None,
            archive_folder: form.archive_folder.clone(),
            sent_folder: form.sent_folder.clone(),
            drafts_folder: form.drafts_folder.clone(),
            trash_folder: form.trash_folder.clone(),
            spam_folder: form.spam_folder.clone(),
            imap_host: form.imap_host.clone(),
            imap_port: form.imap_port,
            imap_tls: form.imap_tls,
            smtp_host: form.smtp_host.clone(),
            smtp_port: form.smtp_port,
            smtp_tls: form.smtp_tls,
            archive_on_reply: form.archive_on_reply,
            prefetch_days: form.prefetch_days,
            sync_mode: form.sync_mode,
            server_stores_sent: false,
            aliases: Vec::new(),
        };
        match crate::application::smtp::probe_server_stores_sent(&probe_account, &form.password)
            .await
        {
            Ok(b) => {
                tracing::info!(
                    account = %form.address,
                    server_stores_sent = b,
                    "Probe abgeschlossen"
                );
                b
            }
            Err(e) => {
                tracing::warn!(
                    account = %form.address,
                    error = %e,
                    "Probe fehlgeschlagen — Fallback auf 'kein Auto-Save', User kann's in Settings ändern"
                );
                false
            }
        }
    };

    // 2. Stash the password in the OS keyring. The entry name embeds the
    //    account UUID so we can look it up unambiguously later.
    let id = AccountId(Uuid::new_v4());
    let entry_name = format!("imap::{}", id.0);
    let entry = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring entry: {e}"))?;
    entry
        .set_password(&form.password)
        .map_err(|e| format!("keyring set: {e}"))?;

    // 3. Build the domain record and dispatch a write.
    let account = Account {
        id,
        display_name: form.display_name,
        address: form.address,
        from_name: form.from_name,
        color: form.color,
        signature: form.signature,
        signature_html: form.signature_html,
        imap: ImapEndpoint {
            host: form.imap_host,
            port: form.imap_port,
            tls: form.imap_tls,
        },
        smtp: SmtpEndpoint {
            host: form.smtp_host,
            port: form.smtp_port,
            tls: form.smtp_tls,
        },
        credential: AuthCredential::Password {
            keyring_entry: entry_name.clone(),
        },
        archive_folder: form.archive_folder,
        sent_folder: form.sent_folder,
        drafts_folder: form.drafts_folder,
        trash_folder: form.trash_folder,
        spam_folder: form.spam_folder,
        archive_on_reply: form.archive_on_reply,
        prefetch_days: form.prefetch_days,
        sync_mode: form.sync_mode,
        // Aus dem Probe-Ergebnis weiter oben — *nicht* aus form.server_stores_sent
        // (das ist hier ein Option<bool>-Override und wurde weiter oben in den
        // Boolean `server_stores_sent` aufgelöst).
        server_stores_sent,
    };

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::AddAccount {
            account: account.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| {
            // DB insert failed — best-effort clean up the keyring entry to
            // avoid orphaned secrets.
            let _ = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
                .and_then(|e| e.delete_credential());
            format!("db insert: {e}")
        })?;

    // Aliases are stored in a side table; replace in one transaction.
    save_aliases(db, account.id, &form.aliases).await?;

    // Per-Konto-Background-Sync-Actor starten. Ab jetzt hält er (je
    // nach sync_mode) eine IDLE-Verbindung oder pollt periodisch und
    // schlägt bei Server-Pushes Alarm — siehe `application::actor`.
    crate::application::actor::spawn_one(
        app.clone(),
        db.clone(),
        &state.actor_handles,
        account.clone(),
    )
    .await;

    // 4. Re-read the account summary via the standard query path so the
    //    response already includes aliases + any trigger-derived fields.
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_account(&conn, &account.id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "account vanished after insert".into())
}

async fn save_aliases(
    db: &crate::infrastructure::db::DbHandle,
    account_id: AccountId,
    aliases: &[AliasForm],
) -> Result<(), String> {
    let records: Vec<AccountAlias> = aliases
        .iter()
        .filter(|a| !a.email.trim().is_empty())
        .map(|a| AccountAlias {
            id: Uuid::new_v4(),
            account_id,
            email: a.email.trim().to_string(),
            from_name: a.from_name.trim().to_string(),
        })
        .collect();
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::ReplaceAliases {
            account_id,
            aliases: records,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("replace aliases: {e}"))
}

#[tauri::command]
pub async fn list_accounts(app: AppHandle) -> Result<Vec<AccountSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::list_accounts(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_account(
    app: AppHandle,
    form: UpdateAccountForm,
) -> Result<AccountSummary, String> {
    // Reuse the keyring entry name (deterministic per account id). If the
    // caller provided a fresh password, rotate the stored secret; otherwise
    // leave it untouched.
    let entry_name = format!("imap::{}", form.id.0);
    if let Some(new_pw) = form.password.as_deref().filter(|p| !p.is_empty()) {
        // If caller requested a live test, use the new password right away.
        if !form.skip_test {
            imap_client::test_login(&form.imap_host, form.imap_port, &form.address, new_pw)
                .await?;
        }
        keyring::Entry::new(KEYRING_SERVICE, &entry_name)
            .map_err(|e| format!("keyring entry: {e}"))?
            .set_password(new_pw)
            .map_err(|e| format!("keyring set: {e}"))?;
    } else if !form.skip_test {
        // Password unchanged — fetch current secret from keyring and test with it.
        let current = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
            .map_err(|e| format!("keyring entry: {e}"))?
            .get_password()
            .map_err(|_| "Kein Passwort im Schlüsselbund gefunden.".to_string())?;
        imap_client::test_login(&form.imap_host, form.imap_port, &form.address, &current).await?;
    }

    let account = Account {
        id: form.id,
        display_name: form.display_name,
        address: form.address,
        from_name: form.from_name,
        color: form.color,
        signature: form.signature,
        signature_html: form.signature_html,
        imap: ImapEndpoint {
            host: form.imap_host,
            port: form.imap_port,
            tls: form.imap_tls,
        },
        smtp: SmtpEndpoint {
            host: form.smtp_host,
            port: form.smtp_port,
            tls: form.smtp_tls,
        },
        credential: AuthCredential::Password {
            keyring_entry: entry_name,
        },
        archive_folder: form.archive_folder,
        sent_folder: form.sent_folder,
        drafts_folder: form.drafts_folder,
        trash_folder: form.trash_folder,
        spam_folder: form.spam_folder,
        archive_on_reply: form.archive_on_reply,
        prefetch_days: form.prefetch_days,
        sync_mode: form.sync_mode,
        server_stores_sent: form.server_stores_sent,
    };

    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpdateAccount {
            account: account.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db update: {e}"))?;

    save_aliases(db, account.id, &form.aliases).await?;

    // Existierendem Actor Bescheid geben: Account-Daten / Sync-Modus
    // haben sich geändert. Der Actor entscheidet selbst ob ein Reconnect
    // nötig ist (z.B. neuer IMAP-Host) oder nur die internen Daten
    // ge-update't werden.
    crate::application::actor::notify_updated(
        &state.actor_handles,
        account.clone(),
    )
    .await;

    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_account(&conn, &account.id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "account vanished after update".into())
}

#[tauri::command]
pub async fn delete_account(app: AppHandle, id: AccountId) -> Result<(), String> {
    // Best-effort keyring purge. Even if the keyring entry is gone or the
    // OS refuses (user locked keychain), we still delete the DB row so the
    // account disappears from the UI.
    let entry_name = format!("imap::{}", id.0);
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &entry_name) {
        let _ = entry.delete_credential();
    }

    let state = app.state::<AppState>();

    // Background-Sync-Actor anhalten BEVOR die DB-Zeile weg ist —
    // sonst würde der Actor noch eine Iteration mit "account not found"
    // logen, wenn er gerade in `open_session_for_account` versucht das
    // Konto neu zu lesen.
    crate::application::actor::shutdown_one(&state.actor_handles, &id).await;

    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteAccount { id, ack: tx })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db delete: {e}"))?;
    Ok(())
}
