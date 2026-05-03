// pi-gestütztes Regel-Lernen aus Spam-Kandidaten.
//
// Architektur-Ausrichtung: "so wenig LLM wie möglich, so viel wie nötig".
// Dieser Modul wird nur aufgerufen wenn der User *explizit* auf "Regel
// lernen" klickt. Pro Aufruf ein pi-Call, mit allen N Kandidaten
// gleichzeitig — nicht pro Mail ein Call. Jede Folge-Anwendung der
// gelernten Regel läuft dann deterministisch durch die Regel-Engine,
// ohne jemals wieder pi zu bemühen.
//
// Defensives JSON-Parsing: lokale Modelle (gemma, llama) liefern häufig
// JSON eingebettet in Markdown-Fences oder mit erklärendem Vorspann.
// Wir extrahieren den ersten balancierten `{...}`-Block statt zu
// hoffen, dass das ganze Response sauber parsbar ist.

use mail_parser::MessageParser;
use serde::{Deserialize, Serialize};

use crate::application::spam_rules::RuleDraft;
use crate::domain::message::MessageId;
use crate::domain::spam_rule::SpamPatternType;
use crate::infrastructure::db::DbHandle;
use crate::infrastructure::queries;

/// Compact feature summary of one spam candidate that gets fed into the
/// pi prompt. Keep it small: pi works better with dense, relevant input
/// than with a full-text dump.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateFeatures {
    pub message_id: String,
    pub from_email: String,
    pub from_domain: String,
    pub subject: String,
    /// Only spam-relevant header lines (X-Spam-*, Authentication-Results,
    /// Return-Path, List-Unsubscribe presence, Received-SPF). One line
    /// per header, prefix included.
    pub relevant_headers: Vec<String>,
    /// First ~300 chars of plain body. NULL if body not cached yet.
    pub body_preview: Option<String>,
}

pub async fn collect_candidate_features(
    db: &DbHandle,
    message_ids: &[MessageId],
) -> Result<Vec<CandidateFeatures>, String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(message_ids.len());
    for id in message_ids {
        let envelope = match queries::get_envelope(&conn, id).map_err(|e| e.to_string())? {
            Some(e) => e,
            None => continue,
        };
        let body = queries::get_body(&conn, id).map_err(|e| e.to_string())?;
        let raw = queries::get_body_raw(&conn, id).map_err(|e| e.to_string())?;

        let from_email = envelope
            .from
            .first()
            .map(|a| a.email.clone())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let from_domain = from_email
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();

        let body_preview = body.and_then(|b| b.plain_text).map(|s| {
            let end = s.char_indices().nth(300).map(|(i, _)| i).unwrap_or(s.len());
            s[..end].to_string()
        });

        let relevant_headers = raw
            .map(|bytes| extract_relevant_headers(&bytes))
            .unwrap_or_default();

        out.push(CandidateFeatures {
            message_id: id.0.to_string(),
            from_email,
            from_domain,
            subject: envelope.subject,
            relevant_headers,
            body_preview,
        });
    }
    Ok(out)
}

/// Pull the header block from raw RFC822 and keep only the fields that
/// carry spam signal. Skips the 30-ish other headers (Date, Content-Type
/// boundaries, MIME-Version …) so the prompt stays compact.
fn extract_relevant_headers(raw: &[u8]) -> Vec<String> {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let interesting: &[&str] = &[
        "x-spam-flag",
        "x-spam-status",
        "x-spam-level",
        "x-spam-score",
        "authentication-results",
        "received-spf",
        "return-path",
        "list-unsubscribe",
        "list-id",
        "precedence",
        "dkim-signature",
    ];
    let mut out = Vec::new();
    for h in msg.headers() {
        let name = h.name.as_str().to_ascii_lowercase();
        if interesting.iter().any(|&k| name == k) {
            // Serialize the raw header value. mail-parser gives us a
            // HeaderValue enum; for our purposes converting via Debug
            // would be ugly. Use the text body if it's text-shaped,
            // otherwise skip.
            let val_str = match &h.value {
                mail_parser::HeaderValue::Text(s) => s.to_string(),
                mail_parser::HeaderValue::TextList(list) => list
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                mail_parser::HeaderValue::Received(r) => format!("{r:?}"),
                mail_parser::HeaderValue::Address(_) => {
                    // Addresses are not useful here except Return-Path —
                    // grab the email form by re-parsing the raw bytes.
                    String::new()
                }
                _ => String::new(),
            };
            if val_str.trim().is_empty() {
                continue;
            }
            // Truncate ridiculously long values (DKIM signatures) to keep
            // prompt size bounded.
            let truncated = if val_str.len() > 200 {
                format!("{}…", &val_str[..200])
            } else {
                val_str
            };
            out.push(format!("{}: {}", h.name.as_str(), truncated));
        }
    }
    out
}

/// Assemble the pi prompt. Deliberately structured: the model sees the
/// candidates as a numbered list, the schema it must return is spelled
/// out verbatim, and there's a short rule-set for *what* to look for.
pub fn build_prompt(features: &[CandidateFeatures]) -> String {
    let mut s = String::new();
    s.push_str(
        "Du bist ein Spam-Filter-Analyst. Der Nutzer hat die folgenden Mails als \
         Spam-Verdacht markiert, weil sein Mailserver-Filter sie übersehen hat.\n\
         \n\
         Deine Aufgabe: finde Muster, die ein deterministischer Regex-/Substring-Filter \
         erfassen kann. Bevorzuge präzise Regeln (wenige False-Positives) gegenüber \
         aggressiven. Wenn du kein klares Muster findest: gib eine leere `rules`-Liste \
         zurück statt zu raten.\n\
         \n\
         Zulässige pattern_type-Werte:\n\
         - from_email        (exakte, case-insensitive Absenderadresse)\n\
         - from_domain       (Domain nach dem @)\n\
         - subject_contains  (Substring im Betreff)\n\
         - subject_regex     (Regex auf den Betreff — nur wenn nötig)\n\
         - body_contains     (Substring im ersten Teil des Mailtextes)\n\
         - header_contains   (Substring im Header-Block, z.B. \"x-spam-status: yes\")\n\
         \n",
    );

    for (i, f) in features.iter().enumerate() {
        s.push_str(&format!("== Kandidat {} ==\n", i + 1));
        s.push_str(&format!("From: {}\n", f.from_email));
        s.push_str(&format!("Subject: {}\n", f.subject));
        if !f.relevant_headers.is_empty() {
            s.push_str("Relevante Header:\n");
            for h in &f.relevant_headers {
                s.push_str(&format!("  {h}\n"));
            }
        }
        if let Some(body) = &f.body_preview {
            // Compact body preview — just the first ~300 chars, one line
            s.push_str("Body-Anfang:\n");
            s.push_str("  ");
            s.push_str(&body.replace('\n', " ").trim().chars().take(300).collect::<String>());
            s.push('\n');
        }
        s.push('\n');
    }

    s.push_str(
        "Antworte ausschließlich mit einem JSON-Objekt in diesem Schema:\n\
         \n\
         {\n\
         \x20 \"rules\": [\n\
         \x20   {\n\
         \x20     \"patternType\": \"from_domain\",\n\
         \x20     \"pattern\": \"promo.example.xyz\",\n\
         \x20     \"confidence\": 0.92,\n\
         \x20     \"reason\": \"6 von 8 Beispielen kommen von dieser Domain\"\n\
         \x20   }\n\
         \x20 ]\n\
         }\n\
         \n\
         Keine Einleitung, keine Erklärung davor, kein Markdown. Nur das JSON.",
    );
    s
}

/// Iterator over every balanced `{...}` block in `raw`. Used to walk
/// past local-LLM noise like ```json fences, preamble prose, and
/// thinking-style preludes (`{"analysis":"..."}`) before the real
/// schema-shaped JSON appears.
pub fn iter_json_objects<'a>(raw: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let bytes = raw.as_bytes();
    let mut cursor = 0usize;
    std::iter::from_fn(move || {
        while cursor < bytes.len() {
            let Some(rel) = bytes[cursor..].iter().position(|&b| b == b'{') else {
                return None;
            };
            let start = cursor + rel;
            let mut depth: i32 = 0;
            let mut i = start;
            while i < bytes.len() {
                match bytes[i] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            let slice = &raw[start..=i];
                            cursor = i + 1;
                            return Some(slice);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            return None;
        }
        None
    })
}

/// Legacy single-shot extractor — kept for the existing tests and as a
/// convenience when you just want "the first JSON object" without
/// caring about schema. Newer code should prefer `iter_json_objects` +
/// schema filtering.
///
/// `dead_code`-annotated because production callers all moved to the
/// schema-filtering walker; the unit tests below still exercise it and
/// it's a one-line wrapper, not worth pruning.
#[allow(dead_code)]
pub fn extract_json_object(raw: &str) -> Option<&str> {
    iter_json_objects(raw).next()
}

#[derive(Debug, Clone, Deserialize)]
struct PiRulesResponse {
    rules: Vec<PiRuleProposal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiRuleProposal {
    #[serde(alias = "pattern_type")]
    pattern_type: String,
    pattern: String,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
}

/// Parse pi's response into `RuleDraft`s. Defensive: unknown
/// pattern_type strings are dropped silently (pi sometimes invents
/// variants like `from_sender` — we just ignore those).
///
/// Walks every balanced `{...}` in the raw text and picks the first
/// one that actually parses as our `{"rules":[...]}` schema. This is
/// more forgiving than "first JSON object wins" because gemma likes to
/// preface the real answer with thinking-style JSON chatter.
pub fn parse_pi_response(raw: &str) -> Result<Vec<RuleDraft>, String> {
    let parsed = iter_json_objects(raw)
        .find_map(|obj| serde_json::from_str::<PiRulesResponse>(obj).ok())
        .ok_or_else(|| {
            "pi-Antwort enthielt kein JSON-Objekt mit einem `rules`-Array."
                .to_string()
        })?;
    let mut out = Vec::new();
    for p in parsed.rules {
        let Some(pt) = parse_pattern_type(&p.pattern_type) else {
            tracing::warn!("unknown pattern_type from pi: {}", p.pattern_type);
            continue;
        };
        let pattern = p.pattern.trim().to_string();
        if pattern.is_empty() {
            continue;
        }
        // Validate syntactically (regex, etc.) — ignore patterns that
        // wouldn't survive `add_spam_rule`'s own check.
        if crate::application::spam_rules::validate_pattern(pt, &pattern).is_err() {
            continue;
        }
        out.push(RuleDraft {
            account_id: None,
            pattern_type: pt,
            pattern,
            confidence: p.confidence,
            reason: p.reason,
        });
    }
    Ok(out)
}

fn parse_pattern_type(s: &str) -> Option<SpamPatternType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "from_email" | "fromemail" => Some(SpamPatternType::FromEmail),
        "from_domain" | "fromdomain" => Some(SpamPatternType::FromDomain),
        "subject_contains" | "subjectcontains" => Some(SpamPatternType::SubjectContains),
        "subject_regex" | "subjectregex" => Some(SpamPatternType::SubjectRegex),
        "body_contains" | "bodycontains" => Some(SpamPatternType::BodyContains),
        "header_contains" | "headercontains" => Some(SpamPatternType::HeaderContains),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_balanced_object() {
        assert_eq!(
            extract_json_object("noise {\"a\":1} tail"),
            Some("{\"a\":1}")
        );
    }

    #[test]
    fn handles_nested_braces() {
        assert_eq!(
            extract_json_object("pre ```json\n{\"x\":{\"y\":2}}\n```"),
            Some("{\"x\":{\"y\":2}}")
        );
    }

    #[test]
    fn parses_well_formed_response() {
        let raw = r#"Here is the analysis:
```json
{
  "rules": [
    {"patternType": "from_domain", "pattern": "spam.xyz", "confidence": 0.9, "reason": "most mails"}
  ]
}
```"#;
        let drafts = parse_pi_response(raw).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].pattern, "spam.xyz");
    }

    #[test]
    fn tolerates_snake_case_field() {
        let raw = r#"{"rules":[{"pattern_type":"from_email","pattern":"a@b"}]}"#;
        let drafts = parse_pi_response(raw).unwrap();
        assert_eq!(drafts.len(), 1);
    }

    #[test]
    fn drops_unknown_pattern_types() {
        let raw = r#"{"rules":[
            {"patternType":"invented","pattern":"foo"},
            {"patternType":"from_domain","pattern":"x.com"}
        ]}"#;
        let drafts = parse_pi_response(raw).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].pattern, "x.com");
    }

    #[test]
    fn returns_err_on_no_json() {
        assert!(parse_pi_response("sorry, I couldn't analyze").is_err());
    }

    #[test]
    fn skips_preamble_json_and_picks_rules_object() {
        // gemma likes to prefix a thinking-style object before the real answer.
        let raw = r#"
            {"thinking":"let me analyse the mails"}

            Here is the result:
            {"rules":[{"patternType":"from_domain","pattern":"x.com"}]}
        "#;
        let drafts = parse_pi_response(raw).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].pattern, "x.com");
    }
}
