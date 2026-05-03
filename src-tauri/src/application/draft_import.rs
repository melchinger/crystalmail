// Externer "Draft-aus-Template"-Import.
//
// Use-Case: ein Python-Script (Buchhaltung, CRM, …) hat gerade ein
// Rechnungs-PDF erzeugt und will jetzt eine Mail mit diesem Anhang +
// einem vorhandenen Markdown-Mailtemplate als Body in den Composer
// von CrystalMail laden — vom User gegengeprüft, dann manuell
// verschickt.
//
// Trigger ist die App-Kommandozeile (deckt URL-Scheme `crystalmail://`
// auf Windows automatisch mit ab, weil das OS dort den Argumenten-
// Vektor anhängt). Zwei Aufruf-Formen:
//
//   crystalmail.exe --draft-from-template <path>
//                   [--param key=value]...
//                   [--attach <path>]...
//
//   crystalmail.exe --draft-job <path-to-json>
//
// Die Job-JSON-Variante ist die Skript-freundliche, weil sie um
// jegliches Shell-Quoting herumsegelt (Pfade mit Leerzeichen, Umlaute,
// `=`-Zeichen in Werten, …). Ein Python-Script schreibt sich die
// Datei einfach lokal und ruft die App damit auf — fertig.
//
// Template-Format ist Markdown mit minimalem `key: value`-Frontmatter:
//
//   ---
//   to: $customer_email
//   cc: backoffice@firma.de
//   subject: Rechnung $invoice_no — $month $year
//   account: alice@firma.de
//   ---
//   Hallo,
//
//   anbei die Rechnung $invoice_no.
//
//   VG, Alice
//
// `$key`-Variablen werden mit Werten aus `--param` ersetzt; zusätzlich
// die üblichen Datums-Varianten (`$date_iso`, `$datetime`, `$year`, …)
// damit Templates ohne explizite Param-Zwang ihren eigenen Datum-Stempel
// kriegen können.
//
// Sicherheits-Modell:
//   * KEIN Auto-Send. Wir bauen einen Draft im Composer auf und legen
//     den Final-Send-Knopf in die Hände des Users. Externer Trigger
//     darf niemals ohne Augen-Kontakt eine Mail rausschicken.
//   * Pfade werden 1:1 verwendet (absolut erwartet); keine Expansion
//     auf `~` o.ä., kein Wildcard-Glob. Jeder Param/Anhang muss
//     wörtlich kommen.
//   * Frontmatter-Felder akzeptieren ausschließlich klassische Header-
//     Felder (`to`, `cc`, `bcc`, `subject`, `account`); alles andere
//     wird ignoriert. Kein Pfad-Eval, kein RunScript, keine
//     Spät-Lade-Tricks aus dem Frontmatter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Vom Caller gelieferter Roh-Auftrag. Identisch zu dem, was wir aus
/// der Argv-Sequenz oder einer Job-JSON parsen — interner Bündel-Typ.
#[derive(Debug, Clone, Default)]
pub struct ImportRequest {
    pub template_path: PathBuf,
    pub params: HashMap<String, String>,
    pub attachments: Vec<PathBuf>,
}

/// Erkannter Frontmatter-Header. Alles `Option`, weil das Template
/// auch ein nackter Body ohne Frontmatter sein darf.
#[derive(Debug, Clone, Default)]
pub struct TemplateMeta {
    pub to: Option<String>,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub subject: Option<String>,
    /// Mail-Adresse des From-Accounts. Frontend wählt damit die
    /// passende Identität aus der Account-Liste; ist sie leer oder
    /// matched nichts, fällt der Composer auf den Default-Account
    /// zurück.
    pub account: Option<String>,
}

/// Frontmatter + Body, getrennt aufgeteilt.
#[derive(Debug, Clone, Default)]
pub struct TemplateData {
    pub meta: TemplateMeta,
    pub body: String,
}

/// Was wir am Ende ans Frontend weiterreichen — gleicher Shape wie
/// `ComposeDraft` auf der TS-Seite, einmal substituiert und mit
/// resolveden Anhängen.
///
/// `attachments` trägt den Composer-Anhang-Shape mit `clientId`,
/// `path`, `filename`, `sizeBytes`, `mimeType`. Wir wiederholen den
/// hier statt Cross-Modul-Import, weil das Compose-Modul rein
/// frontend-seitig lebt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedImportDraft {
    pub account_email: Option<String>,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<PreparedAttachment>,
    /// Roh-Pfad des Templates, rein für UI-Anzeige ("Aus … geladen").
    pub source_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedAttachment {
    pub client_id: String,
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub mime_type: Option<String>,
}

/// Variablen-Kontext für die Template-Substitution. Enthält alle
/// `--param`-Werte plus Datums-Varianten basierend auf `now()` in der
/// lokalen Zeitzone (Buchhaltungs-Use-Case denkt lokal).
struct SubstCtx {
    params: HashMap<String, String>,
    now: chrono::DateTime<chrono::Local>,
}

impl SubstCtx {
    fn new(params: HashMap<String, String>) -> Self {
        Self {
            params,
            now: chrono::Local::now(),
        }
    }

    /// Resolve a single `$name`. Format-Strings 1:1 identisch zu
    /// `application/workflows.rs::lookup`, damit User dieselben Date-
    /// Varianten in Workflow-Templates UND Import-Templates erwarten
    /// können.
    fn lookup(&self, name: &str) -> Option<String> {
        // Param-Treffer hat Vorrang vor Built-ins, damit ein Caller
        // mit `--param year=2099` das gewinnt — Tests und Spezial-
        // Templates sollen die Date-Varianten überschreiben können.
        if let Some(v) = self.params.get(name) {
            return Some(v.clone());
        }
        let pad = |n: u32, w: usize| format!("{:0>w$}", n, w = w);
        let y = self.now.format("%Y").to_string();
        let m = pad(self.now.format("%-m").to_string().parse().unwrap_or(0), 2);
        let d = pad(self.now.format("%-d").to_string().parse().unwrap_or(0), 2);
        let h = pad(self.now.format("%-H").to_string().parse().unwrap_or(0), 2);
        let mi = pad(self.now.format("%-M").to_string().parse().unwrap_or(0), 2);
        let s = pad(self.now.format("%-S").to_string().parse().unwrap_or(0), 2);
        Some(match name {
            "date_iso" => format!("{y}-{m}-{d}"),
            "date_de" => format!("{d}.{m}.{y}"),
            "datetime" => format!("{y}-{m}-{d} {h}:{mi}"),
            "datetime_seconds" => format!("{y}-{m}-{d} {h}:{mi}:{s}"),
            "datetime_iso" => format!("{y}-{m}-{d}T{h}:{mi}"),
            "datetime_compact" => format!("{y}{m}{d}-{h}{mi}"),
            "time" => format!("{h}:{mi}"),
            "time_seconds" => format!("{h}:{mi}:{s}"),
            "year" => y,
            "month" => m,
            "day" => d,
            _ => return None,
        })
    }

    /// Identisch im Verhalten zu `TemplateCtx::substitute` —
    /// unbekannte Variablen bleiben unverändert (`$PATH` in Pfaden),
    /// damit User-Tippfehler im Output sichtbar sind statt still
    /// zu leeren Strings zu kollabieren.
    ///
    /// **UTF-8-sicher**: iteriert über `char_indices()`, nicht über
    /// rohe Bytes. Frühere Implementierungen mit `bytes[i] as char`
    /// haben Multi-Byte-Sequenzen wie `ü` (`0xC3 0xBC`) in zwei
    /// Latin-1-Zeichen (`Ã¼`) zerlegt — exakt der Bug, der bei
    /// Umlauten im Template-Body sichtbar wurde.
    fn substitute(&self, s: &str) -> String {
        substitute_with(s, |name| self.lookup(name))
    }
}

/// Gemeinsamer UTF-8-sicherer Substituer für `$name`-Form. Genutzt
/// von `SubstCtx::substitute` und potenziell weiteren Modulen — das
/// Iterations-Schema ist bei jedem identisch, der einzige Unterschied
/// ist die Lookup-Funktion. Hier extrahiert, damit ein Bug-Fix nicht
/// dreimal gleich gepflegt werden muss.
pub fn substitute_with<F: Fn(&str) -> Option<String>>(s: &str, lookup: F) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.char_indices().peekable();
    while let Some((_, c)) = iter.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        // `$` gesehen — versuche `[A-Za-z_][A-Za-z0-9_]*` zu harvesten.
        // Wir nutzen die Byte-Offsets aus `char_indices()`, weil wir am
        // Ende einen `&str`-Slice für den Var-Namen rausziehen wollen.
        let var_start = match iter.peek() {
            Some(&(j, _)) => j,
            None => {
                out.push('$');
                continue;
            }
        };
        let mut var_end = var_start;
        let mut first = true;
        while let Some(&(j, vc)) = iter.peek() {
            let ok = if first {
                vc.is_ascii_alphabetic() || vc == '_'
            } else {
                vc.is_ascii_alphanumeric() || vc == '_'
            };
            if !ok {
                break;
            }
            var_end = j + vc.len_utf8();
            iter.next();
            first = false;
        }
        if var_end > var_start {
            let name = &s[var_start..var_end];
            if let Some(val) = lookup(name) {
                out.push_str(&val);
                continue;
            }
            // Var-Name harvested aber kein Lookup-Treffer → `$name`
            // literally durchreichen, damit User-Tippfehler sichtbar
            // bleiben.
            out.push('$');
            out.push_str(name);
            continue;
        }
        // Kein gültiger Var-Name nach `$` → nur `$` ausgeben.
        out.push('$');
    }
    out
}

/// `~`-Expansion für User-bequeme CLI-Aufrufe. Windows-CMD und
/// PowerShell expandieren `~` *nicht* in Argumenten an native Exes,
/// daher kommt das hier oft literal an. Akzeptiert `~`, `~/foo`,
/// und `~\foo` — alles relativ zum Home-Verzeichnis. Schlägt
/// `dirs::home_dir()` fehl (sehr unüblich, kaputtes Profil), bleibt
/// der Pfad unverändert und der nachgelagerte File-Open-Error ist
/// dann selbsterklärend.
pub fn expand_user_path(p: &Path) -> PathBuf {
    let s = match p.to_str() {
        Some(s) => s,
        None => return p.to_path_buf(),
    };
    if !s.starts_with('~') {
        return p.to_path_buf();
    }
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return p.to_path_buf(),
    };
    if s == "~" {
        return home;
    }
    // `~/...` oder `~\...`. Zwei Bytes weg, Rest als relativer Pfad.
    let rest = s[1..].trim_start_matches(['/', '\\']);
    home.join(rest)
}

/// Argv-Parser für die beiden CLI-Aufrufformen. Tolerant bei
/// Fehlern — unbekannte Flags ignorieren wir (App soll auch starten
/// wenn der User sie ohne Import-Trigger aufruft), aber wenn ein
/// `--param` ohne `=` kommt oder `--attach` ohne Pfad, knallt's mit
/// klarem Fehler statt stiller Verwerfung.
pub fn parse_argv(argv: &[String]) -> Result<Option<ImportRequest>, String> {
    let mut iter = argv.iter().skip(1).peekable();
    let mut req: Option<ImportRequest> = None;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--draft-from-template" | "-T" => {
                let path = iter
                    .next()
                    .ok_or_else(|| "--draft-from-template ohne Pfad".to_string())?;
                let r = req.get_or_insert_with(ImportRequest::default);
                r.template_path = expand_user_path(Path::new(path));
            }
            "--param" | "-P" => {
                let kv = iter
                    .next()
                    .ok_or_else(|| "--param ohne key=value".to_string())?;
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| format!("--param erwartet key=value, bekam: {kv}"))?;
                let r = req.get_or_insert_with(ImportRequest::default);
                r.params.insert(k.to_string(), v.to_string());
            }
            "--attach" | "-A" => {
                let path = iter
                    .next()
                    .ok_or_else(|| "--attach ohne Pfad".to_string())?;
                let r = req.get_or_insert_with(ImportRequest::default);
                r.attachments.push(expand_user_path(Path::new(path)));
            }
            "--draft-job" | "-J" => {
                let path = iter
                    .next()
                    .ok_or_else(|| "--draft-job ohne Pfad".to_string())?;
                let job = read_job_json(Path::new(path))?;
                req = Some(job);
            }
            _ => { /* unbekanntes Flag — Tauri/Cargo/single_instance liefern allerlei mit. */ }
        }
    }

    if let Some(r) = &req {
        if r.template_path.as_os_str().is_empty() {
            return Err(
                "--draft-from-template fehlt — entweder direkt oder via --draft-job angeben"
                    .to_string(),
            );
        }
    }
    Ok(req)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobJson {
    template: String,
    #[serde(default)]
    params: HashMap<String, String>,
    #[serde(default)]
    attachments: Vec<String>,
}

fn read_job_json(path: &Path) -> Result<ImportRequest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Job-JSON lesen ({}): {e}", path.display()))?;
    let job: JobJson = serde_json::from_str(&text)
        .map_err(|e| format!("Job-JSON parsen ({}): {e}", path.display()))?;
    Ok(ImportRequest {
        template_path: expand_user_path(Path::new(&job.template)),
        params: job.params,
        attachments: job
            .attachments
            .into_iter()
            .map(|s| expand_user_path(Path::new(&s)))
            .collect(),
    })
}

/// Splittet ein Roh-Markdown in Frontmatter + Body. Erkennt zwei
/// Frontmatter-Formen:
///   1. **YAML-Style** mit `---` … `---`-Markern am Datei-Anfang.
///   2. **RFC-822-Style**: Header-Block direkt am Datei-Anfang
///      (`To: …`, `From: …`, `Subject: …`), abgeschlossen durch eine
///      Leerzeile. Wird nur erkannt, wenn die *erste* nicht-leere
///      Zeile selbst wie ein Header aussieht (`Bekannter-Key: Wert`).
///      Dadurch werden gewöhnliche Body-Texte mit Doppelpunkt im
///      ersten Wort (z.B. „Hallo: anbei …") nicht fälschlich als
///      Header geparst.
///
/// Fehlt beides, gilt der ganze Inhalt als Body und Meta bleibt leer.
pub fn parse_template(raw: &str) -> TemplateData {
    // BOM tolerieren — Windows-Editoren legen manchmal eine UTF-8-BOM
    // an den Dateianfang, sonst kollidiert die mit dem `---`-Match.
    let stripped = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let trimmed = stripped.trim_start_matches(['\r', '\n']);

    if !trimmed.starts_with("---") {
        // Friendly-Header-Format: `# Subject\n\nFrom: …\n…\n\nBody`
        // — exakt das, was der saveBody-Markdown-Workflow als Export
        // produziert. Greift nur wenn die erste nicht-leere Zeile
        // entweder ein H1-Heading oder ein Header-Key ist; sonst
        // durchreichen als reiner Body.
        if let Some(td) = try_parse_friendly_headers(stripped) {
            return td;
        }
        return TemplateData {
            meta: TemplateMeta::default(),
            body: stripped.to_string(),
        };
    }
    // Nach dem ersten `---` suchen wir den Schluss-Marker. Beide
    // müssen jeweils auf eigener Zeile stehen.
    let after_open = match trimmed.find('\n') {
        Some(idx) => &trimmed[idx + 1..],
        None => return TemplateData {
            meta: TemplateMeta::default(),
            body: stripped.to_string(),
        },
    };
    // End-Marker finden: Zeile, die exakt `---` ist (evtl. mit \r am Ende).
    let mut meta_text = String::new();
    let mut body_text = String::new();
    let mut found_end = false;
    for (line_idx, line) in after_open.split_inclusive('\n').enumerate() {
        let bare = line.trim_end_matches(['\r', '\n']);
        if bare == "---" {
            // Body ist alles nach dieser Zeile.
            // line_idx ist hier der Index in der split_inclusive-Liste —
            // wir brauchen den Byte-Offset. Pragmatisch: rebuilden via
            // erneutes Iterieren.
            let mut byte_off = 0usize;
            for (i2, l2) in after_open.split_inclusive('\n').enumerate() {
                if i2 == line_idx {
                    byte_off += l2.len();
                    break;
                }
                byte_off += l2.len();
            }
            body_text = after_open[byte_off..].to_string();
            found_end = true;
            break;
        }
        meta_text.push_str(line);
    }
    if !found_end {
        // Kein End-Marker → Frontmatter-Versuch verworfen, ganzes
        // Original als Body behandeln. Sicher gegen kaputte Templates.
        return TemplateData {
            meta: TemplateMeta::default(),
            body: stripped.to_string(),
        };
    }

    let meta = parse_meta_lines(&meta_text);
    TemplateData {
        meta,
        body: body_text,
    }
}

/// Extrahiert die nackte E-Mail-Adresse aus einer RFC-822-artigen
/// Mailbox-Form. Akzeptiert:
///   * `alice@firma.de`              → `alice@firma.de`
///   * `<alice@firma.de>`            → `alice@firma.de`
///   * `Alice <alice@firma.de>`      → `alice@firma.de`
///   * `"A. Bauer" <a.bauer@x.de>`   → `a.bauer@x.de`
///
/// Greift weder das Template noch das Account-Match scheitert,
/// wenn der User im `from:`-Frontmatter eine Display-Name-Variante
/// einträgt — sonst würde der ganze String („Alice <alice@…>")
/// gegen `account.address` ("alice@…") verglichen und nie matchen.
fn extract_email(s: &str) -> String {
    let s = s.trim();
    if let Some(open) = s.rfind('<') {
        if let Some(close) = s.rfind('>') {
            if close > open + 1 {
                let inner = s[open + 1..close].trim();
                if inner.contains('@') {
                    return inner.to_string();
                }
            }
        }
    }
    s.to_string()
}

fn parse_meta_lines(text: &str) -> TemplateMeta {
    let mut meta = TemplateMeta::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        match key.as_str() {
            "to" => meta.to = Some(val),
            "cc" => meta.cc = Some(val),
            "bcc" => meta.bcc = Some(val),
            "subject" => meta.subject = Some(val),
            "account" | "from" => meta.account = Some(val),
            _ => {}
        }
    }
    meta
}

/// Bekannte Header-Keys (case-insensitive). Wird in der RFC-822-
/// Erkennung benutzt, damit die erste Zeile unmissverständlich als
/// Header zählt — sonst würde ein Body-Anfang wie „Bestellnummer:
/// 123" fälschlich den ganzen Header-Block triggern.
fn is_known_header_key(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "to" | "cc" | "bcc" | "subject" | "from" | "account"
    )
}

/// Heuristik: Sieht der Key wie ein Header-Key aus? Bewusst lax —
/// erlaubt alle alphanumerischen Keys + `-`/`_`. Wird benutzt um zu
/// entscheiden, ob eine `Word: Value`-Zeile im Header-Block bleibt
/// oder den Body einleitet.
fn looks_like_header_key(k: &str) -> bool {
    let k = k.trim();
    !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Versucht den Datei-Anfang als „freundliches" Template-Header-
/// Format zu parsen. Drei Formen werden unterstützt:
///
///   1. **Markdown-Heading** als Subject: `# Mein Subject` direkt am
///      Anfang (mit oder ohne Leerzeile davor).
///   2. **Header-Block** im RFC-822-Stil: `From: …`, `To: …`,
///      `Subject: …` etc., abgeschlossen durch eine Leerzeile.
///   3. Beides zusammen — exakt das Format, das der saveBody-Markdown-
///      Workflow exportiert (`# {subject}\n\nFrom: …\nDate: …\n\n{body}`).
///      Das ist der Hauptgrund für diese Erweiterung: gespeicherte
///      Mails sollen sich symmetrisch als Templates wiederverwenden
///      lassen.
///
/// Unbekannte Header (z.B. `Date:` aus dem saveBody-Export) werden
/// silently konsumiert und nicht in `meta` gespeichert — der Body
/// soll keine fremden Headers mehr enthalten, aber wir wollen auch
/// nicht jedes Header-Feld inhaltlich verwenden (`Date:` würde sonst
/// das alte Mail-Datum statt heute reinziehen).
///
/// Liefert `None`, wenn nichts header-haftiges gefunden wurde — der
/// Caller fällt dann auf „alles ist Body" zurück. Dadurch sind
/// gewöhnliche Markdown-Bodies ohne Header sicher: nichts wird
/// ungewollt als Header-Block geschluckt.
fn try_parse_friendly_headers(raw: &str) -> Option<TemplateData> {
    enum State {
        Initial,
        Headers,
    }

    let mut state = State::Initial;
    let mut meta = TemplateMeta::default();
    let mut header_lines: Vec<String> = Vec::new();
    let mut consumed = 0usize;
    let mut subject_from_heading = false;

    for line in raw.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\r', '\n']);
        match state {
            State::Initial => {
                if bare.is_empty() {
                    // Führende Leerzeilen erlauben — auch zwischen
                    // `# Heading` und Header-Block.
                    consumed += line.len();
                    continue;
                }
                // Markdown-H1 als Subject. Form: `# Text` oder
                // `#  Text` (toleriert mehrfaches Whitespace).
                if let Some(rest) = bare.strip_prefix('#') {
                    let after = rest.trim_start();
                    // Nur als Subject-Heading werten, wenn nach dem
                    // `#` ein Space stand — sonst ist es vermutlich
                    // ein Markdown-Anker (`#Heading`-Style) oder
                    // schlicht ein `#`-Body-Inhalt.
                    if rest.starts_with(' ') || rest.starts_with('\t') {
                        if !subject_from_heading {
                            meta.subject = Some(after.trim().to_string());
                            subject_from_heading = true;
                        }
                        consumed += line.len();
                        continue;
                    }
                }
                // Trigger für Block-Modus: NUR bei *bekanntem* Key.
                // Wenn jemand Body mit `Bestellnummer: 123` anfängt,
                // soll das nicht als Header-Block geschluckt werden.
                if let Some((k, _v)) = bare.split_once(':') {
                    if is_known_header_key(k.trim()) {
                        header_lines.push(bare.to_string());
                        consumed += line.len();
                        state = State::Headers;
                        continue;
                    }
                }
                // Inhalt der weder Heading noch Header ist: Body
                // beginnt hier. Wenn wir bisher *gar nichts*
                // konsumiert haben (kein Subject, keine Header),
                // gibt's keinen Grund zu greifen — None zurück
                // damit der ganze Roh-Text als Body durchgeht.
                if !subject_from_heading && header_lines.is_empty() {
                    return None;
                }
                // Sonst: Subject hatten wir schon; der Body fängt
                // beim aktuellen Cursor an.
                let body = raw[consumed..].to_string();
                return Some(TemplateData { meta, body });
            }
            State::Headers => {
                if bare.is_empty() {
                    // Blank-Line-Separator: Body beginnt nach dieser
                    // Zeile.
                    consumed += line.len();
                    let parsed = parse_meta_lines(&header_lines.join("\n"));
                    merge_meta(&mut meta, parsed);
                    let body = raw[consumed..].to_string();
                    return Some(TemplateData { meta, body });
                }
                // Folding-Continuation (RFC 822 §3.4.8): WS-
                // präfixierte Zeile gehört zum vorigen Header.
                if line.starts_with(' ') || line.starts_with('\t') {
                    if let Some(last) = header_lines.last_mut() {
                        last.push(' ');
                        last.push_str(bare.trim_start());
                    }
                    consumed += line.len();
                    continue;
                }
                // Weitere Header-Zeile?
                if let Some((k, _v)) = bare.split_once(':') {
                    if looks_like_header_key(k) {
                        header_lines.push(bare.to_string());
                        consumed += line.len();
                        continue;
                    }
                }
                // Header-Block ohne Blank-Line abrupt zu Ende.
                // Konservativ: das, was bisher Header war, behalten,
                // ab hier ist Body.
                let parsed = parse_meta_lines(&header_lines.join("\n"));
                merge_meta(&mut meta, parsed);
                let body = raw[consumed..].to_string();
                return Some(TemplateData { meta, body });
            }
        }
    }

    // Datei zu Ende ohne dass wir einen Body gesehen haben —
    // wahrscheinlich ein Template das nur aus Subject + Headers
    // besteht. Body bleibt leer.
    if subject_from_heading || !header_lines.is_empty() {
        if !header_lines.is_empty() {
            let parsed = parse_meta_lines(&header_lines.join("\n"));
            merge_meta(&mut meta, parsed);
        }
        return Some(TemplateData {
            meta,
            body: String::new(),
        });
    }
    None
}

/// Übernimmt Felder aus `parsed` in `into`, wobei explizite Header
/// (z.B. `Subject:`) immer Vorrang gegenüber dem aus dem Markdown-
/// Heading abgeleiteten Subject haben — wenn jemand beides angibt,
/// gewinnt der explizite Header.
fn merge_meta(into: &mut TemplateMeta, parsed: TemplateMeta) {
    if parsed.to.is_some() {
        into.to = parsed.to;
    }
    if parsed.cc.is_some() {
        into.cc = parsed.cc;
    }
    if parsed.bcc.is_some() {
        into.bcc = parsed.bcc;
    }
    if parsed.subject.is_some() {
        into.subject = parsed.subject;
    }
    if parsed.account.is_some() {
        into.account = parsed.account;
    }
}

/// Bauen aus einem ImportRequest den finalen ComposeDraft-Payload.
/// Liest das Template, substituiert Variablen, sammelt Anhang-Metadaten.
pub fn build_prepared_draft(req: &ImportRequest) -> Result<PreparedImportDraft, String> {
    let raw = std::fs::read_to_string(&req.template_path)
        .map_err(|e| format!("Template lesen ({}): {e}", req.template_path.display()))?;
    let tpl = parse_template(&raw);

    // Diagnose-Log: User kann im Dev-Terminal sehen, was der Parser
    // aus dem Template gezogen hat. Spart Mut-mach-Schleifen wenn
    // `To:` oder `From:` nicht greifen wie erwartet.
    tracing::info!(
        target: "draft_import",
        template = %req.template_path.display(),
        meta_to = ?tpl.meta.to,
        meta_cc = ?tpl.meta.cc,
        meta_bcc = ?tpl.meta.bcc,
        meta_subject = ?tpl.meta.subject,
        meta_account = ?tpl.meta.account,
        param_keys = ?req.params.keys().collect::<Vec<_>>(),
        body_len = tpl.body.len(),
        "draft-import: template parsed"
    );

    let ctx = SubstCtx::new(req.params.clone());

    let to = tpl.meta.to.map(|s| ctx.substitute(&s)).unwrap_or_default();
    let cc = tpl.meta.cc.map(|s| ctx.substitute(&s)).unwrap_or_default();
    let bcc = tpl.meta.bcc.map(|s| ctx.substitute(&s)).unwrap_or_default();
    let subject = tpl
        .meta
        .subject
        .map(|s| ctx.substitute(&s))
        .unwrap_or_default();
    // Account-Email aus dem Frontmatter: erst Variablen substituieren,
    // dann mit `extract_email` die nackte Adresse aus einer evtl.
    // Display-Name-Form (`Alice <alice@…>`) ziehen — sonst matched
    // das Frontend gegen `account.address` nirgends.
    let account_email = tpl
        .meta
        .account
        .map(|s| ctx.substitute(&s))
        .map(|s| extract_email(&s))
        .filter(|s| !s.is_empty());
    let body = ctx.substitute(&tpl.body);

    let mut atts: Vec<PreparedAttachment> = Vec::with_capacity(req.attachments.len());
    for (idx, p) in req.attachments.iter().enumerate() {
        let meta = std::fs::metadata(p)
            .map_err(|e| format!("Anhang lesen ({}): {e}", p.display()))?;
        let size = meta.len();
        let filename = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "attachment.bin".to_string());
        let mime = guess_mime_simple(p);
        atts.push(PreparedAttachment {
            client_id: format!("imp-{}-{}", idx, uuid_short()),
            path: p.to_string_lossy().to_string(),
            filename,
            size_bytes: size,
            mime_type: Some(mime),
        });
    }

    Ok(PreparedImportDraft {
        account_email,
        to,
        cc,
        bcc,
        subject,
        body,
        attachments: atts,
        source_template: req.template_path.to_string_lossy().to_string(),
    })
}

fn uuid_short() -> String {
    // Kurzer Random-Tail, damit zwei Aufrufe in derselben Sekunde
    // keine kollidierenden clientIds erzeugen.
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

fn guess_mime_simple(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("html") | Some("htm") => "text/html",
        Some("zip") => "application/zip",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_basic() {
        let raw = "---\nto: a@b.de\nsubject: Hi\n---\nHallo\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.to.as_deref(), Some("a@b.de"));
        assert_eq!(t.meta.subject.as_deref(), Some("Hi"));
        assert_eq!(t.body, "Hallo\n");
    }

    #[test]
    fn no_frontmatter_passes_through() {
        let raw = "Plain body, no header.\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.to, None);
        assert_eq!(t.body, raw);
    }

    #[test]
    fn unterminated_frontmatter_falls_back_to_body() {
        let raw = "---\nto: a@b.de\nNeverCloses\n";
        let t = parse_template(raw);
        // Kein End-Marker → komplett als Body, Meta leer.
        assert_eq!(t.meta.to, None);
        assert!(t.body.starts_with("---"));
    }

    #[test]
    fn substitute_uses_params_first() {
        let mut p = HashMap::new();
        p.insert("year".to_string(), "2099".to_string());
        let ctx = SubstCtx::new(p);
        // Param überschreibt das Built-in `year`.
        assert_eq!(ctx.substitute("Jahr $year"), "Jahr 2099");
    }

    #[test]
    fn substitute_unknown_var_passes_through() {
        let ctx = SubstCtx::new(HashMap::new());
        assert_eq!(ctx.substitute("/bin:$PATH"), "/bin:$PATH");
    }

    #[test]
    fn parse_argv_full() {
        let argv = vec![
            "crystalmail.exe".to_string(),
            "--draft-from-template".to_string(),
            "C:/t.md".to_string(),
            "--param".to_string(),
            "k=v".to_string(),
            "--attach".to_string(),
            "C:/a.pdf".to_string(),
        ];
        let r = parse_argv(&argv).unwrap().expect("request");
        assert_eq!(r.template_path, PathBuf::from("C:/t.md"));
        assert_eq!(r.params.get("k"), Some(&"v".to_string()));
        assert_eq!(r.attachments.len(), 1);
    }

    #[test]
    fn parse_argv_no_trigger_returns_none() {
        let argv = vec!["crystalmail.exe".to_string()];
        let r = parse_argv(&argv).unwrap();
        assert!(r.is_none());
    }

    // ─── extract_email ─────────────────────────────────────────────

    #[test]
    fn extract_email_bare() {
        assert_eq!(extract_email("alice@firma.de"), "alice@firma.de");
    }

    #[test]
    fn extract_email_angle_brackets_only() {
        assert_eq!(extract_email("<alice@firma.de>"), "alice@firma.de");
    }

    #[test]
    fn extract_email_display_name() {
        assert_eq!(
            extract_email("Alice Bauer <alice@firma.de>"),
            "alice@firma.de"
        );
    }

    #[test]
    fn extract_email_quoted_display_name() {
        assert_eq!(
            extract_email("\"A. Bauer\" <a.bauer@x.de>"),
            "a.bauer@x.de"
        );
    }

    // ─── try_parse_friendly_headers ────────────────────────────────

    #[test]
    fn friendly_savebody_export_format() {
        // Genau das Format, das saveBody-MD produziert.
        let raw = "# Rechnung 2026-001 — Januar\n\nFrom: alice@firma.de\nDate: 2026-01-15T10:30:00+00:00\n\nHallo Kunde,\n\nanbei die Rechnung.\n";
        let t = parse_template(raw);
        assert_eq!(
            t.meta.subject.as_deref(),
            Some("Rechnung 2026-001 — Januar")
        );
        assert_eq!(t.meta.account.as_deref(), Some("alice@firma.de"));
        assert_eq!(t.body, "Hallo Kunde,\n\nanbei die Rechnung.\n");
        // Mail-Header dürfen NICHT mehr im Body stehen.
        assert!(!t.body.contains("From:"));
        assert!(!t.body.contains("Date:"));
    }

    #[test]
    fn friendly_explicit_subject_overrides_heading() {
        // Wenn beides drin ist, gewinnt explizites `Subject:`.
        let raw = "# Heading-Subject\n\nSubject: Header-Subject\n\nBody.\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.subject.as_deref(), Some("Header-Subject"));
    }

    #[test]
    fn friendly_headers_only_no_heading() {
        let raw = "From: alice@firma.de\nTo: kunde@example.de\nSubject: Hi\n\nBody hier.\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.account.as_deref(), Some("alice@firma.de"));
        assert_eq!(t.meta.to.as_deref(), Some("kunde@example.de"));
        assert_eq!(t.meta.subject.as_deref(), Some("Hi"));
        assert_eq!(t.body, "Body hier.\n");
    }

    #[test]
    fn friendly_pure_body_passes_through() {
        let raw = "Hallo Kunde,\n\nbloß ein Body, kein Header.\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.to, None);
        assert_eq!(t.meta.subject, None);
        assert_eq!(t.body, raw);
    }

    #[test]
    fn friendly_body_with_colon_intro_not_eaten() {
        // „Bestellnummer: 12345" als allerERSTE nicht-leere Zeile soll
        // nicht den Header-Modus triggern, weil sonst Bodies mit
        // Kolon-Anfang fälschlich konsumiert würden. Genau diese Form
        // hat aber `looks_like_header_key` — der erste Token ist
        // alphanumerisch. Deshalb: erst NACH dem ersten Trigger ist
        // der Block-Modus aktiv. Greift nur wenn entweder Heading
        // oder bekannter Header zuerst kam. Hier ist „Bestellnummer"
        // unbekannt, kein Trigger → ganzer Inhalt = Body.
        //
        // Akzeptiertes Trade-off: wenn jemand `Bestellnummer:` als
        // ersten Header verwendet, wird's dennoch ignoriert (Key
        // unbekannt → meta-Felder bleiben leer, Body fängt nach
        // Blank-Line an).
        let raw = "Bestellnummer: 12345\n\nKunde, hier deine Bestellung.\n";
        let t = parse_template(raw);
        // `Bestellnummer` ist kein known-key → kein Trigger,
        // alles bleibt Body.
        assert_eq!(t.body, raw);
        assert_eq!(t.meta.to, None);
    }

    #[test]
    fn friendly_yaml_frontmatter_still_works() {
        // Bestehender YAML-Pfad darf nicht regredieren.
        let raw = "---\nto: a@b.de\nsubject: Hi\n---\nHallo\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.to.as_deref(), Some("a@b.de"));
        assert_eq!(t.meta.subject.as_deref(), Some("Hi"));
        assert_eq!(t.body, "Hallo\n");
    }

    // ─── UTF-8-Sicherheit ──────────────────────────────────────────
    //
    // Reproduziert den User-Bug: Umlaute im Template-Body wurden
    // zerstört, weil das alte `substitute()` Bytes statt Chars
    // iterierte und Multi-Byte-UTF-8 in einzelne Latin-1-Zeichen
    // zerlegte (`ü` → `Ã¼`).

    #[test]
    fn substitute_preserves_utf8_in_template_text() {
        let ctx = SubstCtx::new(HashMap::new());
        let s = "Rechnung für April — über 1.250 €";
        // Kein `$var` drin — Output muss byte-genau gleich sein.
        assert_eq!(ctx.substitute(s), s);
    }

    #[test]
    fn substitute_preserves_utf8_around_var() {
        let mut p = HashMap::new();
        p.insert("name".into(), "Müller".into());
        let ctx = SubstCtx::new(p);
        // Sowohl die Umlaute im Var-Wert als auch die im
        // umgebenden Text müssen heil bleiben.
        assert_eq!(
            ctx.substitute("Hallo Frau $name, schön dass Sie da sind."),
            "Hallo Frau Müller, schön dass Sie da sind."
        );
    }

    #[test]
    fn substitute_unknown_var_keeps_name_with_utf8() {
        let ctx = SubstCtx::new(HashMap::new());
        // Unbekanntes `$xyz` muss als `$xyz` durchkommen, drumherum
        // muss UTF-8 erhalten bleiben.
        assert_eq!(
            ctx.substitute("Über $xyz nichts bekannt."),
            "Über $xyz nichts bekannt."
        );
    }

    #[test]
    fn build_prepared_draft_extracts_to_from_savebody_with_added_to() {
        // Genau der User-Reproducer: saveBody-MD-Export, von Hand
        // um eine `To:`-Zeile ergänzt, dann als Template benutzt.
        // `meta.to` muss gesetzt werden und nicht im Body landen.
        let raw = "# Rechnung 042\n\nTo: kunde@example.de\nFrom: alice@firma.de\nDate: 2026-01-15T10:30:00+00:00\n\nHallo,\nanbei.\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.to.as_deref(), Some("kunde@example.de"));
        assert_eq!(t.meta.account.as_deref(), Some("alice@firma.de"));
        assert_eq!(t.meta.subject.as_deref(), Some("Rechnung 042"));
        assert_eq!(t.body, "Hallo,\nanbei.\n");
    }

    #[test]
    fn build_prepared_draft_to_via_param_substitution() {
        let raw = "---\nto: $customer_email\nsubject: Hi\n---\nBody\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.to.as_deref(), Some("$customer_email"));
        // Substitution greift erst in build_prepared_draft, nicht
        // schon im Parser.
    }

    #[test]
    fn friendly_heading_only_no_headers() {
        let raw = "# Mein Subject\n\nDer Body kommt direkt nach dem Subject.\n";
        let t = parse_template(raw);
        assert_eq!(t.meta.subject.as_deref(), Some("Mein Subject"));
        assert_eq!(t.body, "Der Body kommt direkt nach dem Subject.\n");
    }

    #[test]
    fn parse_argv_param_value_with_equals() {
        let argv = vec![
            "crystalmail.exe".to_string(),
            "--draft-from-template".to_string(),
            "C:/t.md".to_string(),
            "--param".to_string(),
            "url=https://x.de/path?k=v".to_string(),
        ];
        let r = parse_argv(&argv).unwrap().expect("request");
        assert_eq!(
            r.params.get("url"),
            Some(&"https://x.de/path?k=v".to_string())
        );
    }
}
