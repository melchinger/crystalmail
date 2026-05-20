// Auto-Extraction von Contact-Stammdaten aus Mail-Signaturen via pi.
//
// Flow:
//   1. Envelope laden (gibt uns From-Adresse + Account + body_cached-Flag).
//   2. Skip wenn From eine eigene Account-Adresse oder Alias ist —
//      Self-Mail soll keinen Self-Contact erzeugen.
//   3. Skip wenn schon ein Contact für die From-Adresse existiert (UI
//      ruft das eigentlich nur an wenn "Lookup = HistoryOnly" zurückkam,
//      aber wir prüfen defensiv).
//   4. Skip wenn ein extraction_misses-Eintrag existiert UND keine
//      neuere Mail von dieser Adresse seit dem Versuch eingetroffen
//      ist (Cache).
//   5. Body holen (cached oder via IMAP fetch). Body-Text wird gegen
//      eine grobe Heuristik abgeschnitten — nur die letzten ~3000
//      Zeichen werden an pi geschickt, wo Signaturen typisch leben.
//   6. pi-Prompt: "Extract structured contact info as JSON".
//   7. Parse: wenn ein Mindest-Set an Feldern (`name` + min. 1 von
//      org/phone/mobile/street) drinnen ist → Contact anlegen.
//      Sonst → extraction_misses-Eintrag.
//
// Die ganze Pipeline ist async und blockiert nicht: das Tauri-Command
// awaitet auf das Ergebnis weil das UI sofort den neuen Contact-Status
// reflektieren will, aber der Pi-Subprozess läuft im Hintergrund.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tauri::AppHandle;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::domain::contact::{Contact, ContactId, ContactOrigin};
use crate::domain::message::MessageId;
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::queries;

/// Wieviele Zeichen Body-Text wir an pi schicken. Signaturen leben
/// fast immer in den letzten paar hundert Zeichen — 3000 ist großzügig
/// genug für lange Disclaimer aber reduziert Token-Cost um ~10x.
const BODY_TAIL_BYTES: usize = 3000;

/// Pi-Timeout: extraction ist eine simple JSON-Aufgabe, sollte unter
/// 30s sein. 60s als Sicherheitsnetz für lokale CPUs mit großen
/// Modellen.
const PI_TIMEOUT_SECS: u64 = 60;

/// Pi-Prompt — ein einziger System-Turn, knappe Schema-Vorgabe.
///
/// Beim Bauen wird optional eine Tag-Liste eingespeist. Wenn der User
/// noch keine Tags hat, lässt der Caller den `tags`-Block ganz weg
/// (siehe `build_extract_prompt`).
const EXTRACT_PROMPT_BASE: &str = r#"Aus der folgenden E-Mail extrahiere die Kontaktdaten des ABSENDERS aus seiner Signatur (typischerweise unten, oft nach "--", "Mit freundlichen Grüßen", "Best regards" o.ä.).

Antworte AUSSCHLIESSLICH mit einem einzigen gültigen JSON-Objekt in EXAKT dieser Form:
{
  "name": "...",
  "organization": "...",
  "job_title": "...",
  "phone": "...",
  "mobile": "...",
  "street": "...",
  "zip": "...",
  "city": "...",
  "country": "...",
  "website": "..."__TAGS_FIELD__
}

Felder ohne erkennbare Daten als leeren String "". Keine Erläuterungen, kein Markdown — nur das JSON-Objekt.__TAGS_INSTRUCTION__"#;

/// Baut den Prompt mit eingespeister Tag-Liste. Wenn `available_tags`
/// leer ist, wird der `tags`-Block ganz weggelassen — wir wollen den
/// Pi nicht mit einem leeren Array-Vokabular aus der Bahn werfen.
fn build_extract_prompt(available_tags: &[String]) -> String {
    if available_tags.is_empty() {
        return EXTRACT_PROMPT_BASE
            .replace("__TAGS_FIELD__", "")
            .replace("__TAGS_INSTRUCTION__", "");
    }
    let tags_csv = available_tags
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let field = ",\n  \"tags\": []";
    let instruction = format!(
        "\n\nVerfügbare Tags (kontrolliertes Vokabular): [{tags_csv}]\n\
         Wähle aus diesen Tags AUSSCHLIESSLICH die, die zum Absender passen \
         (basierend auf Mail-Inhalt UND Signatur). Liefere ein Array mit \
         exakt geschriebenen Tag-Namen aus der Liste — keine neuen Tags \
         erfinden. Wenn keiner passt: leeres Array []."
    );
    EXTRACT_PROMPT_BASE
        .replace("__TAGS_FIELD__", field)
        .replace("__TAGS_INSTRUCTION__", &instruction)
}

/// Custom-Deserializer: nimmt `null`, fehlende Felder UND falsche Typen
/// (Number, Bool …) und mappt sie alle auf einen leeren String. pi-
/// Outputs aus kleineren Modellen variieren da wild — gemma3 liefert
/// gerne `"phone": null`, llama3 gerne `"phone": "<unknown>"`-Strings.
/// Wir wollen das DOWN-converten ohne dass der ganze Parse-Pfad knallt.
fn null_to_empty<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    // serde_json::Value ist tolerant — schluckt jeden JSON-Typ. Hinterher
    // entscheiden wir was draus wird.
    let v = serde_json::Value::deserialize(de)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Null => Ok(String::new()),
        // Number / Bool: gibt's selten, aber dann als Display-String
        // weiterreichen (z.B. PLZ kommt manchmal als Zahl).
        other => Ok(other.to_string().trim_matches('"').to_string()),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedFields {
    #[serde(default, deserialize_with = "null_to_empty")]
    pub name: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub organization: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub job_title: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub phone: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub mobile: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub street: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub zip: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub city: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub country: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub website: String,
    /// Vom pi vorgeschlagene Tags. Wir matchen die nachträglich gegen
    /// die existierende `tags`-Tabelle (case-insensitive) und linken
    /// nur exakte Treffer — pi soll keine neuen Tags erfinden, sondern
    /// nur aus dem User-Vokabular wählen.
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub tags: Vec<String>,
}

fn null_to_empty_vec<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let v = serde_json::Value::deserialize(de)?;
    match v {
        serde_json::Value::Array(arr) => Ok(arr
            .into_iter()
            .filter_map(|x| match x {
                serde_json::Value::String(s) => Some(s),
                _ => None,
            })
            .collect()),
        _ => Ok(Vec::new()),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtractionResult {
    /// Erfolg: neuer (oder bestehender) Contact.
    Created { contact_id: String, fields: ExtractedFields },
    /// Pi konnte nichts brauchbares finden — extraction_miss persistiert.
    Empty,
    /// Bereits vorhandener Contact für die From-Adresse — kein Re-Extract.
    AlreadyExists { contact_id: String },
    /// Mail hat keine From-Adresse oder ist Self-Mail — nicht extrahierbar.
    NotApplicable { reason: String },
    /// Cache-Hit: kürzlich erfolglos versucht, neuere Mail noch nicht
    /// eingetroffen. UI kann das als "Schon mal probiert, nichts da"
    /// anzeigen.
    Skipped { reason: String },
}

pub async fn extract_for_message(
    app: AppHandle,
    db: DbHandle,
    message_id: MessageId,
) -> Result<ExtractionResult, String> {
    // ── 1. Envelope laden + Validate-Step ──────────────────────────
    let envelope = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_envelope(&conn, &message_id)
            .map_err(|e| e.to_string())?
            .ok_or("envelope not found")?
    };
    let from = envelope.from.first().cloned();
    let Some(from) = from else {
        return Ok(ExtractionResult::NotApplicable {
            reason: "Mail hat keine From-Adresse".into(),
        });
    };
    let from_email = from.email.trim().to_lowercase();
    if from_email.is_empty() || !from_email.contains('@') {
        return Ok(ExtractionResult::NotApplicable {
            reason: "From-Adresse ist leer oder ungültig".into(),
        });
    }

    // ── 2. Self-Mail-Check ────────────────────────────────────────
    {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let is_self: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM accounts WHERE lower(address) = ?1
                    UNION ALL
                    SELECT 1 FROM account_aliases WHERE lower(email) = ?1
                 )",
                rusqlite::params![from_email],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if is_self {
            return Ok(ExtractionResult::NotApplicable {
                reason: "Eigene Adresse — kein Auto-Extract".into(),
            });
        }
    }

    // ── 3. Bereits Contact vorhanden? ──────────────────────────────
    {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let lookup = queries::contact_lookup_for_email(&conn, &from_email)
            .map_err(|e| e.to_string())?;
        if let crate::domain::contact::ContactLookup::Contact { contact } = lookup {
            return Ok(ExtractionResult::AlreadyExists {
                contact_id: contact.id.0.to_string(),
            });
        }
    }

    // ── 4. Cache-Check: extraction_misses ──────────────────────────
    let cached_miss: Option<String> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT last_attempted_envelope_id FROM extraction_misses WHERE email = ?1",
            rusqlite::params![from_email],
            |r| r.get::<_, String>(0),
        )
        .ok()
    };
    if let Some(last_attempted_id) = cached_miss {
        // "Hat sich seither was Neueres eingebucht?" — neuere Mail
        // = neuere envelope.id (UUID, monoton ist's nicht, aber wir
        // vergleichen über das Datum).
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let date_pair: Option<(String, String)> = conn
            .query_row(
                "SELECT
                    (SELECT date_utc FROM envelopes WHERE id = ?1),
                    (SELECT MAX(date_utc) FROM envelopes e, json_each(e.from_json) addr
                     WHERE lower(json_extract(addr.value, '$.email')) = ?2)",
                rusqlite::params![last_attempted_id, from_email],
                |r| Ok((r.get(0).unwrap_or_default(), r.get(1).unwrap_or_default())),
            )
            .ok();
        if let Some((last_attempted_date, latest_date)) = date_pair {
            if !last_attempted_date.is_empty() && latest_date <= last_attempted_date {
                return Ok(ExtractionResult::Skipped {
                    reason: "Vor kurzem erfolglos versucht, keine neuere Mail seither".into(),
                });
            }
        }
    }

    // ── 5. Body holen (cached oder fetch) ─────────────────────────
    let body_text = if envelope.body_cached {
        match crate::application::body::cached(&db, &message_id)
            .map_err(|e| format!("body cached: {e}"))?
        {
            Some(b) => b
                .plain_text
                .or(b.html_text)
                .unwrap_or_default(),
            None => String::new(),
        }
    } else {
        // Lazy-fetch: dieselbe Pipeline wie der Reader.
        let parsed = crate::application::body::fetch_and_store(&app, &db, message_id)
            .await
            .map_err(|e| format!("body fetch: {e}"))?;
        parsed.plain.or(parsed.html).unwrap_or_default()
    };

    // Tail nehmen — Signatur ist immer am Ende.
    let body_for_pi = if body_text.len() > BODY_TAIL_BYTES {
        let start = body_text.len() - BODY_TAIL_BYTES;
        // Char-boundary-safe: zum nächsten utf-8-grenz-byte vorrücken.
        let mut idx = start;
        while idx < body_text.len() && !body_text.is_char_boundary(idx) {
            idx += 1;
        }
        format!("...\n{}", &body_text[idx..])
    } else {
        body_text
    };

    if body_for_pi.trim().is_empty() {
        // Body leer → nichts zu extrahieren, aber kein Fehler. Cache-Miss
        // damit wir nicht beim nächsten Mail-Open dieselbe leere Body-
        // Pipeline wieder anwerfen.
        record_miss(&db, &from_email, &message_id).await;
        return Ok(ExtractionResult::Empty);
    }

    // ── 6. Pi-Call ─────────────────────────────────────────────────
    // Verfügbare Tags ins Prompt — pi schlägt aus diesem Vokabular
    // vor, statt eigene zu erfinden.
    let available_tags: Vec<crate::domain::contact::Tag> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_tags(&conn).map_err(|e| e.to_string())?
    };
    let tag_names: Vec<String> = available_tags.iter().map(|t| t.name.clone()).collect();
    let prompt_template = build_extract_prompt(&tag_names);
    let prompt = format!(
        "{}\n\n--- BEGIN E-MAIL ---\n{}\n--- END E-MAIL ---",
        prompt_template, body_for_pi
    );
    let json_text = call_pi(&app, prompt, PI_TIMEOUT_SECS).await?;

    // ── 7. Parse + Persist ─────────────────────────────────────────
    let extracted = parse_extracted_json(&json_text)?;
    if !is_useful(&extracted) {
        record_miss(&db, &from_email, &message_id).await;
        return Ok(ExtractionResult::Empty);
    }

    let now = Utc::now();
    let contact = Contact {
        id: ContactId(Uuid::new_v4()),
        display_name: if extracted.name.trim().is_empty() {
            // Fallback auf Header-Display-Name oder den Local-Part.
            from.name.clone().unwrap_or_else(|| {
                from_email.split('@').next().unwrap_or(&from_email).to_string()
            })
        } else {
            extracted.name.trim().to_string()
        },
        organization: nonempty(&extracted.organization),
        job_title: nonempty(&extracted.job_title),
        phone: nonempty(&extracted.phone),
        mobile: nonempty(&extracted.mobile),
        street: nonempty(&extracted.street),
        zip: nonempty(&extracted.zip),
        city: nonempty(&extracted.city),
        country: nonempty(&extracted.country),
        website: nonempty(&extracted.website),
        notes: String::new(),
        origin: ContactOrigin::Extracted,
        pinned: false,
        last_extracted_envelope_id: Some(message_id.0.to_string()),
        created_at: now,
        updated_at: now,
    };
    let new_id = contact.id;

    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::CreateContact {
            contact,
            initial_email: Some(from_email),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| e.to_string())?;

    // Tag-Auto-Link: pi-Vorschläge gegen die User-Vokabel-Liste matchen
    // (case-insensitive, exakter Name) und nur Treffer linken. Wir
    // erfinden hier KEINE neuen Tags — wäre eine zu starke Autorität
    // dem Modell zu geben.
    if !extracted.tags.is_empty() {
        let matched_ids: Vec<crate::domain::contact::TagId> = extracted
            .tags
            .iter()
            .filter_map(|suggested| {
                available_tags
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(suggested.trim()))
                    .map(|t| t.id)
            })
            .collect();
        if !matched_ids.is_empty() {
            let (tx, rx) = oneshot::channel();
            let _ = db
                .writer
                .send(WriteCmd::ReplaceContactTags {
                    contact_id: new_id,
                    tag_ids: matched_ids,
                    ack: tx,
                })
                .await;
            // Tag-Link-Fehler ist nicht-fatal: Contact wurde schon
            // angelegt, der User kann die Tags manuell setzen.
            if let Ok(Err(e)) = rx.await {
                tracing::warn!(error = %e, "tag-auto-link nach extract fehlgeschlagen");
            }
        }
    }

    Ok(ExtractionResult::Created {
        contact_id: new_id.0.to_string(),
        fields: extracted,
    })
}

/// Pi-Call mit Timeout. Wir nutzen die globale pi_config aus AppState
/// statt einer eigenen — sonst hätten wir N parallel-laufende pi-
/// Subprozesse mit unterschiedlichem State.
///
/// Pub damit `event_extract` und andere extraction-Pipelines denselben
/// Pi-Pfad teilen. `timeout_secs` ist per-Caller, weil verschiedene
/// Tasks unterschiedliche Input-Größen + Token-Budgets haben
/// (Termin-Extraktion füttert mehr Body als Kontakt-Extraktion).
pub async fn call_pi(
    app: &AppHandle,
    prompt: String,
    timeout_secs: u64,
) -> Result<String, String> {
    use tauri::Manager;
    let state = app.state::<crate::state::AppState>();
    let cfg = {
        let guard = state
            .pi_config
            .lock()
            .map_err(|_| "pi_config lock poisoned".to_string())?;
        guard.clone()
    };

    let rpc = crate::llm::pi_rpc::PiRpc::spawn(app.clone(), &cfg).await?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        rpc.prompt_collect(prompt),
    )
    .await;

    match result {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(format!("pi-Call: {e}")),
        Err(_) => Err(format!(
            "pi-Call dauerte länger als {timeout_secs}s — Modell zu groß?"
        )),
    }
}

/// JSON aus pi-Antwort isolieren. Pi kann gerade bei kleineren Modellen
/// Vor- und Nachtext um das eigentliche Objekt drumrum produzieren —
/// wir suchen das erste balancierte `{...}` und parsen dasselbe.
fn parse_extracted_json(raw: &str) -> Result<ExtractedFields, String> {
    let json_str = first_balanced_object(raw).ok_or_else(|| {
        format!(
            "pi-Antwort enthält kein JSON-Objekt:\n---\n{}\n---",
            raw.trim()
        )
    })?;
    let parsed: JsonValue = serde_json::from_str(&json_str)
        .map_err(|e| format!("pi-JSON nicht parsebar: {e}\n{json_str}"))?;
    let extracted: ExtractedFields = serde_json::from_value(parsed)
        .map_err(|e| format!("pi-JSON Schema-Fehler: {e}"))?;
    Ok(extracted)
}

/// Extrahiert das erste balancierte `{...}` aus pi-Antworten. Pub damit
/// andere extraction-Pipelines (event_extract o.a.) denselben
/// Tolerance-Layer für vor/nach-Text und String-Escapes nutzen können.
pub fn first_balanced_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut start: Option<usize> = None;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s_idx) = start {
                        return Some(s[s_idx..=i].to_string());
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// "Brauchbar" = mindestens ein Name UND mindestens ein
/// Adress-/Org-/Phone-Feld. Verhindert dass jede 1-Zeilen-Mail mit
/// `Mit Gruß, Anna` einen Müll-Contact erzeugt.
fn is_useful(e: &ExtractedFields) -> bool {
    if e.name.trim().is_empty() {
        return false;
    }
    !e.organization.trim().is_empty()
        || !e.phone.trim().is_empty()
        || !e.mobile.trim().is_empty()
        || !e.street.trim().is_empty()
        || !e.city.trim().is_empty()
}

fn nonempty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

async fn record_miss(db: &DbHandle, email: &str, envelope_id: &MessageId) {
    let (tx, rx) = oneshot::channel();
    let _ = db
        .writer
        .send(WriteCmd::RecordExtractionMiss {
            email: email.to_string(),
            envelope_id: *envelope_id,
            ack: tx,
        })
        .await;
    let _ = rx.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_object_simple() {
        let raw = r#"Sure, here's the JSON: { "name": "Alice" }
End of message."#;
        assert_eq!(
            first_balanced_object(raw).as_deref(),
            Some(r#"{ "name": "Alice" }"#)
        );
    }

    #[test]
    fn balanced_object_nested() {
        let raw = r#"prefix {"a": {"b": "c"}, "d": "e"} suffix"#;
        assert_eq!(
            first_balanced_object(raw).as_deref(),
            Some(r#"{"a": {"b": "c"}, "d": "e"}"#)
        );
    }

    #[test]
    fn balanced_object_strings_with_braces() {
        let raw = r#"{"phone": "+49 {123}", "name": "X"}"#;
        assert_eq!(first_balanced_object(raw).as_deref(), Some(raw));
    }

    #[test]
    fn balanced_object_unbalanced_returns_none() {
        let raw = r#"{ "broken": "without close""#;
        assert_eq!(first_balanced_object(raw), None);
    }

    #[test]
    fn is_useful_requires_name_and_one_more() {
        let only_name = ExtractedFields {
            name: "Anna".into(),
            ..Default::default()
        };
        assert!(!is_useful(&only_name));

        let name_plus_org = ExtractedFields {
            name: "Anna".into(),
            organization: "Acme GmbH".into(),
            ..Default::default()
        };
        assert!(is_useful(&name_plus_org));
    }

    #[test]
    fn is_useful_rejects_empty_name() {
        let no_name = ExtractedFields {
            organization: "Acme GmbH".into(),
            ..Default::default()
        };
        assert!(!is_useful(&no_name));
    }

    #[test]
    fn parse_handles_null_fields() {
        // Häufiger Output von gemma3 / llama3 — null statt "" für leere
        // Felder. Vor dem custom-deserializer hat das den ganzen Parse
        // gekillt mit "invalid type: null, expected a string".
        let raw = r#"{
            "name": "Anna Schmidt",
            "organization": "Acme GmbH",
            "jobTitle": null,
            "phone": null,
            "mobile": null,
            "street": null,
            "zip": null,
            "city": "München",
            "country": null,
            "website": null
        }"#;
        let parsed = parse_extracted_json(raw).expect("must parse with nulls");
        assert_eq!(parsed.name, "Anna Schmidt");
        assert_eq!(parsed.organization, "Acme GmbH");
        assert_eq!(parsed.city, "München");
        assert_eq!(parsed.phone, "");
        assert_eq!(parsed.website, "");
    }

    #[test]
    fn parse_handles_missing_fields() {
        // Manche Modelle lassen Felder einfach weg statt sie als null
        // zu deklarieren.
        let raw = r#"{"name": "Bob", "organization": "Co"}"#;
        let parsed = parse_extracted_json(raw).expect("must parse with missing");
        assert_eq!(parsed.name, "Bob");
        assert_eq!(parsed.organization, "Co");
        assert_eq!(parsed.phone, "");
    }

    #[test]
    fn parse_handles_numeric_zip() {
        // Manche Modelle liefern PLZ als Zahl statt String.
        let raw = r#"{"name": "X", "organization": "Y", "zip": 71126}"#;
        let parsed = parse_extracted_json(raw).expect("must parse numeric zip");
        assert_eq!(parsed.zip, "71126");
    }
}

impl Default for ExtractedFields {
    fn default() -> Self {
        Self {
            name: String::new(),
            organization: String::new(),
            job_title: String::new(),
            phone: String::new(),
            mobile: String::new(),
            street: String::new(),
            zip: String::new(),
            city: String::new(),
            country: String::new(),
            website: String::new(),
            tags: Vec::new(),
        }
    }
}
