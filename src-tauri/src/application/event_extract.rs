// Auto-Extraction von Termin-Daten aus einer Mail via pi.
//
// Schwester-Modul zu `contact_extract`: gleiche Pi-Pipeline (Body holen,
// Prompt bauen, pi callen, JSON parsen), aber:
//   * persistiert NICHT — der User soll im EventEditor reviewen und
//     selbst speichern. Wir liefern nur einen `ExtractedEventDraft`
//     zurück.
//   * fokussiert auf Termin-Felder (Titel/Zeit/Ort) statt Stammdaten.
//   * der Body wird komplett (mit größerem Tail) an pi geschickt, weil
//     Termin-Hinweise irgendwo in der Mail stehen können — nicht nur
//     am Ende wie Signaturen.
//   * Meeting-URLs (Zoom/Teams/Meet/Webex/…) landen explizit in
//     `location`, nicht in `description`. Das war die expliziter User-
//     Wunsch.
//
// Timezone-Strategie: pi gibt nackte Lokalzeit `YYYY-MM-DDTHH:MM`
// zurück (kein Offset). Frontend wendet dieselbe Konvertierung an wie
// für `datetime-local`-Form-Inputs — system-lokaler Offset wird
// gestempelt. Year-Disambiguation passiert pi-seitig: wir füttern das
// Datum der Mail als Anker ("nächsten Mittwoch" → Mittwoch nach Mail-
// Datum, nicht nach `today()`).

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::domain::message::MessageId;
use crate::infrastructure::db::DbHandle;
use crate::infrastructure::queries;

/// Wieviele Bytes Body-Text an pi gehen. Termin-Hinweise stehen
/// typischerweise im Mail-Kopf (Einladungstext, Datum/Zeit, Meeting-Link),
/// selten erst am Ende — 4000 Bytes decken den relevanten Bereich ab und
/// halten die Inferenz-Last für lokale Modelle handhabbar. Frühere 8000-
/// Byte-Variante hat auf langsamen CPUs reproduzierbar in den Timeout
/// gelaufen.
const BODY_BYTES: usize = 4000;

/// Pi-Timeout. JSON-Extraktion ist eigentlich eine kleine Aufgabe, aber
/// lokale Modelle (gemma3:12b, llama3:8b auf CPU) brauchen für die
/// Prompt-Verarbeitung Zeit, bevor der erste Token rauskommt. 180s gibt
/// realistischen Setups Luft; alles darüber hinaus wäre ein zu langer
/// UI-Block — der User soll lieber das Modell verkleinern (siehe
/// Einstellungen → KI) als 5 Minuten auf eine Mail-Analyse warten.
const PI_TIMEOUT_SECS: u64 = 180;

const EXTRACT_PROMPT: &str = r#"Extrahiere Termin-Daten aus der E-Mail. Antworte NUR mit JSON in exakt dieser Form:
{"found":true,"summary":"","start_local":"YYYY-MM-DDTHH:MM","end_local":"YYYY-MM-DDTHH:MM","location":"","description":""}

Regeln:
- WICHTIG: Betreff UND Body sind beide Quellen. Termin-Info kann NUR im Betreff stehen.
- start_local / end_local: ISO-Format STRIKT `YYYY-MM-DDTHH:MM` mit Bindestrichen, Großbuchstabe T, Doppelpunkten, ZWEISTELLIGEM Monat + Tag + Stunde + Minute. KEIN Punkt, KEIN Komma, KEIN Wochentag, KEIN AM/PM, KEINE Zeitzone, KEIN Offset.
- end fehlt → start+1h. Nur Datum genannt → 09:00 / 10:00.
- Relative Daten ("morgen", "nächsten Mi", "kommenden Di") relativ zum Mail-Datum auflösen.
- Deutsche Datumsformate konvertieren: "19.5.26" → "2026-05-19", "19. Mai 2026" → "2026-05-19", "19 Uhr" → "19:00".
- location: Vollständige Meeting-URL (Zoom/Teams/Meet/Webex/Jitsi/BBB) bevorzugt vor physischem Ort.
- WEDER Betreff NOCH Body enthalten Termin-Daten → {"found":false,"summary":"","start_local":"","end_local":"","location":"","description":""}.
- Kein Markdown, kein Kommentar — nur das JSON-Objekt.

BEISPIEL:
Eingabe-Betreff: `Video-Regio-Austausch 19.5.26 19 Uhr`
Eingabe-Body: `... am kommenden Dienstag um 19 Uhr ... ZOOM-Raum: https://zoom.us/j/88660338369 ...`
Erwartete Ausgabe: {"found":true,"summary":"Video-Regio-Austausch","start_local":"2026-05-19T19:00","end_local":"2026-05-19T20:00","location":"https://zoom.us/j/88660338369","description":"Netzwerk-Team Südwest, monatlicher Austausch."}
"#;

/// Datum/Uhrzeit-Format für den Frontend-Editor — exakt das, was ein
/// HTML5 `datetime-local`-Input liefern würde.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedEventDraft {
    pub summary: String,
    /// Nackte Lokalzeit `YYYY-MM-DDTHH:MM` — frontend stempelt den
    /// System-Offset beim Speichern drauf (gleiche Konvertierung wie für
    /// `datetime-local`-Inputs im EventEditor).
    pub start_local: String,
    pub end_local: String,
    pub location: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventExtractionResult {
    /// pi hat brauchbare Termin-Daten gefunden. Frontend öffnet den
    /// EventEditor im create-mode mit dem Draft vorbefüllt.
    Found { draft: ExtractedEventDraft },
    /// pi konnte keine Termin-Daten erkennen (oder `found: false`
    /// geliefert). Frontend zeigt eine Toast-/Fehlermeldung.
    Empty,
    /// Mail nicht gefunden, Body leer, pi nicht verfügbar etc. —
    /// strukturelle Probleme, die der User selbst nicht beheben kann
    /// ohne Kontext.
    NotApplicable { reason: String },
}

/// Lockerer Parse-Layer für die pi-JSON. Pi-Modelle ignorieren das
/// gewünschte Schema schon mal: `null`, fehlende Felder, Number statt
/// String, alles möglich. Wir mappen all das auf einen leeren String,
/// damit ein einzelnes schräges Feld nicht den ganzen Extract killt.
#[derive(Debug, Deserialize)]
struct RawPiOutput {
    #[serde(default)]
    found: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "null_to_empty")]
    summary: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    start_local: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    end_local: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    location: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    description: String,
}

fn null_to_empty<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let v = serde_json::Value::deserialize(de)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Null => Ok(String::new()),
        other => Ok(other.to_string().trim_matches('"').to_string()),
    }
}

pub async fn extract_event_for_message(
    app: AppHandle,
    db: DbHandle,
    message_id: MessageId,
) -> Result<EventExtractionResult, String> {
    // ── 1. Envelope laden ─────────────────────────────────────────
    let envelope = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_envelope(&conn, &message_id)
            .map_err(|e| e.to_string())?
            .ok_or("envelope not found")?
    };

    // ── 2. Body holen (cached oder fetch) ─────────────────────────
    let body_text = if envelope.body_cached {
        match crate::application::body::cached(&db, &message_id)
            .map_err(|e| format!("body cached: {e}"))?
        {
            Some(b) => b.plain_text.or(b.html_text).unwrap_or_default(),
            None => String::new(),
        }
    } else {
        let parsed = crate::application::body::fetch_and_store(&app, &db, message_id)
            .await
            .map_err(|e| format!("body fetch: {e}"))?;
        parsed.plain.or(parsed.html).unwrap_or_default()
    };

    if body_text.trim().is_empty() {
        return Ok(EventExtractionResult::NotApplicable {
            reason: "Mail-Body ist leer".into(),
        });
    }

    // Kopf (statt Schwanz wie bei Signaturen) — Termin-Hinweise stehen
    // meistens früh in der Mail. Char-boundary-safe abschneiden.
    let body_for_pi = if body_text.len() > BODY_BYTES {
        let mut idx = BODY_BYTES;
        while idx > 0 && !body_text.is_char_boundary(idx) {
            idx -= 1;
        }
        format!("{}\n...", &body_text[..idx])
    } else {
        body_text
    };

    // ── 3. Pi-Prompt mit Mail-Datum als Anker für relative Zeiten ─
    let mail_date_iso = envelope.date.to_rfc3339();
    let subject = if envelope.subject.is_empty() {
        "(ohne Betreff)".to_string()
    } else {
        envelope.subject.clone()
    };
    // Subject zuerst UND zuletzt einbauen: vorne als Kontext, hinten
    // direkt vor dem JSON-Output noch mal als Reminder. Termin-Info im
    // Betreff (häufiger Fall: "Meeting Mi 14:00 - Topic") wurde sonst
    // vom Modell gerne ignoriert wenn der Body keinen passenden
    // Hinweis lieferte.
    let prompt = format!(
        "{}\n\n--- BEGIN E-MAIL ---\n\
         Mail-Datum (Anker für relative Angaben): {}\n\
         BETREFF: {}\n\
         \n\
         BODY:\n{}\n\
         --- END E-MAIL ---\n\
         \n\
         Reminder: Der BETREFF oben ist Teil der Mail. Wenn das Datum/Uhrzeit \
         dort steht (auch wenn der Body nichts dazu sagt), trotzdem extrahieren.",
        EXTRACT_PROMPT, mail_date_iso, subject, body_for_pi
    );

    let json_text =
        crate::application::contact_extract::call_pi(&app, prompt, PI_TIMEOUT_SECS).await?;

    // Pi-Rohantwort loggen — bei "empty"/"not_applicable"-Toasts kann
    // der User damit nachvollziehen, was das Modell tatsächlich
    // geliefert hat (häufig: korrektes Datum aber in falschem Format).
    // tracing::info gegen `RUST_LOG=info` (oder via Logger-Settings)
    // sichtbar.
    tracing::info!(
        target: "event_extract",
        raw_len = json_text.len(),
        raw = %json_text.trim(),
        "pi raw output"
    );

    // ── 4. Parse ─────────────────────────────────────────────────
    let json_str = crate::application::contact_extract::first_balanced_object(&json_text)
        .ok_or_else(|| {
            format!(
                "pi-Antwort enthält kein JSON-Objekt:\n---\n{}\n---",
                json_text.trim()
            )
        })?;
    let raw: RawPiOutput = serde_json::from_str(&json_str)
        .map_err(|e| format!("pi-JSON nicht parsebar: {e}\n{json_str}"))?;

    // `found`-Flag tolerieren — pi-Modelle liefern mal `true`/`false`,
    // mal `"true"`, mal das Feld gar nicht.
    let found_explicit = match raw.found {
        Some(serde_json::Value::Bool(b)) => Some(b),
        Some(serde_json::Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    };

    // Wenn pi explizit `found: false` sagt → empty.
    if found_explicit == Some(false) {
        return Ok(EventExtractionResult::Empty);
    }

    // Sonst: ein Termin gilt nur dann als gefunden, wenn mindestens
    // Start UND Ende normalisierbar sind. Ein nackter Titel ohne
    // Zeitanker hilft niemandem im Kalender.
    let start = normalize_local_datetime(&raw.start_local);
    let end_raw = raw.end_local.trim();
    let end = if end_raw.is_empty() {
        // Pi hat das `end fehlt → start+1h` nicht beachtet — wir holen
        // das hier nach. Lieber selbst Default setzen als auf
        // "empty" werfen.
        start.as_deref().and_then(plus_one_hour)
    } else {
        normalize_local_datetime(end_raw)
    };

    match (start, end) {
        (Some(s), Some(e)) => Ok(EventExtractionResult::Found {
            draft: ExtractedEventDraft {
                summary: raw.summary.trim().to_string(),
                start_local: s,
                end_local: e,
                location: raw.location.trim().to_string(),
                description: raw.description.trim().to_string(),
            },
        }),
        _ => {
            tracing::info!(
                target: "event_extract",
                start_raw = %raw.start_local,
                end_raw = %raw.end_local,
                "pi event-extraction: start/end nicht normalisierbar"
            );
            Ok(EventExtractionResult::Empty)
        }
    }
}

/// Akzeptiert die häufigen Output-Varianten kleinerer LLMs und mappt
/// sie auf das vom Frontend erwartete Format `YYYY-MM-DDTHH:MM`.
/// Erkannte Eingaben:
///   - `2026-05-19T19:00` (ideal)
///   - `2026-05-19T19:00:00` → Sekunden droppen
///   - `2026-05-19T19:00+02:00` / `…Z` → Offset/Z droppen
///   - `2026-05-19 19:00` (Leerzeichen statt T)
///   - `2026-05-19T19:00 Uhr` → " Uhr" droppen
/// Gibt `None` bei alles, was sich nicht eindeutig auf YYYY-MM-DDTHH:MM
/// reduzieren lässt — der Caller behandelt das als "Empty"-Outcome.
fn normalize_local_datetime(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // " Uhr"-Suffix abschneiden ("2026-05-19T19:00 Uhr").
    let s = s.trim_end_matches(" Uhr").trim();
    // Z oder ±HH:MM Offset abschneiden — wir wollen reine Lokalzeit.
    let s = if let Some(stripped) = s.strip_suffix('Z') {
        stripped.trim_end()
    } else if let Some(idx) = find_offset_idx(s) {
        &s[..idx]
    } else {
        s
    };
    // Whitespace zwischen Datum und Zeit → T normalisieren.
    let normalized = if let Some((d, t)) = s.split_once(' ') {
        format!("{}T{}", d.trim(), t.trim())
    } else {
        s.to_string()
    };
    // Sekunden droppen wenn vorhanden: "...:00:00" → "...:00".
    let normalized = match normalized.matches(':').count() {
        2 => {
            // YYYY-MM-DDTHH:MM:SS → vor dem zweiten Doppelpunkt kappen.
            let bytes = normalized.as_bytes();
            let mut count = 0;
            let mut idx = bytes.len();
            for (i, &b) in bytes.iter().enumerate() {
                if b == b':' {
                    count += 1;
                    if count == 2 {
                        idx = i;
                        break;
                    }
                }
            }
            normalized[..idx].to_string()
        }
        1 => normalized,
        _ => return None,
    };
    // Final-Check: passt jetzt auf YYYY-MM-DDTHH:MM (genau 16 Zeichen,
    // Trennzeichen an den erwarteten Positionen, Rest Ziffern).
    let b = normalized.as_bytes();
    if b.len() != 16 {
        return None;
    }
    let digit = |i: usize| b.get(i).map(|c| c.is_ascii_digit()).unwrap_or(false);
    if !(digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && b[4] == b'-'
        && digit(5)
        && digit(6)
        && b[7] == b'-'
        && digit(8)
        && digit(9)
        && b[10] == b'T'
        && digit(11)
        && digit(12)
        && b[13] == b':'
        && digit(14)
        && digit(15))
    {
        return None;
    }
    Some(normalized)
}

/// Suche den Anfang eines `+HH:MM` / `-HH:MM`-Suffix. Liefert den Index
/// des `+`/`-` falls vorhanden, sonst `None`. Wir dürfen nicht naiv auf
/// `find('-')` rauschen, weil das Datum selbst Bindestriche hat — nur
/// das suffix-Pattern zählt.
fn find_offset_idx(s: &str) -> Option<usize> {
    // Suffix-Muster: `[+-]HH:MM` an Position len-6 oder `[+-]HHMM` an
    // Position len-5. Beide Fälle prüfen.
    let bytes = s.as_bytes();
    let len = bytes.len();
    if len >= 6
        && (bytes[len - 6] == b'+' || bytes[len - 6] == b'-')
        && bytes[len - 5].is_ascii_digit()
        && bytes[len - 4].is_ascii_digit()
        && bytes[len - 3] == b':'
        && bytes[len - 2].is_ascii_digit()
        && bytes[len - 1].is_ascii_digit()
    {
        return Some(len - 6);
    }
    if len >= 5
        && (bytes[len - 5] == b'+' || bytes[len - 5] == b'-')
        && bytes[len - 4].is_ascii_digit()
        && bytes[len - 3].is_ascii_digit()
        && bytes[len - 2].is_ascii_digit()
        && bytes[len - 1].is_ascii_digit()
    {
        return Some(len - 5);
    }
    None
}

/// `YYYY-MM-DDTHH:MM` → `+1h`, mit Tagesübertrag. Behandeln wir lokal,
/// damit der Caller keinen zusätzlichen chrono-Pfad braucht. Bei
/// invalidem Input → None.
fn plus_one_hour(s: &str) -> Option<String> {
    // Wir parsen `YYYY-MM-DDTHH:MM` als naive datetime via chrono.
    let dt = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok()?;
    let plus = dt.checked_add_signed(chrono::Duration::hours(1))?;
    Some(plus.format("%Y-%m-%dT%H:%M").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_canonical_form() {
        assert_eq!(
            normalize_local_datetime("2026-05-19T19:00"),
            Some("2026-05-19T19:00".to_string())
        );
    }

    #[test]
    fn normalizes_space_separator() {
        assert_eq!(
            normalize_local_datetime("2026-05-19 19:00"),
            Some("2026-05-19T19:00".to_string())
        );
    }

    #[test]
    fn drops_seconds() {
        assert_eq!(
            normalize_local_datetime("2026-05-19T19:00:00"),
            Some("2026-05-19T19:00".to_string())
        );
        assert_eq!(
            normalize_local_datetime("2026-05-19 19:00:42"),
            Some("2026-05-19T19:00".to_string())
        );
    }

    #[test]
    fn drops_timezone_offset() {
        assert_eq!(
            normalize_local_datetime("2026-05-19T19:00+02:00"),
            Some("2026-05-19T19:00".to_string())
        );
        assert_eq!(
            normalize_local_datetime("2026-05-19T19:00:00+0200"),
            Some("2026-05-19T19:00".to_string())
        );
        assert_eq!(
            normalize_local_datetime("2026-05-19T19:00Z"),
            Some("2026-05-19T19:00".to_string())
        );
        assert_eq!(
            normalize_local_datetime("2026-05-19T19:00-05:00"),
            Some("2026-05-19T19:00".to_string())
        );
    }

    #[test]
    fn drops_uhr_suffix() {
        assert_eq!(
            normalize_local_datetime("2026-05-19T19:00 Uhr"),
            Some("2026-05-19T19:00".to_string())
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(normalize_local_datetime("").is_none());
        assert!(normalize_local_datetime("19.5.26 19:00").is_none());
        assert!(normalize_local_datetime("Mittwoch 19 Uhr").is_none());
    }

    #[test]
    fn plus_one_hour_rolls_over_midnight() {
        assert_eq!(
            plus_one_hour("2026-05-19T23:30"),
            Some("2026-05-20T00:30".to_string())
        );
    }

    #[test]
    fn plus_one_hour_basic() {
        assert_eq!(
            plus_one_hour("2026-05-19T19:00"),
            Some("2026-05-19T20:00".to_string())
        );
    }
}
