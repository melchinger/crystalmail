use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth::AuthCredential;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountId(pub Uuid);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImapEndpoint {
    pub host: String,
    pub port: u16,
    /// true = implicit TLS (port 993), false = STARTTLS
    pub tls: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpEndpoint {
    pub host: String,
    /// Submission port: 587 (STARTTLS) or 465 (implicit TLS).
    pub port: u16,
    pub tls: bool,
}

/// Wie das Konto auf neue Mails reagiert. `Idle` ist Empfehlung und Default
/// für frisch angelegte Accounts: eine persistente IMAP-Verbindung pro Konto
/// hält die INBOX im IDLE-Mode, der Server pusht `EXISTS`/`EXPUNGE` Events,
/// sobald sich was ändert. Ressourcenschonend und latenzarm.
///
/// `Polling` läuft ohne IDLE-Verbindung und syncht stattdessen die INBOX
/// alle paar Minuten neu — Fallback für Provider, die IDLE nicht oder
/// schlecht beherrschen, oder für Setups mit zickigen Firewalls/NATs die
/// die langlebige IMAP-Connection nach kurzer Zeit killen.
///
/// `IdleAndPolling` macht beides parallel — IDLE als Push-Primärweg,
/// Polling als Sicherheitsnetz. Sinnvoll wenn IDLE meist tut, aber
/// gelegentlich Pakete verschluckt.
///
/// Manuelle `Refresh`-Aktionen (Hotkey, UI-Button) funktionieren in allen
/// drei Modi unverändert — die hängen nicht am Actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Idle,
    Polling,
    IdleAndPolling,
}

impl Default for SyncMode {
    fn default() -> Self {
        SyncMode::Idle
    }
}

impl SyncMode {
    /// Liest aus der DB-Spalte (TEXT, CHECK-Constraint pinnt die Werte).
    /// Unbekannte Strings fallen still auf den Default zurück, weil eine
    /// kaputte Spalte den Account nicht unbenutzbar machen soll.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "polling" => SyncMode::Polling,
            "idle_and_polling" => SyncMode::IdleAndPolling,
            _ => SyncMode::Idle,
        }
    }

    pub fn as_db_str(self) -> &'static str {
        match self {
            SyncMode::Idle => "idle",
            SyncMode::Polling => "polling",
            SyncMode::IdleAndPolling => "idle_and_polling",
        }
    }

    /// Soll für dieses Konto ein langlebiger IDLE-Actor laufen?
    pub fn uses_idle(self) -> bool {
        matches!(self, SyncMode::Idle | SyncMode::IdleAndPolling)
    }

    /// Soll für dieses Konto ein periodischer Polling-Timer laufen?
    pub fn uses_polling(self) -> bool {
        matches!(self, SyncMode::Polling | SyncMode::IdleAndPolling)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: AccountId,
    /// Display name shown in the account switcher / unified-inbox colorbar.
    pub display_name: String,
    /// Full email address used as default From and for server login.
    pub address: String,
    /// Human name prepended to the From header when composing.
    pub from_name: String,
    /// Per-account hex color for unified-inbox accents.
    pub color: String,
    /// Optional signature appended to outgoing mail (plain text).
    pub signature: Option<String>,
    /// Rich HTML signature. When set, treated as authoritative; `signature`
    /// is used as the text/plain alternative (stripped if the user didn't
    /// maintain a separate plain version).
    pub signature_html: Option<String>,
    pub imap: ImapEndpoint,
    pub smtp: SmtpEndpoint,
    pub credential: AuthCredential,
    /// Canonical folder paths per account. Different providers use different
    /// names (e.g. `[Gmail]/All Mail`, `INBOX.Sent`, `Gesendet`) — all
    /// user-configurable.
    pub archive_folder: String,
    pub sent_folder: String,
    pub drafts_folder: String,
    pub trash_folder: String,
    pub spam_folder: String,
    /// When true, replying to a message in this account automatically
    /// archives the parent after `\Answered` is set. Purely a workflow
    /// toggle — forwards are not affected.
    #[serde(default)]
    pub archive_on_reply: bool,
    /// Background-prefetch window in days. 0 disables prefetch.
    #[serde(default = "default_prefetch_days")]
    pub prefetch_days: i64,
    /// Wie der Background-Sync läuft (IDLE / Polling / beides).
    #[serde(default)]
    pub sync_mode: SyncMode,
    /// Speichert der SMTP-Server gesendete Mails automatisch im
    /// IMAP-Sent-Ordner ab? Bei `true` skippt unser Send-Pfad das
    /// zusätzliche IMAP-APPEND, sonst hätten wir doppelte Sent-Einträge.
    /// Wert wird beim Account-Setup via Probe-Mail ermittelt und kann
    /// vom User in den Konten-Einstellungen manuell überschrieben werden.
    #[serde(default)]
    pub server_stores_sent: bool,
}

fn default_prefetch_days() -> i64 {
    2
}

/// Extra "From" identities attached to an account. Always share the same
/// login credentials as the parent — servers accept sending under any
/// address the mailbox owns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountAlias {
    pub id: Uuid,
    pub account_id: AccountId,
    pub email: String,
    pub from_name: String,
}
