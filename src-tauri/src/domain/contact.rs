// Domain-Typen für das zwei-Schicht-Adressbuch:
//
//   * `AddressHistoryEntry` — pure Recency+Frequency, befüllt aus Sync.
//     Versorgt die Compose-Autocomplete und sonst nichts.
//   * `Contact` + `ContactEmail` — kuratierte/extrahierte Personen mit
//     strukturierten Adressdaten. Das ist das, was der User als
//     "Adressbuch" sieht.
//
// Die Membership-Beziehung "diese E-Mail-Adresse → dieser Contact"
// hängt am `contact_emails`-Tisch (1:N), damit ein Contact mehrere
// Adressen haben kann (Privat + Arbeit + alte Domain). Address-History-
// Einträge und Contacts sind unabhängig: eine Adresse kann in der
// History auftauchen ohne dass ein Contact existiert; ein Contact
// kann manuell angelegt sein, ohne dass die History je einen Eintrag
// hatte.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ContactId(pub Uuid);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TagId(pub Uuid);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    pub color: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Schreib-Modell für die address_history-Tabelle. Reads sind in der
/// Regel der schmalere `AddressCompletion`-Typ unten — nur die Felder
/// die das Autocomplete-UI tatsächlich anzeigt.
///
/// Aktuell läuft der Schreibpfad direkt aus dem Sync-Loop heraus mit
/// SQL-INSERTs (siehe `db_ops::upsert_address_history`); dieser Typ
/// ist die geplante Domain-Repräsentation für einen späteren
/// Refactor, in dem WriteCmds dann nicht mehr rohe Spalten tragen.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AddressHistoryEntry {
    pub email: String,
    pub display_name: Option<String>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub send_count: i64,
    pub recv_count: i64,
    pub is_role: bool,
}

/// Read-Modell fürs Compose-Autocomplete. Gerankt nach
/// `(send_count*3 + recv_count) DESC, last_seen_at DESC`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressCompletion {
    pub email: String,
    pub display_name: Option<String>,
    /// Falls für die Adresse ein Contact existiert: dessen ID.
    /// UI nutzt das um statt der nackten E-Mail einen Personen-Eintrag
    /// mit Name/Org anzuzeigen.
    pub contact_id: Option<ContactId>,
    /// Display-Name aus dem Contact (überschreibt history.display_name
    /// falls vorhanden — das ist der vom User kuratierte Name).
    pub contact_display_name: Option<String>,
    pub send_count: i64,
    pub recv_count: i64,
    pub last_seen_at: DateTime<Utc>,
}

/// Voll-Struktur für DB-Roundtrips. Frontend bekommt eine flachere
/// `ContactDetail`-Variante (s. unten) die zusätzlich die Liste der
/// Emails dabei hat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub id: ContactId,
    pub display_name: String,
    pub organization: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub mobile: Option<String>,
    pub street: Option<String>,
    pub zip: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub website: Option<String>,
    pub notes: String,
    pub origin: ContactOrigin,
    pub pinned: bool,
    pub last_extracted_envelope_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContactOrigin {
    /// Manuell vom User angelegt.
    User,
    /// Per pi-Prompt aus einer Mail-Signatur extrahiert.
    Extracted,
}

impl ContactOrigin {
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "extracted" => Self::Extracted,
            _ => Self::User,
        }
    }
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Extracted => "extracted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactEmail {
    /// Auto-Increment-ID, vom DB vergeben — Frontend braucht das
    /// nicht zu kennen, aber der Update-Pfad braucht's.
    pub id: i64,
    pub contact_id: ContactId,
    pub email: String,
    pub is_primary: bool,
}

/// Frontend-Read-Modell mit eingebetteten Adressen + Stats + Tags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactDetail {
    #[serde(flatten)]
    pub contact: Contact,
    pub emails: Vec<ContactEmail>,
    pub tags: Vec<Tag>,
    /// Anzahl Mails über alle assoziierten Adressen. Für die UI-Liste
    /// als Frequency-Hint.
    pub message_count: i64,
    /// Datum der jüngsten Mail über alle assoziierten Adressen.
    pub last_message_at: Option<DateTime<Utc>>,
}

/// Frontend-Read-Modell für die Listen-View. Schlanker als ContactDetail
/// damit eine 500-Kontakte-Liste nicht unnötig fett über die Tauri-
/// Bridge wandert.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactSummary {
    pub id: ContactId,
    pub display_name: String,
    pub organization: Option<String>,
    pub city: Option<String>,
    pub primary_email: Option<String>,
    pub pinned: bool,
    pub message_count: i64,
    pub last_message_at: Option<DateTime<Utc>>,
}

/// Tauri-Form für Create/Update — Notes/Email-Liste werden separat
/// verwaltet (`add_contact_email` / `remove_contact_email`), das hier
/// ist nur das Personen-Stammdatenset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactForm {
    pub display_name: String,
    pub organization: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub mobile: Option<String>,
    pub street: Option<String>,
    pub zip: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub website: Option<String>,
    pub notes: String,
    pub pinned: bool,
}

/// Role-Address-Heuristik: identifiziert no-reply / mailing-list /
/// bounces / notification-Adressen, die wir aus der Compose-Autocomplete
/// raushalten wollen. Konservativ gehalten — `info@`, `admin@`, `support@`
/// etc. sind oft echte Personen-Postfächer und werden NICHT geflaggt.
///
/// Identisch zu den LIKE-Patterns in `migrations/0021_contacts.sql`,
/// damit Backfill und Live-Side-Effect dieselbe Klassifizierung liefern.
pub fn is_role_address(email: &str) -> bool {
    let lower = email.to_lowercase();
    let lower = lower.trim();
    const ROLE_PREFIXES: &[&str] = &[
        "noreply@",
        "no-reply@",
        "donotreply@",
        "donot-reply@",
        "do-not-reply@",
        "mailer-daemon@",
        "postmaster@",
        "bounce@",
        "bounces@",
        "notification@",
        "notifications@",
        "newsletter@",
    ];
    if ROLE_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    if let Some((local, _)) = lower.split_once('@') {
        // VERP / Mailing-list patterns: `*-bounces@` und `reply+token@`.
        if local.ends_with("-bounces") || local.starts_with("reply+") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_addresses_recognized() {
        assert!(is_role_address("noreply@example.com"));
        assert!(is_role_address("No-Reply@example.com")); // case-insensitive
        assert!(is_role_address("listname-bounces@lists.example.com"));
        assert!(is_role_address("reply+abc123token@notification.github.com"));
        assert!(is_role_address("mailer-daemon@mail.example.com"));
    }

    #[test]
    fn personal_addresses_not_flagged() {
        assert!(!is_role_address("alice@example.com"));
        assert!(!is_role_address("info@firma.de")); // konservativ: könnte echtes Postfach sein
        assert!(!is_role_address("admin@kunde.com"));
        assert!(!is_role_address("support@helpdesk.de"));
    }
}

/// Status für die Reader-Header-UI. Gibt dem Frontend die Information
/// zum Person-Icon: existiert ein Contact für diese Adresse, oder ist
/// nur die History dafür da, oder noch nichts?
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContactLookup {
    /// Voller Contact für die Adresse vorhanden.
    Contact { contact: Contact },
    /// Adresse ist in der History, aber kein Contact (UI bietet
    /// "Kontakt anlegen"-Button an, der ggf. die Auto-Extraction
    /// triggert).
    HistoryOnly {
        display_name: Option<String>,
        send_count: i64,
        recv_count: i64,
    },
    /// Adresse ist komplett unbekannt — frische Mail, noch nicht
    /// gesynct etc.
    Unknown,
}
