// Tauri-Commands für das Adressbuch (zwei-Schicht-Modell):
//
//   * Layer A (address_history): `list_address_completions` für die
//     Compose-Autocomplete.
//   * Layer B (contacts): CRUD + Email-Mgmt + Reader-Header-Lookup.
//     Auto-Extraction-Trigger steht separat — der Pi-Prompt-Pfad
//     läuft async im Hintergrund (siehe `application::contact_extract`).

use chrono::Utc;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::domain::contact::{
    AddressCompletion, Contact, ContactDetail, ContactForm, ContactId, ContactLookup,
    ContactOrigin, ContactSummary, Tag, TagId,
};
use crate::infrastructure::db::WriteCmd;
use crate::infrastructure::queries::{self, EnvelopeSummary};
use crate::state::AppState;

/// Compose-Autocomplete: Top-N Kandidaten für einen Prefix. Erst ab
/// `prefix.len() >= 2` sinnvoll — der Caller im Frontend gated darauf.
#[tauri::command]
pub async fn list_address_completions(
    app: AppHandle,
    prefix: String,
    limit: Option<i64>,
) -> Result<Vec<AddressCompletion>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let cap = limit.unwrap_or(8).clamp(1, 25);
    queries::list_address_completions(&conn, &prefix, cap).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_contacts(
    app: AppHandle,
    query: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<ContactSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let lim = limit.unwrap_or(200).clamp(1, 1000);
    let off = offset.unwrap_or(0).max(0);
    queries::list_contacts(&conn, query.as_deref(), lim, off).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_contact(
    app: AppHandle,
    contact_id: String,
) -> Result<ContactDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let id = Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    queries::get_contact(&conn, &ContactId(id))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contact not found".to_string())
}

/// Reader-Header-Lookup: für eine gegebene E-Mail-Adresse zurückgeben
/// ob ein Contact existiert / nur History / unknown.
#[tauri::command]
pub async fn lookup_contact_by_email(
    app: AppHandle,
    email: String,
) -> Result<ContactLookup, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::contact_lookup_for_email(&conn, &email).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_messages_for_contact(
    app: AppHandle,
    contact_id: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<EnvelopeSummary>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let id = Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    let lim = limit.unwrap_or(200).clamp(1, 1000);
    let off = offset.unwrap_or(0).max(0);
    queries::list_messages_for_contact(&conn, &ContactId(id), lim, off)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_contact(
    app: AppHandle,
    form: ContactForm,
    initial_email: Option<String>,
) -> Result<ContactDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let now = Utc::now();
    let contact = Contact {
        id: ContactId(Uuid::new_v4()),
        display_name: form.display_name,
        organization: form.organization.filter(|s| !s.is_empty()),
        job_title: form.job_title.filter(|s| !s.is_empty()),
        phone: form.phone.filter(|s| !s.is_empty()),
        mobile: form.mobile.filter(|s| !s.is_empty()),
        street: form.street.filter(|s| !s.is_empty()),
        zip: form.zip.filter(|s| !s.is_empty()),
        city: form.city.filter(|s| !s.is_empty()),
        country: form.country.filter(|s| !s.is_empty()),
        website: form.website.filter(|s| !s.is_empty()),
        notes: form.notes,
        origin: ContactOrigin::User,
        pinned: form.pinned,
        last_extracted_envelope_id: None,
        created_at: now,
        updated_at: now,
    };

    let id = contact.id;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::CreateContact {
            contact,
            initial_email,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;

    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_contact(&conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contact disappeared after insert".to_string())
}

#[tauri::command]
pub async fn update_contact(
    app: AppHandle,
    contact_id: String,
    form: ContactForm,
) -> Result<ContactDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let id_uuid =
        Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    let id = ContactId(id_uuid);

    // Bestehende Daten lesen → wir wollen origin, last_extracted_envelope_id,
    // created_at NICHT vom Form überschrieben sehen (der User editiert
    // nur Stammdaten).
    let existing = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_contact(&conn, &id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "contact not found".to_string())?
    };
    let now = Utc::now();
    let contact = Contact {
        id,
        display_name: form.display_name,
        organization: form.organization.filter(|s| !s.is_empty()),
        job_title: form.job_title.filter(|s| !s.is_empty()),
        phone: form.phone.filter(|s| !s.is_empty()),
        mobile: form.mobile.filter(|s| !s.is_empty()),
        street: form.street.filter(|s| !s.is_empty()),
        zip: form.zip.filter(|s| !s.is_empty()),
        city: form.city.filter(|s| !s.is_empty()),
        country: form.country.filter(|s| !s.is_empty()),
        website: form.website.filter(|s| !s.is_empty()),
        notes: form.notes,
        origin: existing.contact.origin,
        pinned: form.pinned,
        last_extracted_envelope_id: existing.contact.last_extracted_envelope_id,
        created_at: existing.contact.created_at,
        updated_at: now,
    };

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpdateContact { contact, ack: tx })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;

    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_contact(&conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contact disappeared after update".to_string())
}

#[tauri::command]
pub async fn delete_contact(app: AppHandle, contact_id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let id_uuid =
        Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::DeleteContact {
            contact_id: ContactId(id_uuid),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_contact_email(
    app: AppHandle,
    contact_id: String,
    email: String,
    is_primary: Option<bool>,
) -> Result<ContactDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let id_uuid =
        Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    let id = ContactId(id_uuid);
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::AddContactEmail {
            contact_id: id,
            email,
            is_primary: is_primary.unwrap_or(false),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;

    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_contact(&conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contact not found".to_string())
}

#[tauri::command]
pub async fn remove_contact_email(
    app: AppHandle,
    contact_id: String,
    email: String,
) -> Result<ContactDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let id_uuid =
        Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    let id = ContactId(id_uuid);
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::RemoveContactEmail {
            contact_id: id,
            email,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;

    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_contact(&conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contact not found".to_string())
}

#[tauri::command]
pub async fn set_primary_contact_email(
    app: AppHandle,
    contact_id: String,
    email: String,
) -> Result<ContactDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let id_uuid =
        Uuid::parse_str(&contact_id).map_err(|e| format!("invalid contact_id: {e}"))?;
    let id = ContactId(id_uuid);
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::SetPrimaryContactEmail {
            contact_id: id,
            email,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;

    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_contact(&conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contact not found".to_string())
}

/// Auto-Extraction-Trigger. Async-Pfad: ruft pi mit dem Mail-Body
/// und persistiert das Ergebnis (Contact ODER extraction_misses).
/// Liefert das Ergebnis synchron zurück damit das UI direkt
/// reagieren kann.
#[tauri::command]
pub async fn extract_contact_from_message(
    app: AppHandle,
    message_id: String,
) -> Result<crate::application::contact_extract::ExtractionResult, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let id_uuid =
        Uuid::parse_str(&message_id).map_err(|e| format!("invalid message_id: {e}"))?;
    crate::application::contact_extract::extract_for_message(
        app.clone(),
        db.clone(),
        crate::domain::message::MessageId(id_uuid),
    )
    .await
    .map_err(|e| e.to_string())
}

// ─── Tag-Commands ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_tags(app: AppHandle) -> Result<Vec<Tag>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    crate::infrastructure::queries::list_tags(&conn).map_err(|e| e.to_string())
}

/// Erstellt ein neuen Tag ODER liefert die ID des bestehenden Tags
/// mit demselben Namen (case-insensitive). UI rendert das transparent
/// als "Tag hinzufügen" — der Caller muss nicht differenzieren.
#[tauri::command]
pub async fn upsert_tag(
    app: AppHandle,
    name: String,
    color: Option<String>,
) -> Result<Tag, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(crate::infrastructure::db::WriteCmd::UpsertTag {
            name: name.clone(),
            color: color.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    let tag_id = rx
        .await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let tag = conn
        .query_row(
            "SELECT id, name, color, created_at FROM tags WHERE id = ?1",
            rusqlite::params![tag_id.0.to_string()],
            |r| {
                let id_str: String = r.get(0)?;
                Ok(Tag {
                    id: TagId(
                        Uuid::parse_str(&id_str).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                    ),
                    name: r.get(1)?,
                    color: r.get(2)?,
                    created_at: chrono::DateTime::parse_from_rfc3339(
                        &r.get::<_, String>(3)?,
                    )
                    .map(|d| d.with_timezone(&chrono::Utc))
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            },
        )
        .map_err(|e| e.to_string())?;
    Ok(tag)
}

#[tauri::command]
pub async fn update_tag(app: AppHandle, tag: Tag) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(crate::infrastructure::db::WriteCmd::UpdateTag { tag, ack: tx })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_tag(app: AppHandle, tag_id: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let id_uuid = Uuid::parse_str(&tag_id).map_err(|e| format!("invalid tag_id: {e}"))?;
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(crate::infrastructure::db::WriteCmd::DeleteTag {
            tag_id: TagId(id_uuid),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())
}

// ─── Import / Export ─────────────────────────────────────────────────

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportContactsResult {
    pub path: String,
    pub count: i64,
}

/// Sammelt alle Contacts (mit Emails + Tags) und schreibt sie als
/// VCF an `path`. Caller (Frontend) hat den Speicherpfad via Datei-
/// Dialog ausgewählt — wir berühren keinen anderen Pfad.
#[tauri::command]
pub async fn export_contacts_vcf(
    app: AppHandle,
    path: String,
) -> Result<ExportContactsResult, String> {
    let details = collect_all_details(&app).await?;
    let count = details.len() as i64;
    let vcf = crate::application::contacts_io::write_vcf(&details);
    std::fs::write(&path, vcf).map_err(|e| format!("write {path}: {e}"))?;
    Ok(ExportContactsResult { path, count })
}

#[tauri::command]
pub async fn export_contacts_csv(
    app: AppHandle,
    path: String,
) -> Result<ExportContactsResult, String> {
    let details = collect_all_details(&app).await?;
    let count = details.len() as i64;
    let csv = crate::application::contacts_io::write_csv(&details)?;
    std::fs::write(&path, csv).map_err(|e| format!("write {path}: {e}"))?;
    Ok(ExportContactsResult { path, count })
}

#[tauri::command]
pub async fn import_contacts_vcf(
    app: AppHandle,
    path: String,
) -> Result<crate::application::contacts_io::ImportReport, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
    let records = crate::application::contacts_io::parse_vcf(&raw);
    apply_imported_records(&app, records).await
}

#[tauri::command]
pub async fn import_contacts_csv(
    app: AppHandle,
    path: String,
) -> Result<crate::application::contacts_io::ImportReport, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
    let records = crate::application::contacts_io::parse_csv(&raw)?;
    apply_imported_records(&app, records).await
}

/// Helper: vollständige Detail-Records aller Contacts für Export.
async fn collect_all_details(app: &AppHandle) -> Result<Vec<ContactDetail>, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let summaries =
        crate::infrastructure::queries::list_contacts(&conn, None, 100_000, 0)
            .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(summaries.len());
    for s in summaries {
        if let Some(d) = crate::infrastructure::queries::get_contact(&conn, &s.id)
            .map_err(|e| e.to_string())?
        {
            out.push(d);
        }
    }
    Ok(out)
}

/// Atomarer-light Apply: jeden Record durchgehen, bei Email-Konflikt
/// (UNIQUE-Verletzung in contact_emails) skippen, sonst Contact +
/// Tags + Mails persistieren. Tags werden vor dem Loop einmalig
/// upsert't damit wir die ID-Mapping-Tabelle bauen können.
async fn apply_imported_records(
    app: &AppHandle,
    records: Vec<crate::application::contacts_io::ContactImport>,
) -> Result<crate::application::contacts_io::ImportReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;

    let mut report = crate::application::contacts_io::ImportReport::default();

    // Phase 1: alle Tag-Namen einmalig upsert'en. Wir holen vorher
    // die existierende Tag-Liste, alles was nicht drin ist zählen wir
    // als "tags_created".
    let tag_names_to_upsert =
        crate::application::contacts_io::distinct_tag_names(&records);
    let existing_tags = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        crate::infrastructure::queries::list_tags(&conn).map_err(|e| e.to_string())?
    };
    let existing_tag_names: std::collections::HashSet<String> = existing_tags
        .iter()
        .map(|t| t.name.to_lowercase())
        .collect();
    // tag_name (lowercased) → TagId
    let mut tag_map: std::collections::HashMap<String, crate::domain::contact::TagId> =
        existing_tags
            .iter()
            .map(|t| (t.name.to_lowercase(), t.id))
            .collect();
    for name in &tag_names_to_upsert {
        let key = name.to_lowercase();
        if tag_map.contains_key(&key) {
            continue;
        }
        let (tx, rx) = oneshot::channel();
        db.writer
            .send(crate::infrastructure::db::WriteCmd::UpsertTag {
                name: name.clone(),
                color: None,
                ack: tx,
            })
            .await
            .map_err(|_| "writer channel closed".to_string())?;
        match rx.await.map_err(|_| "writer dropped ack".to_string())? {
            Ok(id) => {
                if !existing_tag_names.contains(&key) {
                    report.tags_created += 1;
                }
                tag_map.insert(key, id);
            }
            Err(e) => {
                tracing::warn!(error = %e, name = %name, "tag upsert failed");
            }
        }
    }

    // Phase 2: Records durchgehen.
    for rec in records {
        if rec.display_name.trim().is_empty() {
            report.skipped_invalid += 1;
            continue;
        }

        // Erste Email als initial_email; wenn sie schon einem anderen
        // Contact gehört, fliegt das CreateContact mit UNIQUE-Verletzung
        // raus → wir skippen den Datensatz und vermerken das.
        let primary_email = rec.emails.first().cloned();

        let now = chrono::Utc::now();
        let new_id = uuid::Uuid::new_v4();
        let contact = crate::domain::contact::Contact {
            id: crate::domain::contact::ContactId(new_id),
            display_name: rec.display_name.trim().to_string(),
            organization: rec.organization,
            job_title: rec.job_title,
            phone: rec.phone,
            mobile: rec.mobile,
            street: rec.street,
            zip: rec.zip,
            city: rec.city,
            country: rec.country,
            website: rec.website,
            notes: rec.notes,
            origin: crate::domain::contact::ContactOrigin::User,
            pinned: false,
            last_extracted_envelope_id: None,
            created_at: now,
            updated_at: now,
        };

        let (tx, rx) = oneshot::channel();
        db.writer
            .send(crate::infrastructure::db::WriteCmd::CreateContact {
                contact,
                initial_email: primary_email.clone(),
                ack: tx,
            })
            .await
            .map_err(|_| "writer channel closed".to_string())?;
        match rx.await.map_err(|_| "writer dropped ack".to_string())? {
            Ok(()) => {
                report.created += 1;
                // Weitere Emails (ab Index 1) anhängen.
                for (i, email) in rec.emails.iter().enumerate().skip(1) {
                    let (tx, rx) = oneshot::channel();
                    let _ = db
                        .writer
                        .send(crate::infrastructure::db::WriteCmd::AddContactEmail {
                            contact_id: crate::domain::contact::ContactId(new_id),
                            email: email.clone(),
                            is_primary: i == 0,
                            ack: tx,
                        })
                        .await;
                    let _ = rx.await;
                }
                // Tags linken.
                let tag_ids: Vec<crate::domain::contact::TagId> = rec
                    .tags
                    .iter()
                    .filter_map(|t| tag_map.get(&t.to_lowercase()).copied())
                    .collect();
                if !tag_ids.is_empty() {
                    let (tx, rx) = oneshot::channel();
                    let _ = db
                        .writer
                        .send(crate::infrastructure::db::WriteCmd::ReplaceContactTags {
                            contact_id: crate::domain::contact::ContactId(new_id),
                            tag_ids,
                            ack: tx,
                        })
                        .await;
                    let _ = rx.await;
                }
            }
            Err(_) => {
                // Vermutlich UNIQUE-Konflikt auf primary_email. Falls
                // ja, vermerken; bei generic-DB-Error trotzdem als
                // skipped zählen, damit Bedingungs-Counter stimmen.
                report.skipped_existing_email += 1;
                if let Some(e) = primary_email {
                    report.skipped_addresses.push(e);
                }
            }
        }
    }

    Ok(report)
}

/// Atomarer Replace der Tag-Membership eines Contacts. Frontend schickt
/// die GEWÜNSCHTE Liste der Tag-IDs; Backend differt selbst und schreibt
/// nur was sich geändert hat (Performance-Hinweis: fast immer < 10 Tags
/// pro Contact, gespart wird hier wenig — die Atomicity ist der Grund).
#[tauri::command]
pub async fn set_contact_tags(
    app: AppHandle,
    contact_id: String,
    tag_ids: Vec<String>,
) -> Result<ContactDetail, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let cid_uuid = Uuid::parse_str(&contact_id)
        .map_err(|e| format!("invalid contact_id: {e}"))?;
    let cid = ContactId(cid_uuid);
    let parsed_tag_ids: Vec<TagId> = tag_ids
        .iter()
        .map(|s| Uuid::parse_str(s).map(TagId))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid tag_id: {e}"))?;

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(crate::infrastructure::db::WriteCmd::ReplaceContactTags {
            contact_id: cid,
            tag_ids: parsed_tag_ids,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;

    let conn = db.reads.get().map_err(|e| e.to_string())?;
    crate::infrastructure::queries::get_contact(&conn, &cid)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "contact not found".to_string())
}
