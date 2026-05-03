// Workflow executor. Takes a loaded `Workflow` plus a target message
// and runs the step list top-to-bottom. Each step produces a
// `StepResult` — even when a step fails, later steps still run; the
// result list is the audit trail the UI renders.
//
// ─── Security posture ────────────────────────────────────────────────
// The `RunScript` step is the one non-reversible surface here. Three
// guards cage it:
//
//   1. **Script directory allowlist.** The executor refuses to run a
//      script whose *resolved* path isn't a direct child of the
//      `WorkflowConfig::script_dir` the user configured. Symlink games,
//      `..` path traversal, and absolute paths in the `script` field
//      are all rejected.
//   2. **No shell.** We hand a pre-tokenised `Vec<String>` of args to
//      `tokio::process::Command` — no `sh -c`, no shell expansion, so
//      a `$(rm -rf /)` in a subject never becomes a command.
//   3. **Template vars are argv, not text.** Substitution happens on
//      whole argument tokens, not inside a command string. So `$csv`
//      expands to a single argv entry even if the filename contains
//      spaces or quotes.
//
// Stage 2 will add per-workflow "confirm before first run" gating; the
// model for it already fits into `WorkflowRunResult`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::process::Command;

use crate::domain::message::MessageId;
use crate::domain::workflow::{
    BodyFormat, ParamSource, ParameterKind, ScriptParam, Step, Workflow, WorkflowId,
};
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::queries::{self, EnvelopeDetail};
use crate::state::{AppState, WorkflowConfig};

use super::message_ops;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepResult {
    pub step_index: u32,
    /// Human-readable label of the step type (`"saveAttachments"` etc.)
    pub step_type: &'static str,
    pub ok: bool,
    /// Short one-line summary shown in the list; full detail lives in
    /// `detail` (expandable on click in the result dialog).
    pub message: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunResult {
    pub workflow_id: crate::domain::workflow::WorkflowId,
    pub message_id: MessageId,
    pub steps: Vec<StepResult>,
    /// True iff *all* steps reported `ok`. Drives the summary banner
    /// colour in the result dialog.
    pub all_ok: bool,
}

/// Execute `workflow` against the message identified by `message_id`.
/// Returns a per-step audit trail. Only the caller decides what to
/// show the user (dialog vs. silent log); this function never touches
/// Tauri events directly.
pub async fn apply(
    db: &DbHandle,
    cfg: &WorkflowConfig,
    workflow: &Workflow,
    message_id: MessageId,
    prompt_values: std::collections::HashMap<String, String>,
) -> Result<WorkflowRunResult, String> {
    let (envelope, raw_body, plain_text) = load_message(db, &message_id)?;

    let mut ctx = TemplateCtx::from_envelope(&envelope);
    ctx.prompt_values = prompt_values;

    // Pre-materialise lazy vars that any step references. We only
    // unpack attachments once, even if four steps use `$csv`.
    let needs = scan_needs(&workflow.steps);
    if needs.attachments {
        match materialise_attachments(&raw_body, &message_id) {
            Ok((dir, csv, all)) => {
                ctx.attachments_dir = Some(dir);
                ctx.csv = csv;
                ctx.attachment_files = all;
            }
            Err(e) => {
                // Attachments couldn't be unpacked — steps that need
                // them will fail individually with a useful message.
                tracing::warn!(message_id = %message_id.0, "workflow: attachments prep failed: {e}");
            }
        }
    }

    let mut results: Vec<StepResult> = Vec::with_capacity(workflow.steps.len());
    for (idx, step) in workflow.steps.iter().enumerate() {
        let res = run_step(cfg, &mut ctx, idx as u32, step, &raw_body, plain_text.as_deref())
            .await;
        results.push(res);
    }

    let all_ok = results.iter().all(|r| r.ok);
    Ok(WorkflowRunResult {
        workflow_id: workflow.id,
        message_id,
        steps: results,
        all_ok,
    })
}

// ─── context ──────────────────────────────────────────────────────────

struct TemplateCtx {
    from: String,
    subject: String,
    /// RFC3339-Snapshot ($date). Kompletter Timestamp, behalten wir
    /// für Rückwärts-Kompatibilität — alte Workflows mit `$date` als
    /// Filename-Bestandteil würden sonst kaputt gehen.
    date: String,
    /// Parsed DateTime damit die Date-Varianten ($date_iso/$date_de/
    /// $time/$year/$month/$day) ohne erneutes Parsen aufgelöst werden
    /// können. Local-Time, weil Clockodo-Einträge & Filenamen meist
    /// lokal gedacht sind ("heute 14:30 zwei Stunden Support").
    date_local: chrono::DateTime<chrono::Local>,
    attachments_dir: Option<PathBuf>,
    /// First CSV in `attachments_dir`, cached for the `$csv` template
    /// var (legacy convenience — new workflows should prefer the
    /// `FirstAttachment { extension }` parameter source instead).
    csv: Option<PathBuf>,
    /// Every non-inline attachment written to `attachments_dir`, in
    /// the order the parser returned them. Used by `FirstAttachment`
    /// to resolve "give me the first file ending in `.pdf`" at argv
    /// build time.
    attachment_files: Vec<PathBuf>,
    /// Populated by a `SaveBody` step — later `RunScript` steps can
    /// then feed `$body_md` to their interpreter.
    body_md: Option<PathBuf>,
    /// Vom Caller eingesammelte Prompt-Werte. Schlüssel ist der
    /// `ScriptParam.key`, Wert das, was der User im Pre-Apply-Dialog
    /// eingetippt hat. Leer wenn keine Prompt-Params im Workflow oder
    /// wenn der Workflow von einem Auto-Trigger-Pfad gestartet wurde.
    prompt_values: std::collections::HashMap<String, String>,
}

impl TemplateCtx {
    fn from_envelope(e: &EnvelopeDetail) -> Self {
        let from = e
            .from
            .first()
            .map(|a| a.email.clone())
            .unwrap_or_default();
        Self {
            from,
            subject: e.subject.clone(),
            date: e.date.to_rfc3339(),
            date_local: e.date.with_timezone(&chrono::Local),
            attachments_dir: None,
            csv: None,
            attachment_files: Vec::new(),
            body_md: None,
            prompt_values: std::collections::HashMap::new(),
        }
    }

    /// Resolve `FirstAttachment { extension }` to a concrete path.
    /// Matches case-insensitively; compound extensions (`tar.gz`) work
    /// because we just do a literal suffix check on the lowered
    /// filename.
    fn first_attachment_of(&self, extension: &str) -> Option<PathBuf> {
        let ext = extension.trim_start_matches('.').to_ascii_lowercase();
        if ext.is_empty() {
            return None;
        }
        let suffix = format!(".{ext}");
        self.attachment_files
            .iter()
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.to_ascii_lowercase().ends_with(&suffix))
                    .unwrap_or(false)
            })
            .cloned()
    }

    /// Resolve all `$var` occurrences in `s`. Unknown names pass
    /// through unchanged so a legitimate `$PATH` in a Windows path
    /// argument survives a round trip, and so a user typo yields a
    /// visible broken command rather than a silent empty string.
    ///
    /// **UTF-8-sicher**: ältere Versionen iterierten über `bytes[i]`
    /// und zerlegten Multi-Byte-Sequenzen (`ü` → `Ã¼`) bevor sie sie
    /// in den Output schoben. Über `char_indices()` bleibt das
    /// erhalten.
    fn substitute(&self, s: &str) -> String {
        crate::application::draft_import::substitute_with(s, |name| self.lookup(name))
    }

    /// Wie `substitute`, aber jede aufgelöste **Variable** wird vor
    /// dem Splice durch `sanitize_path_segment` gefiltert — Windows-
    /// verbotene Zeichen (`< > : " / \ | ? *` plus Steuerzeichen)
    /// werden durch `_` ersetzt, trailing Dots/Spaces gestrippt. Vom
    /// User-Template literal getippte Separatoren (`/`, `\`) bleiben
    /// ungefiltert — nur Variablen-Werte gehen durch den Filter.
    ///
    /// Path-Variablen (`attachments_dir`, `csv`, `body_md`) werden
    /// bewusst **nicht** sanitisiert: ihre Werte sind absolute Pfade,
    /// deren Separatoren legitime Pfad-Trenner sind. String-
    /// Variablen wie `$subject`, `$from`, `$datetime` schon — Reply-
    /// Subjects („Re: Termin morgen") oder Time-Strings (`14:32`)
    /// produzieren sonst unter Windows bei `std::fs::write` ein
    /// `os error 3` (Path Not Found), weil das OS den `:` als
    /// Stream-Separator interpretiert.
    fn substitute_for_path(&self, s: &str) -> String {
        // UTF-8-sicher via `substitute_with`. Unterscheidet pro Var,
        // ob ihr Wert sanitisiert werden muss (Subject/Datums-Strings
        // mit `:`) oder ob er ein echter Pfad ist (`$attachments_dir`
        // & Co. — Slashes bleiben).
        crate::application::draft_import::substitute_with(s, |name| {
            self.lookup(name).map(|v| {
                if is_path_variable(name) {
                    v
                } else {
                    sanitize_path_segment(&v)
                }
            })
        })
    }

    fn lookup(&self, name: &str) -> Option<String> {
        // Date-Varianten — alle aus `date_local` abgeleitet damit die
        // Lokalzeit konsistent durchschlägt. Format-Strings sind
        // strftime-kompatibel; chrono's `format()` baut den String
        // jedes Mal frisch (kein Caching nötig, ist sub-µs).
        match name {
            "from" => Some(self.from.clone()),
            "subject" => Some(self.subject.clone()),
            "date" => Some(self.date.clone()),
            // ISO-Date: 2026-04-30. Filename-safe und das Format das
            // Clockodo / die meisten Web-APIs als Date-Eingabe wollen.
            "date_iso" => Some(self.date_local.format("%Y-%m-%d").to_string()),
            // Deutsches Anzeige-Format: 30.04.2026.
            "date_de" => Some(self.date_local.format("%d.%m.%Y").to_string()),
            // 24h-Uhrzeit, lokal: 14:30. Kein Sekunden-Anteil — bei
            // Mail-Eingang ist das selten relevant.
            "time" => Some(self.date_local.format("%H:%M").to_string()),
            // Filename-freundlicher Komplett-Zeitstempel: 20260430-1430.
            "datetime_compact" => {
                Some(self.date_local.format("%Y%m%d-%H%M").to_string())
            }
            // YYYY-MM-DD HH:MM — was Clockodo & Co. als Datum+Zeit-
            // Argument typischerweise erwarten. Lokal, kein Sekunden-
            // Anteil, mit Leerzeichen-Separator.
            "datetime" => {
                Some(self.date_local.format("%Y-%m-%d %H:%M").to_string())
            }
            // Sekunden-Form für APIs die's genauer wollen.
            "datetime_seconds" => {
                Some(self.date_local.format("%Y-%m-%d %H:%M:%S").to_string())
            }
            // ISO-8601-ish Variante mit T-Separator (RFC3339-light, ohne TZ).
            "datetime_iso" => {
                Some(self.date_local.format("%Y-%m-%dT%H:%M").to_string())
            }
            // 24h mit Sekunden, lokal: 14:30:45.
            "time_seconds" => {
                Some(self.date_local.format("%H:%M:%S").to_string())
            }
            // Einzelteile als Bausteine — wer eine eigene Form will.
            "year" => Some(self.date_local.format("%Y").to_string()),
            "month" => Some(self.date_local.format("%m").to_string()),
            "day" => Some(self.date_local.format("%d").to_string()),
            "attachments_dir" => self.attachments_dir.as_ref().map(path_to_string),
            "csv" => self.csv.as_ref().map(path_to_string),
            "body_md" => self.body_md.as_ref().map(path_to_string),
            _ => None,
        }
    }
}

fn path_to_string(p: &PathBuf) -> String {
    p.to_string_lossy().into_owned()
}

/// Vars, deren Werte echte Pfade sind (vom Workflow-Engine selbst
/// gesetzt — nicht aus User-Strings abgeleitet). Diese werden im
/// Pfad-Kontext NICHT sanitisiert, weil ihre Separatoren legitime
/// Trenner sind. Alle anderen `$var` (subject, from, date-Varianten,
/// prompt_values) werden gefiltert, bevor sie in einen Pfad
/// eingebaut werden.
fn is_path_variable(name: &str) -> bool {
    matches!(name, "attachments_dir" | "csv" | "body_md")
}

/// Filtert eine Substituierten-Wert auf Windows-Filenamen-Tauglichkeit.
/// Verbotene Zeichen → `_`, Steuerzeichen → entfallen, trailing
/// Dots/Spaces werden weggetrimmt (Windows lehnt beides am Ende einer
/// Komponente ab).
///
/// Auf POSIX wäre nur `/` und NUL verboten; trotzdem laufen wir das
/// volle Windows-Set, damit Workflow-Skripte zwischen den OSen
/// portabel bleiben — ein Subject mit `:` darf nicht auf einem
/// Linux-Build durchrutschen und beim ersten Windows-User explodieren.
fn sanitize_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => out.push('_'),
            c if (c as u32) < 0x20 => { /* Steuerzeichen verwerfen */ }
            c => out.push(c),
        }
    }
    while out.ends_with(' ') || out.ends_with('.') {
        out.pop();
    }
    out
}

#[derive(Default)]
struct Needs {
    attachments: bool,
}

fn scan_needs(steps: &[Step]) -> Needs {
    let mut n = Needs::default();
    let mut inspect = |s: &str| {
        if s.contains("$attachments_dir") || s.contains("$csv") {
            n.attachments = true;
        }
    };
    for step in steps {
        match step {
            Step::SaveAttachments { target_dir, filter } => {
                inspect(target_dir);
                if let Some(f) = filter {
                    inspect(f);
                }
            }
            Step::SaveBody { path, .. } => inspect(path),
            Step::RunScript { script, parameters } => {
                inspect(script);
                for p in parameters {
                    if !p.enabled {
                        continue;
                    }
                    match &p.source {
                        // Route template references back through
                        // `inspect` as a synthetic `$name` so we don't
                        // have to touch `n.attachments` outside the
                        // single closure path — keeps the borrow
                        // checker happy without a second dispatch.
                        ParamSource::Template { var } => {
                            inspect(&format!("${var}"));
                        }
                        ParamSource::Fixed { value } => inspect(value),
                        // Any `FirstAttachment` source needs the
                        // attachment dir materialised regardless of
                        // extension — the lookup is a scan over the
                        // already-extracted files.
                        ParamSource::FirstAttachment { .. } => {
                            inspect("$attachments_dir");
                        }
                        // Default-Templates können auf Vars wie
                        // `$subject` zeigen — durch denselben Inspect-
                        // Pfad jagen, damit Attachment-Materialisierung
                        // korrekt vorbereitet wird, falls jemand
                        // `$csv` als Fallback eintippt.
                        ParamSource::Prompt { default_template } => {
                            if let Some(tpl) = default_template {
                                inspect(tpl);
                            }
                        }
                    }
                }
            }
        }
    }
    n
}

// ─── message loading ──────────────────────────────────────────────────

fn load_message(
    db: &DbHandle,
    id: &MessageId,
) -> Result<(EnvelopeDetail, Vec<u8>, Option<String>), String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let envelope = queries::get_envelope(&conn, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Envelope nicht gefunden.".to_string())?;
    let raw = queries::get_body_raw(&conn, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "Mail-Body noch nicht geladen — Mail erst öffnen, dann Workflow anwenden."
                .to_string()
        })?;
    let body = queries::get_body(&conn, id).map_err(|e| e.to_string())?;
    Ok((envelope, raw, body.and_then(|b| b.plain_text)))
}

fn materialise_attachments(
    raw: &[u8],
    message_id: &MessageId,
) -> Result<(PathBuf, Option<PathBuf>, Vec<PathBuf>), String> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or("RFC822 parse failed")?;

    let base = std::env::temp_dir().join(format!(
        "crystalmail-wf-{}",
        message_id.0.simple()
    ));
    // Wipe a pre-existing dir from a prior run of the same message —
    // the user expects fresh contents on every workflow application.
    if base.exists() {
        let _ = std::fs::remove_dir_all(&base);
    }
    std::fs::create_dir_all(&base).map_err(|e| format!("mkdir {}: {e}", base.display()))?;

    let mut first_csv: Option<PathBuf> = None;
    let mut all_files: Vec<PathBuf> = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();

    for (idx, part) in msg.attachments().enumerate() {
        // Skip inline (cid-referenced) parts — they're body-decoration,
        // not user attachments, and shouldn't satisfy a
        // `FirstAttachment` binding.
        if matches!(part.body, PartType::InlineBinary(_)) {
            continue;
        }
        let data: Vec<u8> = match &part.body {
            PartType::Text(s) | PartType::Html(s) => s.as_bytes().to_vec(),
            PartType::Binary(b) => b.to_vec(),
            _ => continue,
        };
        let raw_name = part
            .attachment_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("attachment-{}.bin", idx + 1));
        let safe = sanitize_filename(&raw_name);
        let unique = dedupe_name(&safe, &mut used_names);
        let out_path = base.join(&unique);
        std::fs::write(&out_path, &data)
            .map_err(|e| format!("write {}: {e}", out_path.display()))?;

        if first_csv.is_none() && unique.to_ascii_lowercase().ends_with(".csv") {
            first_csv = Some(out_path.clone());
        }
        all_files.push(out_path);
    }

    Ok((base, first_csv, all_files))
}

fn sanitize_filename(name: &str) -> String {
    // Replace the path-traversal and FS-reserved set. Keep it tight
    // instead of clever — we'd rather name a CSV "report_2024.csv"
    // than re-encode umlauts the user might have expected.
    let forbidden: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|', '\0'];
    let cleaned: String = name
        .chars()
        .map(|c| if forbidden.contains(&c) || c.is_control() { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        "attachment.bin".into()
    } else {
        trimmed.to_string()
    }
}

fn dedupe_name(candidate: &str, used: &mut HashSet<String>) -> String {
    if used.insert(candidate.to_string()) {
        return candidate.to_string();
    }
    // Same filename twice in one mail — rare but legal. Append `-N`
    // before the extension.
    let (stem, ext) = match candidate.rfind('.') {
        Some(dot) if dot > 0 => (&candidate[..dot], &candidate[dot..]),
        _ => (candidate, ""),
    };
    for n in 2..1000 {
        let try_name = format!("{stem}-{n}{ext}");
        if used.insert(try_name.clone()) {
            return try_name;
        }
    }
    // Cap — 1000 duplicates is already absurd; fall through to uuid.
    let u = format!("{stem}-{}{ext}", uuid::Uuid::new_v4().simple());
    used.insert(u.clone());
    u
}

// ─── step execution ───────────────────────────────────────────────────

async fn run_step(
    cfg: &WorkflowConfig,
    ctx: &mut TemplateCtx,
    idx: u32,
    step: &Step,
    raw_body: &[u8],
    plain_text: Option<&str>,
) -> StepResult {
    match step {
        Step::SaveAttachments { target_dir, filter } => {
            // target_dir geht durch den Pfad-Filter (Subject im
            // Verzeichnis-Namen würde sonst auf Windows knallen);
            // filter ist ein Glob-Pattern, das wir 1:1 brauchen
            // (`*.pdf`-Sterne dürfen nicht gefiltert werden) — das
            // läuft weiter durch den normalen Substituer.
            let target = ctx.substitute_for_path(target_dir);
            let flt = filter.as_deref().map(|f| ctx.substitute(f));
            save_attachments_step(idx, &target, flt.as_deref(), raw_body)
        }
        Step::SaveBody { path, format } => {
            // Pfad-Substitution: Variablen-Werte werden auf Windows-
            // taugliche Filenamen-Zeichen reduziert. Sonst legt z.B.
            // ein Reply-Subject („Re: Termin") via `:` einen
            // Alternate-Stream an und der `write` schlägt mit
            // os error 3 fehl.
            let resolved = ctx.substitute_for_path(path);
            let res = save_body_step(idx, &resolved, *format, raw_body, plain_text, ctx);
            // If the write succeeded and format = md, publish $body_md
            // so subsequent RunScript steps can reference it.
            if res.ok && matches!(format, BodyFormat::Md) {
                ctx.body_md = Some(PathBuf::from(&resolved));
            }
            res
        }
        Step::RunScript { script, parameters } => {
            let script_name = ctx.substitute(script);
            run_script_step(cfg, ctx, idx, &script_name, parameters).await
        }
    }
}

fn save_attachments_step(
    idx: u32,
    target_dir: &str,
    filter: Option<&str>,
    raw: &[u8],
) -> StepResult {
    let target = PathBuf::from(target_dir);
    if let Err(e) = std::fs::create_dir_all(&target) {
        return err(idx, "saveAttachments", format!("Ordner anlegen: {e}"));
    }

    let msg = match MessageParser::default().parse(raw) {
        Some(m) => m,
        None => return err(idx, "saveAttachments", "RFC822 nicht parsebar".into()),
    };

    let mut used: HashSet<String> = HashSet::new();
    // Pre-populate with files that already exist in target so we don't
    // silently overwrite (the user might have saved something manually).
    if let Ok(read_dir) = std::fs::read_dir(&target) {
        for entry in read_dir.flatten() {
            if let Some(n) = entry.file_name().to_str() {
                used.insert(n.to_string());
            }
        }
    }

    let mut written: Vec<String> = Vec::new();
    let mut skipped_by_filter = 0;
    let mut errors: Vec<String> = Vec::new();

    for (i, part) in msg.attachments().enumerate() {
        // Skip inline parts — the user normally wants the "real"
        // attachments, not signature logos.
        if matches!(part.body, PartType::InlineBinary(_)) {
            continue;
        }
        let data: Vec<u8> = match &part.body {
            PartType::Text(s) | PartType::Html(s) => s.as_bytes().to_vec(),
            PartType::Binary(b) => b.to_vec(),
            _ => continue,
        };
        let raw_name = part
            .attachment_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("attachment-{}.bin", i + 1));
        let safe = sanitize_filename(&raw_name);

        if let Some(pat) = filter {
            if !filename_matches(&safe, pat) {
                skipped_by_filter += 1;
                continue;
            }
        }

        let unique = dedupe_name(&safe, &mut used);
        let path = target.join(&unique);
        match std::fs::write(&path, &data) {
            Ok(_) => written.push(unique),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }

    let ok = errors.is_empty();
    let msg_line = if written.is_empty() && errors.is_empty() {
        if skipped_by_filter > 0 {
            format!("0 Anhänge gespeichert ({skipped_by_filter} durch Filter übersprungen).")
        } else {
            "Keine Anhänge vorhanden.".into()
        }
    } else {
        format!(
            "{} Anhang/Anhänge in {} gespeichert.",
            written.len(),
            target.display()
        )
    };
    let detail = if errors.is_empty() {
        if written.is_empty() {
            None
        } else {
            Some(written.join("\n"))
        }
    } else {
        Some(format!(
            "Geschrieben:\n{}\n\nFehler:\n{}",
            written.join("\n"),
            errors.join("\n")
        ))
    };
    StepResult {
        step_index: idx,
        step_type: "saveAttachments",
        ok,
        message: msg_line,
        detail,
    }
}

/// Minimal glob: supports `*.ext` (suffix), `prefix*` (prefix), `*sub*`
/// (contains), and exact filename. No brackets, no `?`, no `**` — if
/// users need more, the executor can grow, but this covers the common
/// "CSV only" case.
fn filename_matches(name: &str, pattern: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let p = pattern.to_ascii_lowercase();
    match (p.starts_with('*'), p.ends_with('*')) {
        (true, true) => {
            let inner = &p[1..p.len() - 1];
            inner.is_empty() || n.contains(inner)
        }
        (true, false) => n.ends_with(&p[1..]),
        (false, true) => n.starts_with(&p[..p.len() - 1]),
        (false, false) => n == p,
    }
}

fn save_body_step(
    idx: u32,
    path: &str,
    format: BodyFormat,
    raw: &[u8],
    plain: Option<&str>,
    ctx: &TemplateCtx,
) -> StepResult {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return err(idx, "saveBody", format!("Ordner anlegen: {e}"));
            }
        }
    }

    let content: Vec<u8> = match format {
        BodyFormat::Eml => raw.to_vec(),
        BodyFormat::Txt => plain.unwrap_or("").as_bytes().to_vec(),
        BodyFormat::Md => {
            let body = plain.unwrap_or("");
            let header = format!(
                "# {}\n\nFrom: {}\nDate: {}\n\n",
                ctx.subject, ctx.from, ctx.date
            );
            (header + body).into_bytes()
        }
    };

    match std::fs::write(p, &content) {
        Ok(_) => StepResult {
            step_index: idx,
            step_type: "saveBody",
            ok: true,
            message: format!("Body in {} geschrieben.", p.display()),
            detail: None,
        },
        Err(e) => err(idx, "saveBody", format!("Schreiben: {e}")),
    }
}

async fn run_script_step(
    cfg: &WorkflowConfig,
    ctx: &TemplateCtx,
    idx: u32,
    script: &str,
    parameters: &[ScriptParam],
) -> StepResult {
    if cfg.script_dir.trim().is_empty() {
        return err(
            idx,
            "runScript",
            "Workflow-Script-Verzeichnis ist nicht konfiguriert (Einstellungen → Workflows)."
                .into(),
        );
    }
    let root = PathBuf::from(&cfg.script_dir);
    // Refuse paths — script is a *filename* relative to `root`.
    // This blocks traversal (`../../rm-rf-home.py`), absolute paths,
    // and Windows UNC-prefixed surprises in one check.
    if script.contains('/') || script.contains('\\') || Path::new(script).is_absolute() {
        return err(
            idx,
            "runScript",
            format!("Script-Name darf keine Pfadbestandteile enthalten: {script}"),
        );
    }
    let full = root.join(script);
    // Canonicalise to defeat symlink escapes. If the canonical path
    // doesn't sit inside the configured root, refuse.
    let canon_root = root.canonicalize().map_err(|e| {
        format!("Script-Verzeichnis nicht lesbar ({}): {e}", root.display())
    });
    let canon_full = full.canonicalize().map_err(|e| {
        format!("Script nicht gefunden ({}): {e}", full.display())
    });
    let (canon_root, canon_full) = match (canon_root, canon_full) {
        (Ok(r), Ok(f)) => (r, f),
        (Err(e), _) | (_, Err(e)) => return err(idx, "runScript", e),
    };
    if !canon_full.starts_with(&canon_root) {
        return err(
            idx,
            "runScript",
            "Script liegt außerhalb des erlaubten Verzeichnisses.".into(),
        );
    }

    let python = if cfg.python_bin.trim().is_empty() {
        // Fall through to the per-OS default if the field was cleared.
        WorkflowConfig::default().python_bin
    } else {
        cfg.python_bin.clone()
    };

    // Pro-Param-Trace VOR dem Build, damit der User sieht was wirklich
    // konfiguriert ist — der häufigste Bug ist „Source steht noch auf
    // Fester Wert, obwohl ich Dialog-Eingabe wollte"; das fällt hier
    // sofort auf weil der Source-Kind explizit gedruckt wird.
    for p in parameters {
        let source_desc: String = match &p.source {
            ParamSource::Fixed { value } => {
                let resolved = ctx.substitute(value);
                if resolved.is_empty() && value.is_empty() {
                    "fixed=<leer>".to_string()
                } else if resolved == *value {
                    format!("fixed={value:?}")
                } else {
                    format!("fixed={value:?} → {resolved:?}")
                }
            }
            ParamSource::Template { var } => match ctx.lookup(var) {
                Some(v) => format!("template=${var} → {v:?}"),
                None => format!("template=${var} → <nicht verfügbar>"),
            },
            ParamSource::FirstAttachment { extension } => {
                match ctx.first_attachment_of(extension) {
                    Some(p_path) => format!(
                        "firstAttachment(.{extension}) → {}",
                        p_path.display()
                    ),
                    None => format!("firstAttachment(.{extension}) → <kein Treffer>"),
                }
            }
            ParamSource::Prompt { default_template } => {
                let user_val = ctx.prompt_values.get(&p.key);
                match (user_val, default_template) {
                    (Some(v), _) if !v.trim().is_empty() => {
                        format!("prompt(user) → {v:?}")
                    }
                    (_, Some(tpl)) if !tpl.trim().is_empty() => {
                        format!(
                            "prompt(default={tpl:?}) → {:?}",
                            ctx.substitute(tpl)
                        )
                    }
                    _ => "prompt → <leer, kein Default>".to_string(),
                }
            }
        };
        tracing::info!(
            target: "workflow_script",
            step = idx,
            script = %script,
            param = %p.cli_name,
            kind = ?p.kind,
            value_type = ?p.value_type,
            enabled = p.enabled,
            required = p.required,
            "  · {source_desc}"
        );
    }

    let args = match build_argv(parameters, ctx) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(
                target: "workflow_script",
                step = idx,
                script = %script,
                error = %e,
                "✘ build_argv: Parameter konnten nicht aufgelöst werden"
            );
            return err(idx, "runScript", e);
        }
    };

    // Debug-Echo der zusammengesetzten Kommandozeile. Bewusst auf
    // INFO-Level damit's auch ohne `RUST_LOG=debug` im Terminal landet
    // — Workflow-Bugs beim User passieren typischerweise einmalig und
    // brauchen den Spuren-Snap genau dann. shell-quote-mäßig formatiert,
    // damit Du den String 1:1 ins Terminal pasten kannst.
    let echo = format_command_for_log(&python, &canon_full, &args);
    tracing::info!(
        target: "workflow_script",
        step = idx,
        script = %script,
        cwd = %canon_root.display(),
        "▶ {echo}"
    );

    let mut cmd = Command::new(&python);
    cmd.arg(&canon_full).args(&args);
    // Run from the script dir so relative `open("x.csv")` in scripts
    // still works as the script author expects.
    cmd.current_dir(&canon_root);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Two env vars every Python script running on Windows deserves:
    // PYTHONUTF8=1 so `print("ä")` doesn't die with a UnicodeEncodeError
    // in the cp1252 stdout stream, and PYTHONUNBUFFERED=1 so we capture
    // output in chunks the user can scroll through instead of one big
    // dump when the process exits. Borrowed from the launchpad sibling
    // project, which spent a while learning both lessons the hard way.
    cmd.env("PYTHONUTF8", "1").env("PYTHONUNBUFFERED", "1");
    #[cfg(target_os = "windows")]
    {
        // Suppress the ephemeral console flash when launching a .py
        // from a GUI app. 0x08000000 = CREATE_NO_WINDOW. `creation_flags`
        // is inherent on `tokio::process::Command` on Windows, so no
        // trait import is needed here.
        cmd.creation_flags(0x08000000);
    }

    let started = std::time::Instant::now();
    let out = match cmd.output().await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                target: "workflow_script",
                step = idx,
                script = %script,
                error = %e,
                "✘ Script konnte nicht gestartet werden"
            );
            return err(idx, "runScript", format!("Start fehlgeschlagen: {e}"));
        }
    };
    let elapsed_ms = started.elapsed().as_millis();

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let code = out.status.code();
    let ok = out.status.success();

    // Run-Ergebnis kompakt loggen. stdout/stderr-Größen, weil der volle
    // Inhalt im Workflow-Result-Dialog sichtbar ist — im Terminal wäre
    // er nur Rauschen. Auf failure den ersten stderr-Block dranhängen,
    // weil das beim Bugfixen die häufigste Frage ist ("warum krachelt
    // es?").
    let stderr_first_line = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();
    if ok {
        tracing::info!(
            target: "workflow_script",
            step = idx,
            script = %script,
            elapsed_ms,
            stdout_bytes = stdout.len(),
            stderr_bytes = stderr.len(),
            "✔ rc=0"
        );
    } else {
        tracing::warn!(
            target: "workflow_script",
            step = idx,
            script = %script,
            elapsed_ms,
            rc = ?code,
            stdout_bytes = stdout.len(),
            stderr_bytes = stderr.len(),
            stderr_first_line = %stderr_first_line,
            "✘ Script-Fehler"
        );
    }

    let summary = if ok {
        format!("Script '{script}' fertig (rc=0).")
    } else {
        match code {
            Some(c) => format!("Script '{script}' fehlgeschlagen (rc={c})."),
            None => format!("Script '{script}' durch Signal beendet."),
        }
    };
    let detail = {
        let mut parts: Vec<String> = Vec::new();
        if !stdout.is_empty() {
            parts.push(format!("stdout:\n{stdout}"));
        }
        if !stderr.is_empty() {
            parts.push(format!("stderr:\n{stderr}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    };
    StepResult {
        step_index: idx,
        step_type: "runScript",
        ok,
        message: summary,
        detail,
    }
}

/// Full lifecycle wrapper around `apply`: loads the workflow and
/// config, runs every step, applies the optional archive-on-success,
/// and records the run (count + last-run timestamp). Shared between
/// the Tauri command (`commands::workflows::apply_workflow`) and the
/// rule-matcher's auto branch (`workflow_rules::spawn_auto_apply`).
///
/// Returns the raw `WorkflowRunResult` so callers can either hand it
/// to the UI (manual invocation) or just log it (auto-triggered).
pub async fn apply_with_lifecycle(
    app: &AppHandle,
    db: &DbHandle,
    workflow_id: WorkflowId,
    message_id: MessageId,
    prompt_values: std::collections::HashMap<String, String>,
) -> Result<WorkflowRunResult, String> {
    let state = app.state::<AppState>();

    let workflow = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_workflow(&conn, &workflow_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Workflow nicht gefunden.".to_string())?
    };
    if !workflow.enabled {
        return Err("Workflow ist deaktiviert.".into());
    }

    let cfg = {
        let guard = state.workflow_config.lock().unwrap();
        guard.clone()
    };

    let mut result = apply(db, &cfg, &workflow, message_id, prompt_values).await?;

    // Optional archive after a fully-successful run. Kept inside
    // `apply_with_lifecycle` (rather than inlined in `apply`) so the
    // plain `apply` stays a side-effect-free step executor that a
    // future dry-run / preview mode can reuse.
    if workflow.archive_after_success && result.all_ok {
        let step_index = result.steps.len() as u32;
        match message_ops::archive(db, message_id).await {
            Ok(_) => {
                result.steps.push(StepResult {
                    step_index,
                    step_type: "archive",
                    ok: true,
                    message: "Mail ins Archiv verschoben.".into(),
                    detail: None,
                });
            }
            Err(e) => {
                result.steps.push(StepResult {
                    step_index,
                    step_type: "archive",
                    ok: false,
                    message: format!("Archivieren fehlgeschlagen: {e}"),
                    detail: None,
                });
            }
        }
    }

    // Run bookkeeping. Fire-and-forget-ish: we don't fail the apply
    // if the counter update trips, but we do wait on the ack to
    // preserve ordering against a follow-up read of `runCount`.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = db
        .writer
        .send(WriteCmd::RecordWorkflowRun {
            workflow_id: workflow.id,
            ack: tx,
        })
        .await;
    let _ = rx.await;

    Ok(result)
}

/// Bauen einer terminal-pasteable Kommandozeile fürs Tracing. Werte
/// mit Whitespace, Backslashes oder Anführungszeichen kriegen Quotes
/// + escapen — POSIX-style, weil das in PowerShell auch funktioniert
/// (PowerShell akzeptiert `"…"` für Argumente mit Spaces). cmd.exe
/// hätte andere Quote-Regeln; wer dort pastet, muss minimal nacharbeiten.
fn format_command_for_log(
    python: &str,
    script_path: &std::path::Path,
    args: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(args.len() + 2);
    parts.push(quote_for_log(python));
    parts.push(quote_for_log(&script_path.display().to_string()));
    for a in args {
        parts.push(quote_for_log(a));
    }
    parts.join(" ")
}

fn quote_for_log(s: &str) -> String {
    let needs_quotes = s.is_empty()
        || s.chars().any(|c| {
            matches!(
                c,
                ' ' | '\t' | '"' | '\'' | '\\' | '$' | '`' | '|' | '&' | ';' | '<' | '>' | '*' | '?'
            )
        });
    if !needs_quotes {
        return s.to_string();
    }
    // In Anführungszeichen einschlossene Zeichen kriegen `\"`-Escape;
    // einzelne Backslashes ebenfalls. Das ist der Mindest-Set, der
    // den String wieder lesbar macht ohne ihn unleserlich zu machen.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn err(idx: u32, kind: &'static str, msg: String) -> StepResult {
    StepResult {
        step_index: idx,
        step_type: kind,
        ok: false,
        message: msg,
        detail: None,
    }
}

/// Build the argv list for a `RunScript` step from its declared
/// parameters. Mirrors launchpad's `build_command_args`, adapted to
/// our `ParamSource` enum: `Fixed` takes its value verbatim (after
/// `$var` substitution so the user can still write `$csv` inline);
/// `Template` resolves to whatever the template ctx carries under
/// that variable name, failing fast when the var isn't materialised
/// (e.g. `$csv` referenced on a mail without a CSV attachment).
fn build_argv(params: &[ScriptParam], ctx: &TemplateCtx) -> Result<Vec<String>, String> {
    // Stable order — the editor already persists `order`, but we
    // re-sort here so the executor doesn't depend on upstream doing
    // it. Two params with the same order fall back to insertion order
    // via `sort_by_key`'s stable sort.
    let mut sorted: Vec<&ScriptParam> = params.iter().collect();
    sorted.sort_by_key(|p| p.order);

    let mut argv: Vec<String> = Vec::new();
    for p in sorted {
        if !p.enabled {
            continue;
        }

        // Resolve the source to a concrete value.
        let value = match &p.source {
            ParamSource::Fixed { value } => ctx.substitute(value),
            ParamSource::Template { var } => match ctx.lookup(var) {
                Some(v) => v,
                None => {
                    if p.required {
                        return Err(format!(
                            "Parameter '{}' verlangt Template-Var ${}, aber die ist für diese Mail nicht verfügbar.",
                            p.label, var
                        ));
                    }
                    // Optional + missing var = skip the param entirely.
                    String::new()
                }
            },
            ParamSource::FirstAttachment { extension } => {
                match ctx.first_attachment_of(extension) {
                    Some(p_path) => p_path.to_string_lossy().into_owned(),
                    None => {
                        if p.required {
                            return Err(format!(
                                "Parameter '{}' braucht einen .{} -Anhang, aber die Mail hat keinen.",
                                p.label, extension
                            ));
                        }
                        String::new()
                    }
                }
            }
            ParamSource::Prompt { default_template } => {
                // Reihenfolge:
                //   1. Wenn der User im Pre-Apply-Dialog einen Wert
                //      eingegeben hat, gewinnt der.
                //   2. Sonst: Default-Template auflösen (kann ein
                //      Template-String wie `$subject` oder ein Literal
                //      sein) — gibt der Auto-Trigger-Fall einen
                //      sinnvollen Fallback, ohne dass jemand einen
                //      Dialog beantworten muss.
                //   3. Sonst: leerer String → unten in der Required-
                //      Prüfung als "Pflichtparameter fehlt".
                match ctx.prompt_values.get(&p.key) {
                    Some(v) if !v.trim().is_empty() => v.clone(),
                    _ => match default_template {
                        Some(tpl) if !tpl.trim().is_empty() => {
                            ctx.substitute(tpl)
                        }
                        _ => {
                            if p.required {
                                return Err(format!(
                                    "Parameter '{}' verlangt eine Dialog-Eingabe (kein Wert übergeben, kein Default).",
                                    p.label
                                ));
                            }
                            String::new()
                        }
                    },
                }
            }
        };

        match p.kind {
            ParameterKind::Flag => {
                // Truthy semantics matching launchpad: only emit the
                // flag when the value evaluates as "yes". This lets
                // a flag be driven by a template var (e.g. a custom
                // one that resolves to "true" conditionally).
                let truthy = matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
                if truthy {
                    argv.push(p.cli_name.clone());
                }
            }
            ParameterKind::Option => {
                if value.is_empty() {
                    if p.required {
                        return Err(format!("Pflichtparameter '{}' fehlt.", p.label));
                    }
                } else {
                    argv.push(p.cli_name.clone());
                    argv.push(value);
                }
            }
            ParameterKind::Positional => {
                if value.is_empty() {
                    if p.required {
                        return Err(format!("Pflichtparameter '{}' fehlt.", p.label));
                    }
                } else {
                    argv.push(value);
                }
            }
        }
    }
    Ok(argv)
}

// ─── Tests ────────────────────────────────────────────────────────────────────
//
// `build_argv` und `TemplateCtx::substitute` sitzen auf der Grenze zwischen
// User-controlled Mail-Daten (Subject, From, Anhänge) und dem Prozess-Launcher.
// Auch wenn argv-Aufrufe (im Gegensatz zu Shell-Strings) per se kein
// Metachar-Escaping brauchen, sind hier zwei Klassen von Fehlverhalten
// realistisch:
//   1. Re-Substitution — wenn der Subject `$body_md` enthält und das nochmal
//      durch `substitute()` läuft, könnte der Angreifer indirekt ein anderes
//      Template-Var auflösen. Tests stellen sicher: Substitution passiert
//      nur einmal, der eingefügte Wert wird nicht erneut gescannt.
//   2. Pflicht-Param-Bypass — `required = true` darf nicht einfach durch
//      `enabled = false` umgangen werden, sonst kann ein User per Editor
//      die Pipeline-Annahme aushebeln.
//
// Die Tests berühren keine Disk und keine SQL — pure Logik.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::workflow::{
        ParamSource, ParameterKind, ScriptParam, ValueType,
    };

    fn empty_ctx() -> TemplateCtx {
        let dt = chrono::DateTime::parse_from_rfc3339("2026-04-26T10:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        TemplateCtx {
            from: "alice@example.com".into(),
            subject: "Daily Report".into(),
            date: "2026-04-26T10:00:00+00:00".into(),
            date_local: dt,
            attachments_dir: None,
            csv: None,
            attachment_files: Vec::new(),
            body_md: None,
            prompt_values: std::collections::HashMap::new(),
        }
    }

    fn ctx_with_attachments(files: Vec<&str>) -> TemplateCtx {
        let mut ctx = empty_ctx();
        ctx.attachment_files = files.into_iter().map(PathBuf::from).collect();
        ctx
    }

    /// Test-Fixture für ScriptParam — füllt nur die für `build_argv`
    /// relevanten Felder; alle anderen kriegen Defaults.
    fn p(
        cli_name: &str,
        kind: ParameterKind,
        source: ParamSource,
        required: bool,
        order: u32,
    ) -> ScriptParam {
        ScriptParam {
            key: cli_name.trim_start_matches('-').replace('-', "_"),
            cli_name: cli_name.into(),
            kind,
            label: cli_name.into(),
            value_type: ValueType::String,
            choices: vec![],
            help_text: None,
            required,
            default_value: None,
            source,
            order,
            enabled: true,
        }
    }

    // ─── substitute ──────────────────────────────────────────────────

    #[test]
    fn substitute_replaces_known_var() {
        let ctx = empty_ctx();
        assert_eq!(ctx.substitute("Hi $from"), "Hi alice@example.com");
    }

    #[test]
    fn substitute_passes_unknown_var_through() {
        let ctx = empty_ctx();
        // $PATH ist kein bekanntes Template-Var → bleibt literally erhalten.
        // Wichtig für Windows-Pfade wie "%PATH%" und Bash-Aufrufe.
        assert_eq!(ctx.substitute("/bin:$PATH:/end"), "/bin:$PATH:/end");
    }

    #[test]
    fn substitute_respects_var_terminator() {
        let ctx = empty_ctx();
        // $from soll bei dem Punkt enden, nicht "$from." als Var lesen.
        assert_eq!(ctx.substitute("$from."), "alice@example.com.");
    }

    /// Sicherheits-relevant: der Wert eines Template-Vars wird *nicht* erneut
    /// substituiert. Sonst könnte ein Angreifer mit gesteuertem Subject
    /// ("$body_md") indirekt auf eine andere Var zeigen.
    #[test]
    fn substitute_does_not_recurse_into_resolved_value() {
        let mut ctx = empty_ctx();
        ctx.subject = "$from".into();
        // Substitute("$subject") muss "$from" liefern (literally), nicht
        // "alice@example.com" (re-substituted).
        assert_eq!(ctx.substitute("$subject"), "$from");
    }

    #[test]
    fn substitute_handles_dollar_at_end_of_string() {
        let ctx = empty_ctx();
        assert_eq!(ctx.substitute("ende$"), "ende$");
    }

    /// Regression: byte-für-byte-Iteration zerlegte UTF-8-Multi-Byte-
    /// Sequenzen in einzelne Latin-1-Zeichen (`ü` = `0xC3 0xBC` →
    /// `Ã¼`). Mit `char_indices()`-basierter Iteration bleibt es
    /// erhalten.
    #[test]
    fn substitute_preserves_utf8_bytes() {
        let mut ctx = empty_ctx();
        ctx.subject = "Müller".into();
        // Sowohl der Var-Wert (`$subject`) als auch die Umlaute im
        // umgebenden Text müssen heil durchkommen.
        assert_eq!(
            ctx.substitute("Hallo $subject — überrascht!"),
            "Hallo Müller — überrascht!"
        );
    }

    #[test]
    fn substitute_for_path_preserves_utf8_in_template() {
        let mut ctx = empty_ctx();
        ctx.subject = "Test".into();
        // Pfad-Skelett mit Umlaut + Var-Subst → Umlaut bleibt erhalten.
        assert_eq!(
            ctx.substitute_for_path("C:/Müll/$subject.md"),
            "C:/Müll/Test.md"
        );
    }

    // ─── substitute_for_path: Windows-Filename-Sanitize ──────────────
    //
    // Reproduziert den User-Bug "saveBody schlägt fehl mit os error 3"
    // beim Sichern von Sent-Folder-Mails: Reply-Subjects haben fast
    // immer `Re: …` und das `:` macht Windows beim Schreiben des
    // Files giftig.

    #[test]
    fn substitute_for_path_strips_colons_from_subject() {
        let mut ctx = empty_ctx();
        ctx.subject = "Re: Termin morgen".into();
        // Subject im Path-Kontext: `:` wird zu `_`.
        assert_eq!(
            ctx.substitute_for_path("C:/archiv/$subject.md"),
            "C:/archiv/Re_ Termin morgen.md"
        );
    }

    #[test]
    fn substitute_for_path_strips_path_separators_from_values() {
        let mut ctx = empty_ctx();
        ctx.subject = "Q1/2026 Status".into();
        // `/` im Subject darf NICHT zu einem Pfad-Trenner werden,
        // sonst landet die Datei in einem ungewollten Unterordner.
        assert_eq!(
            ctx.substitute_for_path("/archive/$subject.md"),
            "/archive/Q1_2026 Status.md"
        );
    }

    #[test]
    fn substitute_for_path_preserves_user_typed_separators() {
        let ctx = empty_ctx();
        // Nichts an Variablen → Template-Skelett bleibt unverändert.
        assert_eq!(
            ctx.substitute_for_path("C:/archiv/sub/file.md"),
            "C:/archiv/sub/file.md"
        );
    }

    #[test]
    fn substitute_for_path_preserves_path_var_separators() {
        let mut ctx = empty_ctx();
        ctx.attachments_dir = Some(PathBuf::from("C:/data/run-42/atts"));
        // Path-Vars (`attachments_dir`/`csv`/`body_md`) werden nicht
        // sanitisiert — ihre Slashes sind echte Pfad-Trenner.
        assert_eq!(
            ctx.substitute_for_path("$attachments_dir/out.md"),
            "C:/data/run-42/atts/out.md"
        );
    }

    #[test]
    fn substitute_for_path_strips_colon_from_datetime() {
        let ctx = empty_ctx();
        // `$datetime` enthält `HH:MM` mit `:` — im Pfad-Kontext muss
        // das verschwinden.
        let resolved = ctx.substitute_for_path("C:/log/$datetime.md");
        assert!(
            !resolved[3..].contains(':'),
            "datetime should be sanitized, got: {resolved}"
        );
    }

    #[test]
    fn substitute_for_path_strips_trailing_dots() {
        let mut ctx = empty_ctx();
        ctx.subject = "End with dots...".into();
        // Trailing Dots sind auf Windows in Filenamen verboten
        // — werden hinten weggetrimmt.
        let r = ctx.substitute_for_path("$subject.md");
        assert_eq!(r, "End with dots.md");
    }

    #[test]
    fn substitute_for_path_strips_control_chars() {
        let mut ctx = empty_ctx();
        ctx.subject = "tab\there\x01end".into();
        let r = ctx.substitute_for_path("$subject.md");
        assert_eq!(r, "tabhereend.md");
    }

    // ─── build_argv: ParamSource::Fixed ──────────────────────────────

    #[test]
    fn fixed_positional_passes_value_verbatim() {
        let params = vec![p(
            "input",
            ParameterKind::Positional,
            ParamSource::Fixed { value: "report.csv".into() },
            true,
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert_eq!(argv, vec!["report.csv"]);
    }

    #[test]
    fn fixed_value_substitutes_inline_template_vars() {
        let params = vec![p(
            "--from",
            ParameterKind::Option,
            ParamSource::Fixed { value: "$from".into() },
            true,
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert_eq!(argv, vec!["--from", "alice@example.com"]);
    }

    /// Verteidigung gegen Command-Injection: ein Subject mit Shell-Metachars
    /// wandert verbatim als ein einzelnes argv-Element durch. Da wir nicht
    /// durch eine Shell pipen (siehe `tokio::process::Command::arg`), gibt
    /// es keine Interpretation von `;`, `&&`, ``-Quotes etc.
    #[test]
    fn fixed_value_with_shell_metachars_passes_verbatim() {
        let mut ctx = empty_ctx();
        ctx.subject = "report; rm -rf / && echo pwn".into();
        let params = vec![p(
            "--label",
            ParameterKind::Option,
            ParamSource::Fixed { value: "$subject".into() },
            true,
            0,
        )];
        let argv = build_argv(&params, &ctx).unwrap();
        assert_eq!(
            argv,
            vec!["--label", "report; rm -rf / && echo pwn"],
            "metachars must reach argv as one element, no splitting"
        );
    }

    // ─── build_argv: ParamSource::Template ───────────────────────────

    #[test]
    fn template_resolves_known_var() {
        let params = vec![p(
            "--subject",
            ParameterKind::Option,
            ParamSource::Template { var: "subject".into() },
            true,
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert_eq!(argv, vec!["--subject", "Daily Report"]);
    }

    #[test]
    fn template_required_missing_var_errors() {
        let params = vec![p(
            "--csv",
            ParameterKind::Option,
            ParamSource::Template { var: "csv".into() },
            true,
            0,
        )];
        let err = build_argv(&params, &empty_ctx()).unwrap_err();
        assert!(err.contains("Template-Var $csv"), "error msg: {err}");
    }

    #[test]
    fn template_optional_missing_var_skipped() {
        let params = vec![p(
            "--csv",
            ParameterKind::Option,
            ParamSource::Template { var: "csv".into() },
            false, // not required
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        // Empty value + Option + not required → param wird komplett weg-
        // gelassen (kein "--csv" mit leerem String).
        assert!(argv.is_empty());
    }

    // ─── build_argv: ParamSource::FirstAttachment ────────────────────

    #[test]
    fn first_attachment_picks_matching_extension() {
        let ctx =
            ctx_with_attachments(vec!["/tmp/data.txt", "/tmp/report.csv", "/tmp/log.txt"]);
        let params = vec![p(
            "--input",
            ParameterKind::Option,
            ParamSource::FirstAttachment { extension: "csv".into() },
            true,
            0,
        )];
        let argv = build_argv(&params, &ctx).unwrap();
        assert_eq!(argv, vec!["--input", "/tmp/report.csv"]);
    }

    #[test]
    fn first_attachment_required_no_match_errors() {
        let ctx = ctx_with_attachments(vec!["/tmp/only.txt"]);
        let params = vec![p(
            "--input",
            ParameterKind::Option,
            ParamSource::FirstAttachment { extension: "csv".into() },
            true,
            0,
        )];
        let err = build_argv(&params, &ctx).unwrap_err();
        assert!(err.contains(".csv"), "error msg: {err}");
    }

    // ─── build_argv: ParameterKind variants ──────────────────────────

    #[test]
    fn flag_truthy_value_emits_cli_name() {
        let params = vec![p(
            "--verbose",
            ParameterKind::Flag,
            ParamSource::Fixed { value: "true".into() },
            false,
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert_eq!(argv, vec!["--verbose"]);
    }

    #[test]
    fn flag_falsy_value_omits_cli_name() {
        let params = vec![p(
            "--verbose",
            ParameterKind::Flag,
            ParamSource::Fixed { value: "false".into() },
            false,
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert!(argv.is_empty());
    }

    #[test]
    fn flag_treats_yes_no_as_truthy_falsy() {
        let truthy = ["1", "true", "yes", "on", "TRUE", "Yes"];
        let falsy = ["0", "false", "no", "off", ""];
        for v in truthy {
            let params = vec![p(
                "--x",
                ParameterKind::Flag,
                ParamSource::Fixed { value: v.into() },
                false,
                0,
            )];
            let argv = build_argv(&params, &empty_ctx()).unwrap();
            assert_eq!(argv, vec!["--x"], "expected truthy for {v:?}");
        }
        for v in falsy {
            let params = vec![p(
                "--x",
                ParameterKind::Flag,
                ParamSource::Fixed { value: v.into() },
                false,
                0,
            )];
            let argv = build_argv(&params, &empty_ctx()).unwrap();
            assert!(argv.is_empty(), "expected falsy for {v:?}");
        }
    }

    #[test]
    fn option_emits_cli_name_then_value() {
        let params = vec![p(
            "--out",
            ParameterKind::Option,
            ParamSource::Fixed { value: "/tmp/x".into() },
            true,
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert_eq!(argv, vec!["--out", "/tmp/x"]);
    }

    #[test]
    fn option_required_with_empty_value_errors() {
        let mut params = vec![p(
            "--out",
            ParameterKind::Option,
            ParamSource::Fixed { value: "".into() },
            true,
            0,
        )];
        // Fixed-Source "" geht direkt durch substitute() (immer noch ""),
        // dann ist value.is_empty() → required → Err.
        params[0].source = ParamSource::Fixed { value: "".into() };
        let err = build_argv(&params, &empty_ctx()).unwrap_err();
        assert!(err.contains("Pflichtparameter"), "error msg: {err}");
    }

    #[test]
    fn positional_emits_value_only() {
        let params = vec![p(
            "input",
            ParameterKind::Positional,
            ParamSource::Fixed { value: "x.csv".into() },
            true,
            0,
        )];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert_eq!(argv, vec!["x.csv"]);
    }

    // ─── enabled / order ─────────────────────────────────────────────

    #[test]
    fn disabled_param_does_not_contribute_to_argv() {
        let mut params = vec![p(
            "--debug",
            ParameterKind::Flag,
            ParamSource::Fixed { value: "true".into() },
            false,
            0,
        )];
        params[0].enabled = false;
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert!(argv.is_empty());
    }

    /// Trotz `required=true` darf ein disabled Param NICHT als Pflicht
    /// gezählt werden — der Editor zeigt diese Kombination explizit als
    /// "deaktiviert" an, der Executor respektiert das.
    #[test]
    fn disabled_required_param_does_not_error() {
        let mut params = vec![p(
            "--mandatory",
            ParameterKind::Option,
            ParamSource::Fixed { value: "".into() },
            true,
            0,
        )];
        params[0].enabled = false;
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert!(argv.is_empty());
    }

    /// Stable-Sort by `order`: kleinere Order-Werte kommen früher in argv.
    /// Wichtig für argparse-Positionals, deren Reihenfolge bedeutsam ist.
    #[test]
    fn order_field_drives_argv_order() {
        let params = vec![
            p(
                "second",
                ParameterKind::Positional,
                ParamSource::Fixed { value: "B".into() },
                true,
                10,
            ),
            p(
                "first",
                ParameterKind::Positional,
                ParamSource::Fixed { value: "A".into() },
                true,
                5,
            ),
            p(
                "third",
                ParameterKind::Positional,
                ParamSource::Fixed { value: "C".into() },
                true,
                15,
            ),
        ];
        let argv = build_argv(&params, &empty_ctx()).unwrap();
        assert_eq!(argv, vec!["A", "B", "C"]);
    }
}
