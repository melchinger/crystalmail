// Deterministic spam-rule matching. No LLM in this module — pi's job
// (in a separate flow) is to propose the pattern strings; from there
// every incoming envelope just gets regex'd.
//
// Feature extraction is lightweight on purpose: sender address, domain,
// subject, and a plain-text body preview. We don't pull full bodies from
// IMAP for matching — we use whatever's already in `envelopes` +
// `bodies.plain_text` for cached ones.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::domain::spam_rule::{SpamPatternType, SpamRule};

/// Features extracted from an envelope for rule matching. The rule
/// engine only looks at this projection — decouples "what we match on"
/// from the database row shape.
#[derive(Debug, Clone)]
pub struct MatchFeatures {
    pub from_email: String,    // lowercased
    pub from_domain: String,   // lowercased, stripped
    pub subject: String,       // raw
    pub body_preview: String,  // first ~500 chars of plain text, lowercased
    /// Lowercased RFC 5322 header block. `None` when not loaded (e.g. body
    /// not cached yet) — a `HeaderContains` rule on such an envelope
    /// simply doesn't match, no false positives.
    pub headers_text: Option<String>,
}

impl MatchFeatures {
    pub fn from_parts(
        from_email: &str,
        subject: &str,
        body_plain: Option<&str>,
        raw_rfc822: Option<&[u8]>,
    ) -> Self {
        let from_email = from_email.trim().to_ascii_lowercase();
        let from_domain = from_email
            .rsplit_once('@')
            .map(|(_, d)| d.to_string())
            .unwrap_or_default();
        let body_preview = body_plain
            .map(|s| {
                let end = s.char_indices().nth(500).map(|(i, _)| i).unwrap_or(s.len());
                s[..end].to_ascii_lowercase()
            })
            .unwrap_or_default();
        let headers_text = raw_rfc822.map(extract_headers_lowercase);
        Self {
            from_email,
            from_domain,
            subject: subject.to_string(),
            body_preview,
            headers_text,
        }
    }
}

/// Pull out the header block (everything up to the first blank line) and
/// lowercase it. Handles both CRLF and LF terminators. `String::from_utf8_lossy`
/// tolerates weird encodings that sometimes slip into X-headers.
fn extract_headers_lowercase(raw: &[u8]) -> String {
    let limit = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n"))
        .unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..limit]).to_ascii_lowercase()
}

/// Validate a rule's pattern. Returns an error if the pattern type requires
/// special syntax (regex) and the input is malformed. Called at save time
/// so bad patterns never reach the hot path.
pub fn validate_pattern(pattern_type: SpamPatternType, pattern: &str) -> Result<(), String> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return Err("Pattern darf nicht leer sein.".into());
    }
    match pattern_type {
        SpamPatternType::SubjectRegex => {
            Regex::new(trimmed).map_err(|e| format!("Ungültiges Regex: {e}"))?;
        }
        // Other types are literal strings — any non-empty value is accepted.
        _ => {}
    }
    Ok(())
}

/// `SpamRule` plus seine vorab kompilierten Hilfs-Daten — hauptsächlich
/// die `Regex` für `SubjectRegex`-Patterns. Pro Sync-Run einmal gebaut,
/// danach im Envelope-Loop nur noch refzdurchgereicht. Spart bei einem
/// typischen Sync (500 Mails × 5 Regex-Regeln) 2.500 `Regex::new()`-
/// Aufrufe — und mit ihnen die nichttriviale Parsing- und NFA-Build-Zeit.
///
/// Patterns, die zur Compile-Zeit kaputt sind, landen mit `regex = None`
/// im Cache — `matches_compiled` interpretiert das als "matcht nicht".
/// So zerlegt eine einzelne defekte Regel nicht den Rest der Filter-
/// Kaskade. (Gleiche Semantik wie das alte `matches()`.)
pub struct Compiled {
    pub rule: SpamRule,
    pub regex: Option<Regex>,
}

impl Compiled {
    pub fn new(rule: SpamRule) -> Self {
        let regex = match rule.pattern_type {
            SpamPatternType::SubjectRegex => Regex::new(rule.pattern.trim()).ok(),
            _ => None,
        };
        Self { rule, regex }
    }
}

/// Komfort-Helfer: Rules-Liste → Vec<Compiled>. Reihenfolge bleibt
/// erhalten, sodass die First-Match-Logik in `match_envelope` (sync.rs)
/// weiterhin deterministisch ist.
pub fn compile_all(rules: Vec<SpamRule>) -> Vec<Compiled> {
    rules.into_iter().map(Compiled::new).collect()
}

/// Match-Predicate gegen die vorkompilierte Form. Identische Logik wie
/// `matches()`, nur dass `SubjectRegex` den gecachten `Regex` benutzt.
pub fn matches_compiled(features: &MatchFeatures, compiled: &Compiled) -> bool {
    let pat = compiled.rule.pattern.trim();
    if pat.is_empty() {
        return false;
    }
    match compiled.rule.pattern_type {
        SpamPatternType::FromEmail => features.from_email == pat.to_ascii_lowercase(),
        SpamPatternType::FromDomain => features.from_domain == pat.to_ascii_lowercase(),
        SpamPatternType::SubjectContains => {
            features.subject.to_ascii_lowercase().contains(&pat.to_ascii_lowercase())
        }
        SpamPatternType::SubjectRegex => {
            // `None` = pattern failed to compile at `Compiled::new`.
            // Wie das alte `matches()` per `Regex::new(...).ok()`-Fallback:
            // defekte Regel matcht stillschweigend nichts statt zu kracheln.
            compiled
                .regex
                .as_ref()
                .map(|re| re.is_match(&features.subject))
                .unwrap_or(false)
        }
        SpamPatternType::BodyContains => {
            features.body_preview.contains(&pat.to_ascii_lowercase())
        }
        SpamPatternType::HeaderContains => {
            // Lowercase comparison both sides — header field names are
            // case-insensitive (RFC 5322 §2.2), and users will typically
            // write patterns in any casing they remember. `None` on the
            // feature side means headers aren't available for this row
            // (body not yet cached); bail rather than false-match.
            match &features.headers_text {
                Some(h) => h.contains(&pat.to_ascii_lowercase()),
                None => false,
            }
        }
    }
}

/// Convenience-Wrapper: kompiliert ad-hoc und matched einmal. Existiert
/// für Tests und vereinzelte Off-Hot-Path-Aufrufer (Preview-Modus etc.).
/// Im Sync- und Apply-Loop **nicht** verwenden — dort `compile_all`
/// einmalig + `matches_compiled` pro Envelope.
#[allow(dead_code)]
pub fn matches(features: &MatchFeatures, rule: &SpamRule) -> bool {
    matches_compiled(features, &Compiled::new(rule.clone()))
}

/// Lightweight DTO used for proposing rules (from pi, from the UI form).
/// Mirrors the fields required to create a `SpamRule` but without id /
/// timestamps / counters which the backend fills in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDraft {
    #[serde(default)]
    pub account_id: Option<crate::domain::account::AccountId>,
    pub pattern_type: SpamPatternType,
    pub pattern: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::spam_rule::{SpamRule, SpamRuleId};
    use chrono::Utc;
    use uuid::Uuid;

    /// Test-Fixture-Helper. Felder die an dieser Stelle nicht relevant sind
    /// kriegen Defaults — `created_at` und `hit_count` etwa beeinflussen
    /// das Match-Verhalten nicht und werden hier nur befüllt, weil
    /// `SpamRule` sie als `pub` hat.
    fn rule(pattern_type: SpamPatternType, pattern: &str) -> SpamRule {
        SpamRule {
            id: SpamRuleId(Uuid::new_v4()),
            account_id: None,
            pattern_type,
            pattern: pattern.into(),
            enabled: true,
            confidence: None,
            reason: None,
            created_at: Utc::now(),
            hit_count: 0,
        }
    }

    fn features(from: &str, subject: &str) -> MatchFeatures {
        MatchFeatures::from_parts(from, subject, None, None)
    }

    #[test]
    fn from_email_lowercases_both_sides() {
        let r = rule(SpamPatternType::FromEmail, "Spam@EXAMPLE.com");
        let c = Compiled::new(r);
        assert!(matches_compiled(
            &features("spam@example.COM", ""),
            &c
        ));
    }

    #[test]
    fn from_domain_strips_local_part() {
        let r = rule(SpamPatternType::FromDomain, "evil.tld");
        let c = Compiled::new(r);
        assert!(matches_compiled(
            &features("anything@evil.tld", ""),
            &c
        ));
        assert!(!matches_compiled(
            &features("ok@good.tld", ""),
            &c
        ));
    }

    #[test]
    fn subject_contains_is_case_insensitive() {
        let r = rule(SpamPatternType::SubjectContains, "ASAP");
        let c = Compiled::new(r);
        assert!(matches_compiled(
            &features("", "Bitte asap erledigen!"),
            &c
        ));
    }

    #[test]
    fn subject_regex_uses_compiled_cache() {
        let r = rule(SpamPatternType::SubjectRegex, r"(?i)\bgewinn\b");
        let c = Compiled::new(r);
        // `regex` ist gefüllt — das ist der Cache-Hit-Beweis.
        assert!(c.regex.is_some(), "valid regex should compile to Some");
        assert!(matches_compiled(
            &features("", "Sie haben den Gewinn nicht abgeholt"),
            &c
        ));
        assert!(!matches_compiled(
            &features("", "kein treffer hier"),
            &c
        ));
    }

    /// Defekte Regex-Pattern dürfen den Filter-Pass nicht abbrechen —
    /// `Compiled::new` legt `regex = None` ab, `matches_compiled` liefert
    /// `false`. Gleiche Semantik wie das alte `matches()`.
    #[test]
    fn invalid_regex_falls_back_to_no_match() {
        let r = rule(SpamPatternType::SubjectRegex, "(unclosed[");
        let c = Compiled::new(r);
        assert!(c.regex.is_none(), "broken regex must not produce a Regex");
        assert!(!matches_compiled(
            &features("", "anything could match"),
            &c
        ));
    }

    #[test]
    fn empty_pattern_never_matches() {
        let r = rule(SpamPatternType::SubjectContains, "   ");
        let c = Compiled::new(r);
        assert!(!matches_compiled(
            &features("", "egal was hier steht"),
            &c
        ));
    }

    /// `BodyContains` ohne body_preview matcht nichts (kein false-positive).
    #[test]
    fn body_rule_without_body_preview_misses() {
        let r = rule(SpamPatternType::BodyContains, "viagra");
        let c = Compiled::new(r);
        // `from_parts(.., None, None)` lässt body_preview leer
        let f = features("a@b.tld", "Subject");
        assert!(!matches_compiled(&f, &c));
    }

    /// `HeaderContains` ohne raw_rfc822 → `headers_text = None`,
    /// matcht ebenfalls nichts.
    #[test]
    fn header_rule_without_headers_misses() {
        let r = rule(SpamPatternType::HeaderContains, "x-mailer: spammy");
        let c = Compiled::new(r);
        let f = features("a@b.tld", "Subject");
        assert!(!matches_compiled(&f, &c));
    }

    /// `compile_all` erhält die Reihenfolge — wichtig für die
    /// First-Match-Logik in `match_envelope`.
    #[test]
    fn compile_all_preserves_order() {
        let rules = vec![
            rule(SpamPatternType::FromDomain, "first.tld"),
            rule(SpamPatternType::FromDomain, "second.tld"),
            rule(SpamPatternType::FromDomain, "third.tld"),
        ];
        let ids: Vec<_> = rules.iter().map(|r| r.id).collect();
        let compiled = compile_all(rules);
        let compiled_ids: Vec<_> = compiled.iter().map(|c| c.rule.id).collect();
        assert_eq!(ids, compiled_ids);
    }

    /// `compile_all` baut Regex *nur* für SubjectRegex — andere Pattern-
    /// Typen brauchen keinen Cache und sollen `regex = None` bleiben,
    /// damit `matches_compiled`s Branch-Logik klar bleibt.
    #[test]
    fn compile_all_only_compiles_subject_regex() {
        let rules = vec![
            rule(SpamPatternType::FromEmail, "x@y.z"),
            rule(SpamPatternType::SubjectRegex, "valid"),
            rule(SpamPatternType::SubjectContains, "asap"),
        ];
        let compiled = compile_all(rules);
        assert!(compiled[0].regex.is_none(), "FromEmail: keine Regex");
        assert!(compiled[1].regex.is_some(), "SubjectRegex: Regex muss da sein");
        assert!(compiled[2].regex.is_none(), "SubjectContains: keine Regex");
    }

    #[test]
    fn validate_pattern_rejects_empty() {
        assert!(validate_pattern(SpamPatternType::FromEmail, "").is_err());
        assert!(validate_pattern(SpamPatternType::FromEmail, "   ").is_err());
    }

    #[test]
    fn validate_pattern_rejects_bad_regex() {
        assert!(validate_pattern(SpamPatternType::SubjectRegex, "(unclosed[").is_err());
    }

    #[test]
    fn validate_pattern_accepts_good_regex() {
        assert!(validate_pattern(SpamPatternType::SubjectRegex, r"\b\w+@\w+\b").is_ok());
    }
}
