// pi-gestütztes Regel-Lernen für Workflows. Analog zum spam_analysis-
// Modul — wir hoffen darauf, dass pi aus einer Handvoll Trainings-
// Mails den *engsten* deterministischen Filter findet, mit dem die
// Regel-Engine dann ohne pi weiterläuft.
//
// Unterschiede zum Spam-Lernen:
//   * Feature-Mix: nicht die typischen Spam-Header, sondern
//     Absender + Subject + Attachment-Typen + Ordner + Account. Das
//     sind die Signale, die unser Rule-Model überhaupt verwertet.
//   * Output-Schema: strukturierte `predicates[]` + optional
//     `folderName` + `mode` (auto|confirm). Kein `confidence` pro
//     Predicate — die Regel als Ganzes wird vom User freigegeben,
//     Predicate-für-Predicate-Review nervt beim Durchklicken.
//   * Prompt-Ton: "engster Filter" statt "bevorzuge präzise". Der
//     User hat die Beispiele *selbst* zusammengestellt — sie sind per
//     Definition die richtige Menge. pi soll den kleinsten gemeinsamen
//     Nenner finden, nicht defensiv unterfiltern.

use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde::{Deserialize, Serialize};

use crate::domain::account::AccountId;
use crate::domain::message::MessageId;
use crate::domain::workflow::{RuleAction, RuleMode, RulePredicate};
use crate::infrastructure::db::DbHandle;
use crate::infrastructure::queries;

/// Compact feature summary of one training candidate fed into the
/// pi prompt. Mirrors the fields that actually drive our Rule
/// predicates — no kitchen sink.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainingFeatures {
    pub message_id: String,
    pub from_email: String,
    pub from_domain: String,
    pub subject: String,
    pub folder_name: String,
    pub account_display_name: String,
    pub account_address: String,
    /// Extensions of non-inline attachments, lower-cased, no dot.
    /// Compound suffixes kept intact (`tar.gz`).
    pub attachment_extensions: Vec<String>,
    /// First ~200 chars of plain body for subject_contains hints.
    /// Short on purpose — the body is seldom the discriminating
    /// feature; from/subject/attachments usually are.
    pub body_preview: Option<String>,
}

pub async fn collect_training_features(
    db: &DbHandle,
    message_ids: &[MessageId],
) -> Result<Vec<TrainingFeatures>, String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(message_ids.len());
    for id in message_ids {
        let envelope = match queries::get_envelope(&conn, id).map_err(|e| e.to_string())? {
            Some(e) => e,
            None => continue,
        };
        let account = match queries::get_account(&conn, &envelope.account_id)
            .map_err(|e| e.to_string())?
        {
            Some(a) => a,
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
            let end = s.char_indices().nth(200).map(|(i, _)| i).unwrap_or(s.len());
            s[..end].replace('\n', " ").trim().to_string()
        });

        let attachment_extensions = raw
            .as_deref()
            .map(extract_attachment_extensions)
            .unwrap_or_default();

        out.push(TrainingFeatures {
            message_id: id.0.to_string(),
            from_email,
            from_domain,
            subject: envelope.subject,
            folder_name: envelope.folder_name,
            account_display_name: account.display_name,
            account_address: account.address,
            attachment_extensions,
            body_preview,
        });
    }
    Ok(out)
}

fn extract_attachment_extensions(raw: &[u8]) -> Vec<String> {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for part in msg.attachments() {
        if matches!(part.body, PartType::InlineBinary(_)) {
            continue;
        }
        let Some(name) = part.attachment_name() else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        // Same "first dot after last path segment" logic the matcher uses.
        let last = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
        if let Some(dot) = last.find('.') {
            if dot > 0 {
                let ext = &last[dot + 1..];
                if !ext.is_empty() {
                    out.push(ext.to_string());
                }
            }
        }
    }
    out
}

pub fn build_prompt(
    features: &[TrainingFeatures],
    workflow_name: &str,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Du baust einen Auto-Trigger für den Workflow \"{workflow_name}\".\n\
         Der Nutzer hat die folgenden {} Mails als Beispiele markiert, auf \
         die dieser Workflow zutreffen soll.\n\
         \n\
         Deine Aufgabe: finde den *engsten* deterministischen Filter, der \
         **genau** diese Beispiele erfassen würde und so wenig andere Mails \
         wie möglich — den sogenannten 'most narrow' Filter. Keine Raterei, \
         keine defensive Untermenge — du sollst den kleinsten gemeinsamen \
         Nenner treffen.\n\
         \n\
         Wenn du keinen gemeinsamen Nenner findest: `predicates` leer lassen \
         und in `reason` erklären, warum die Beispiele zu heterogen sind.\n\
         \n\
         Zulässige Predicates (alle werden AND-verknüpft — kombiniere mehrere \
         wenn sie die Menge schärfer schneiden):\n\
         - fromEmail                {{ \"kind\":\"fromEmail\",        \"value\":\"x@y.de\" }}\n\
         - fromDomain               {{ \"kind\":\"fromDomain\",       \"value\":\"y.de\" }}\n\
         - fromDomainIn             {{ \"kind\":\"fromDomainIn\",     \"values\":[\"a.de\",\"b.de\"] }}\n\
         - subjectContains          {{ \"kind\":\"subjectContains\",  \"value\":\"Rechnung\" }}\n\
         - hasAttachmentExtension   {{ \"kind\":\"hasAttachmentExtension\", \"extension\":\"csv\" }}\n\
         \n\
         Regeln für die Auswahl:\n\
         - `fromEmail` wenn alle Beispiele dieselbe Absenderadresse haben — \
           das ist der schärfste Filter und sollte bevorzugt werden.\n\
         - `fromDomain` wenn sie dieselbe Domain teilen aber unterschiedliche \
           Adressen (z.B. 'noreply@', 'info@' von derselben Firma).\n\
         - `fromDomainIn` wenn zwei bis fünf verschiedene Domains involviert \
           sind — fasst sie in einer Regel statt zwei.\n\
         - `hasAttachmentExtension` wenn alle Beispiele einen Anhang desselben \
           Typs haben und das nicht zufällig ist (z.B. CSV-Imports).\n\
         - `subjectContains` nur wenn eines der obigen die Menge nicht \
           ausreichend einschränkt — ein gemeinsames Wort im Betreff kann dann \
           als zweiter Filter dienen.\n\
         \n\
         Scope:\n\
         - `folderName` setzen (z.B. \"INBOX\") wenn alle Beispiele im selben \
           Ordner liegen und dieser Scope Sinn ergibt. Null lassen wenn egal.\n\
         - `accountAddress` setzen wenn alle Beispiele vom selben Account \
           stammen und die Regel nur dort gelten soll — sonst null.\n\
         \n\
         Mode:\n\
         - `auto` wenn der Workflow nur lesende/ablegende Aktionen ausführt \
           (Anhänge speichern, Body als Datei) und False-Positives unkritisch \
           sind.\n\
         - `confirm` wenn der Workflow Scripts ausführt oder sonstige \
           Nebenwirkungen hat. Im Zweifel immer `confirm`.\n\
         \n\
         Action — was bei einem Treffer mit der Mail passieren soll. \
         Wenn der bestehende Workflow ohnehin die Mail nur ablegen oder \
         archivieren würde und die Beispiele danach aussehen, schlage \
         lieber direkt eine der drei Direkt-Aktionen vor — der User \
         spart sich dann die Workflow-Pipeline:\n\
         - `run_workflow` (Default) — Mail durch den hier trainierten \
           Workflow laufen lassen. Sinnvoll wenn der Workflow echte \
           Schritte hat (Skript, Anhänge speichern usw.).\n\
         - `archive` — Mail einfach ins Account-Archiv. Sinnvoll bei \
           Newslettern, Bestellbestätigungen, allem was \"erledigt + \
           weg aber aufheben\" ist.\n\
         - `delete` — Mail in den Papierkorb. Sinnvoll bei Werbung, \
           Spam-light, generell unwichtigem Müll. Vorsichtig vorschlagen!\n\
         - `move` — Mail in einen bestimmten Ordner. `actionDest` muss \
           dann den Zielordner-Namen tragen.\n\
         \n\
         Delay (`delayMinutes`) — wie viele Minuten darf die Mail im \
         Posteingang liegen, bevor die Action greift. Minuten als Einheit \
         deckt schnellen Cleanup ('nicht in 10 Min gelesen, weg') bis \
         lange Aufbewahrung ('in 30 Tagen weg') ab. 0 = sofort (selten \
         für Direkt-Aktionen, weil der User die Mail dann nie sieht). \
         Anhaltspunkte:\n\
         - 0      bei `run_workflow` (Workflow soll sofort laufen).\n\
         - 10     bei 'Newsletter, die ich in 10 Min nicht angefasst \
           habe, sind nicht relevant'.\n\
         - 60     für 'eine Stunde reicht zum Reagieren'.\n\
         - 1440   für 'heute war noch wichtig, morgen weg' (= 1 Tag).\n\
         - 10080  für 'eine Woche' (typisch Newsletter-Lesefenster).\n\
         - 43200  für 'ein Monat' (Bestellbestätigungen, die man \
           gelegentlich noch braucht).\n\
         \n\
         Setze `dryRun: true` wenn Du Dir bei Action+Delay nicht sicher \
         bist — der User kann dann beobachten, was die Regel anrichten \
         WÜRDE, ohne dass tatsächlich Mails verschwinden.\n\
         \n",
        features.len()
    ));

    for (i, f) in features.iter().enumerate() {
        s.push_str(&format!("== Beispiel {} ==\n", i + 1));
        s.push_str(&format!("From: {}\n", f.from_email));
        s.push_str(&format!("Subject: {}\n", f.subject));
        s.push_str(&format!("Folder: {}\n", f.folder_name));
        s.push_str(&format!(
            "Account: {} ({})\n",
            f.account_display_name, f.account_address
        ));
        if !f.attachment_extensions.is_empty() {
            s.push_str(&format!(
                "Attachments: {}\n",
                f.attachment_extensions.join(", ")
            ));
        }
        if let Some(b) = &f.body_preview {
            s.push_str(&format!("Body-Anfang: {b}\n"));
        }
        s.push('\n');
    }

    s.push_str(
        "Antworte ausschließlich mit einem JSON-Objekt in diesem Schema:\n\
         \n\
         {\n\
         \x20 \"predicates\": [ /* 1..N predicates wie oben spezifiziert */ ],\n\
         \x20 \"folderName\": \"INBOX\" | null,\n\
         \x20 \"accountAddress\": \"u@example.com\" | null,\n\
         \x20 \"mode\": \"auto\" | \"confirm\",\n\
         \x20 \"action\": \"run_workflow\" | \"archive\" | \"delete\" | \"move\",\n\
         \x20 \"actionDest\": \"INBOX.Marketing\" | null,\n\
         \x20 \"delayMinutes\": 0 | 10 | 1440 | 43200 | …,\n\
         \x20 \"dryRun\": true | false,\n\
         \x20 \"reason\": \"kurze Begründung\"\n\
         }\n\
         \n\
         Keine Einleitung, keine Erklärung davor, kein Markdown. Nur das JSON.",
    );
    s
}

/// Parsed pi suggestion. Unlike spam_analysis this returns a single
/// proposal (not an array) — one rule per training run, the user can
/// decide whether to keep it and invoke the learner again for more.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleProposal {
    pub predicates: Vec<RulePredicate>,
    /// Folder scope pi suggested, or None if rule should apply
    /// across folders.
    pub folder_name: Option<String>,
    /// Resolved account id if pi's `accountAddress` hit an existing
    /// account; None means "across accounts" (or pi's hint didn't
    /// match any configured account — we don't make one up).
    pub account_id: Option<AccountId>,
    pub mode: RuleMode,
    /// Was bei einem Treffer passieren soll. Default `RunWorkflow`
    /// (alte Semantik des Trainings — die Regel feuert den Workflow,
    /// für den der User trainiert hat). pi darf alternative Direkt-
    /// Aktionen vorschlagen, wenn die Beispielmenge danach aussieht.
    pub action: RuleAction,
    pub action_dest: Option<String>,
    /// Verzögerung in Minuten, ab `mail.date`. Default 0 für `RunWorkflow`,
    /// pi schlägt für Direkt-Aktionen typischerweise 10–43200 vor
    /// (10 Min für „heute weg" bis 30 Tage für „später noch greifbar").
    pub delay_minutes: u32,
    /// Empfehlung "erstmal nur taggen". pi setzt das zur Sicherheit,
    /// wenn es eine Direkt-Aktion vorschlägt aber bei der Treffsicherheit
    /// nicht 100 % sicher ist. UI rendert dann den Trockenmodus-Schalter
    /// vor-aktiviert.
    pub dry_run: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PiRuleSuggestion {
    #[serde(default)]
    predicates: Vec<serde_json::Value>,
    #[serde(default, alias = "folder_name")]
    folder_name: Option<String>,
    #[serde(default, alias = "account_address")]
    account_address: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default, alias = "action_dest")]
    action_dest: Option<String>,
    /// Vorrang. Akzeptiert auch das alte Feld `delayDays` als Fallback,
    /// falls pi mit dem alten Schema antwortet (selten, aber kostet
    /// nichts) — wird intern × 1440 gerechnet.
    #[serde(default, alias = "delay_minutes")]
    delay_minutes: Option<u32>,
    #[serde(default, alias = "delay_days")]
    delay_days: Option<u32>,
    #[serde(default, alias = "dry_run")]
    dry_run: Option<bool>,
    #[serde(default)]
    reason: Option<String>,
}

/// Parse pi's response into a `RuleProposal`. Walks every balanced
/// `{...}` in the raw text until one parses as our schema — same
/// resilience trick as spam_analysis uses.
///
/// `accounts` is the caller's account list, used to resolve the
/// string `accountAddress` pi returned into a real `AccountId`. If
/// pi hallucinates an address, we drop the scope rather than
/// fabricate an id.
pub fn parse_pi_response(
    raw: &str,
    accounts: &[queries::AccountSummary],
) -> Result<RuleProposal, String> {
    let parsed = super::spam_analysis::iter_json_objects(raw)
        .find_map(|obj| serde_json::from_str::<PiRuleSuggestion>(obj).ok())
        .ok_or_else(|| {
            "pi-Antwort enthielt kein JSON-Objekt mit `predicates`.".to_string()
        })?;

    let mut predicates: Vec<RulePredicate> = Vec::new();
    for v in parsed.predicates {
        if let Some(pred) = parse_predicate(&v) {
            predicates.push(pred);
        }
    }

    let mode = match parsed.mode.as_deref() {
        Some("auto") => RuleMode::Auto,
        Some("confirm") => RuleMode::Confirm,
        // Default to confirm — safe side when pi forgot the field.
        _ => RuleMode::Confirm,
    };

    let folder_name = parsed
        .folder_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let account_id = parsed
        .account_address
        .as_ref()
        .and_then(|addr| {
            let want = addr.trim().to_ascii_lowercase();
            accounts
                .iter()
                .find(|a| a.address.to_ascii_lowercase() == want)
                .map(|a| a.id)
        });

    // Action — Default RunWorkflow (alte Trainings-Semantik). pi darf
    // archive/delete/move vorschlagen wenn die Beispielmenge nach simpler
    // Direkt-Aktion aussieht. `move` ohne `actionDest` ist Schrott —
    // dann fällt die Action auf RunWorkflow zurück.
    let action = parsed
        .action
        .as_deref()
        .and_then(RuleAction::parse)
        .unwrap_or(RuleAction::RunWorkflow);
    let action_dest = parsed
        .action_dest
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let action = match (action, &action_dest) {
        (RuleAction::Move, None) => RuleAction::RunWorkflow,
        (a, _) => a,
    };
    // Bei Nicht-Move-Actions ignorieren wir den vorgeschlagenen
    // actionDest, damit das Frontend keinen Zombie-Wert zeigt.
    let action_dest = if matches!(action, RuleAction::Move) {
        action_dest
    } else {
        None
    };

    // Bevorzugt das neue Minuten-Feld; fällt sonst auf altes
    // Tage-Feld zurück (× 1440 = Minuten/Tag). Beide null = 0.
    let delay_minutes = parsed
        .delay_minutes
        .or_else(|| parsed.delay_days.map(|d| d.saturating_mul(1440)))
        .unwrap_or(0);

    // Sicherheits-Default: bei Direkt-Aktionen ohne explizites
    // `dryRun` schlagen wir Trockenmodus vor — der User kann's vor dem
    // Save abwählen wenn er sich sicher ist. Bei RunWorkflow keine
    // Trocken-Empfehlung (entspricht der bisherigen Trainings-Semantik).
    let dry_run = parsed.dry_run.unwrap_or_else(|| {
        !matches!(action, RuleAction::RunWorkflow)
    });

    Ok(RuleProposal {
        predicates,
        folder_name,
        account_id,
        mode,
        action,
        action_dest,
        delay_minutes,
        dry_run,
        reason: parsed.reason.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
    })
}

/// Forgiving predicate parser. pi sometimes uses snake_case kinds or
/// invents close variants (`from_mail` vs `fromEmail`); we tolerate
/// the common misspellings and drop anything we can't map.
fn parse_predicate(v: &serde_json::Value) -> Option<RulePredicate> {
    let kind = v.get("kind")?.as_str()?.trim();
    match kind {
        "fromEmail" | "from_email" => {
            let value = v.get("value")?.as_str()?.trim().to_string();
            if value.is_empty() {
                None
            } else {
                Some(RulePredicate::FromEmail { value })
            }
        }
        "fromDomain" | "from_domain" => {
            let value = v.get("value")?.as_str()?.trim().to_string();
            if value.is_empty() {
                None
            } else {
                Some(RulePredicate::FromDomain { value })
            }
        }
        "fromDomainIn" | "from_domain_in" => {
            let arr = v.get("values").and_then(|x| x.as_array())?;
            let values: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if values.is_empty() {
                None
            } else {
                Some(RulePredicate::FromDomainIn { values })
            }
        }
        "subjectContains" | "subject_contains" => {
            let value = v.get("value")?.as_str()?.trim().to_string();
            if value.is_empty() {
                None
            } else {
                Some(RulePredicate::SubjectContains { value })
            }
        }
        "hasAttachmentExtension" | "has_attachment_extension" => {
            let extension = v
                .get("extension")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())?;
            if extension.is_empty() {
                None
            } else {
                Some(RulePredicate::HasAttachmentExtension { extension })
            }
        }
        other => {
            tracing::warn!("unknown predicate kind from pi: {other}");
            None
        }
    }
}
