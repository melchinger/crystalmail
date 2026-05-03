// Adressbuch-Import/Export.
//
//   * VCF (vCard 3.0): hand-gerollt. Spec ist trivial für unseren Subset
//     (FN, N, ORG, TITLE, TEL, EMAIL, ADR, URL, NOTE, CATEGORIES).
//     Externe vcard-Crates entweder zu groß oder mit harten Limits
//     beim Round-Trip — selber ist's 200 Zeilen, einfach prüfbar.
//   * CSV: über die `csv`-Crate. Header-basiertes Mapping, Google-
//     Contacts-kompatible Spalten als Default + ein paar Outlook-
//     Aliase (z.B. "First Name + Last Name" → Display Name).
//
// Round-Trip-Garantie: Export → Import liefert dieselben Stammdaten
// (sofern Provider die Felder erhält). Tags landen in CATEGORIES /
// "Tags"-Spalte und werden beim Import automatisch upsert't (neue
// Tags werden angelegt, bestehende verlinkt).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::contact::ContactDetail;

/// Stammdaten + Adressen + Tags für einen einzelnen Import-Datensatz.
/// Identisch zur internen Contact-Struktur, aber ohne `id` (das wird
/// beim DB-Insert vergeben) und mit Tags als String-Liste statt
/// TagIds (die werden ja erst nach dem upsert bekannt).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ContactImport {
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
    pub emails: Vec<String>,
    pub tags: Vec<String>,
}

// ─── VCF Export ──────────────────────────────────────────────────────

/// Schreibt eine Liste von Contacts als vCard-3.0-Strom. Mehrere
/// Karten sequenziell ohne Trenner — vCard-Parser auf der Empfänger-
/// seite erkennen das anhand der BEGIN:VCARD/END:VCARD-Klammern.
pub fn write_vcf(contacts: &[ContactDetail]) -> String {
    let mut out = String::new();
    for c in contacts {
        out.push_str(&contact_to_vcard(c));
    }
    out
}

fn contact_to_vcard(c: &ContactDetail) -> String {
    // ContactDetail flattens den Contact via serde, im Rust-Code
    // bleibt's geschachtelt → Alias auf den inneren Stamm-Block für
    // lesbares Schreiben.
    let core = &c.contact;
    let mut s = String::new();
    s.push_str("BEGIN:VCARD\r\n");
    s.push_str("VERSION:3.0\r\n");
    // FN (display name) ist Pflichtfeld in 3.0.
    s.push_str(&format!("FN:{}\r\n", escape_vcf(&core.display_name)));
    // N: strukturierter Name. Wir haben nur FN — N als ;-separierte
    // Leer-Felder schreiben (Family;Given;Additional;Prefix;Suffix).
    // Manche Reader (Apple Kontakte) verlangen N auf 3.0.
    s.push_str(&format!("N:{};;;;\r\n", escape_vcf(&core.display_name)));
    if let Some(v) = nonempty(&core.organization) {
        s.push_str(&format!("ORG:{}\r\n", escape_vcf(&v)));
    }
    if let Some(v) = nonempty(&core.job_title) {
        s.push_str(&format!("TITLE:{}\r\n", escape_vcf(&v)));
    }
    if let Some(v) = nonempty(&core.phone) {
        s.push_str(&format!("TEL;TYPE=WORK,VOICE:{}\r\n", escape_vcf(&v)));
    }
    if let Some(v) = nonempty(&core.mobile) {
        s.push_str(&format!("TEL;TYPE=CELL,VOICE:{}\r\n", escape_vcf(&v)));
    }
    if core.street.is_some()
        || core.zip.is_some()
        || core.city.is_some()
        || core.country.is_some()
    {
        // ADR: PO-Box;Extended;Street;City;Region;Postal;Country
        let adr = format!(
            ";;{};{};;{};{}",
            escape_vcf(core.street.as_deref().unwrap_or("")),
            escape_vcf(core.city.as_deref().unwrap_or("")),
            escape_vcf(core.zip.as_deref().unwrap_or("")),
            escape_vcf(core.country.as_deref().unwrap_or("")),
        );
        s.push_str(&format!("ADR;TYPE=WORK:{}\r\n", adr));
    }
    if let Some(v) = nonempty(&core.website) {
        s.push_str(&format!("URL:{}\r\n", escape_vcf(&v)));
    }
    for (i, e) in c.emails.iter().enumerate() {
        let typ = if i == 0 || e.is_primary {
            "PREF,INTERNET"
        } else {
            "INTERNET"
        };
        s.push_str(&format!("EMAIL;TYPE={}:{}\r\n", typ, escape_vcf(&e.email)));
    }
    if !core.notes.is_empty() {
        s.push_str(&format!("NOTE:{}\r\n", escape_vcf(&core.notes)));
    }
    if !c.tags.is_empty() {
        let csv = c
            .tags
            .iter()
            .map(|t| escape_vcf(&t.name).replace(',', "\\,"))
            .collect::<Vec<_>>()
            .join(",");
        s.push_str(&format!("CATEGORIES:{}\r\n", csv));
    }
    s.push_str("END:VCARD\r\n");
    s
}

/// Helper: Sonderzeichen in vCard-Werten escapen. Spec: `\`, `;`, `,`,
/// Newline. Wir lassen Komma roh weil's in den meisten Feldern (FN,
/// ORG, NOTE) erlaubt ist; nur in CATEGORIES kontextuell escapt.
fn escape_vcf(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace('\n', "\\n")
        .replace('\r', "")
}

fn nonempty<S: AsRef<str>>(opt: &Option<S>) -> Option<String> {
    opt.as_ref()
        .map(|s| s.as_ref().trim().to_string())
        .filter(|s| !s.is_empty())
}

// ─── VCF Import ──────────────────────────────────────────────────────

/// Parsed eine VCF-Datei in eine Liste von ContactImport-Records.
/// Handhabt mehrere VCARDs sequenziell. Ungültige Karten werden
/// übersprungen, nicht hart abgebrochen — Robustheit vor
/// Pedanterie.
pub fn parse_vcf(input: &str) -> Vec<ContactImport> {
    // Line-folding undo: Zeilen die mit Space oder Tab beginnen,
    // sind Fortsetzungen der vorherigen (vCard-Spec § 5.2).
    let unfolded = unfold_vcard(input);
    let mut results = Vec::new();
    let mut current: Option<ContactImport> = None;

    for line in unfolded.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("BEGIN:VCARD") {
            current = Some(ContactImport::default());
            continue;
        }
        if line.eq_ignore_ascii_case("END:VCARD") {
            if let Some(card) = current.take() {
                if !card.display_name.trim().is_empty() {
                    results.push(card);
                }
            }
            continue;
        }
        let Some(card) = current.as_mut() else { continue };
        // Property-Format: "PROP[;PARAMS]:VALUE"
        let Some((spec, raw_value)) = line.split_once(':') else { continue };
        // Globales Unescape für 95% der Felder. CATEGORIES kriegt den
        // raw_value damit es den `\,`-Trenner-Schutz selber lösen kann
        // — sonst sieht parse_categories nur unescaped Kommas und
        // kann den Original-Trenner nicht von einem escaped-Komma
        // unterscheiden.
        let value = unescape_vcf(raw_value);
        // PROP normalisieren: alles vor dem ersten ';' ist der Name.
        let (prop, params) = match spec.split_once(';') {
            Some((p, params)) => (p, params),
            None => (spec, ""),
        };
        let prop_upper = prop.to_uppercase();
        match prop_upper.as_str() {
            "FN" => {
                card.display_name = value;
            }
            "N" if card.display_name.is_empty() => {
                // Fallback wenn FN fehlt: Family;Given zusammenbauen.
                let parts: Vec<&str> = value.splitn(5, ';').collect();
                let family = parts.first().copied().unwrap_or("").trim();
                let given = parts.get(1).copied().unwrap_or("").trim();
                card.display_name = match (given.is_empty(), family.is_empty()) {
                    (true, true) => String::new(),
                    (true, false) => family.to_string(),
                    (false, true) => given.to_string(),
                    (false, false) => format!("{given} {family}"),
                };
            }
            "ORG" => {
                // ORG kann ";Department"-Suffix haben — nur ersten Teil nehmen.
                let main = value.split(';').next().unwrap_or("").trim().to_string();
                if !main.is_empty() {
                    card.organization = Some(main);
                }
            }
            "TITLE" => {
                card.job_title = nonempty_str(&value);
            }
            "TEL" => {
                let is_cell = params.to_uppercase().contains("CELL");
                if is_cell {
                    card.mobile = nonempty_str(&value);
                } else {
                    // Erstes nicht-CELL-TEL gewinnt. Wenn schon eines da
                    // ist, ignorieren — weniger Felder zerschießen.
                    if card.phone.is_none() {
                        card.phone = nonempty_str(&value);
                    }
                }
            }
            "ADR" => {
                // Format: PO;Ext;Street;City;Region;PostalCode;Country
                let parts: Vec<&str> = value.splitn(7, ';').collect();
                card.street = nonempty_str(parts.get(2).copied().unwrap_or(""));
                card.city = nonempty_str(parts.get(3).copied().unwrap_or(""));
                card.zip = nonempty_str(parts.get(5).copied().unwrap_or(""));
                card.country = nonempty_str(parts.get(6).copied().unwrap_or(""));
            }
            "URL" => {
                card.website = nonempty_str(&value);
            }
            "EMAIL" => {
                let v = value.trim().to_string();
                if !v.is_empty() && v.contains('@') {
                    card.emails.push(v);
                }
            }
            "NOTE" => {
                card.notes = value;
            }
            "CATEGORIES" => {
                // Komma-separierte Tag-Liste, mit `\,` als escape.
                // Raw-Value verwenden damit der Trenner-Schutz noch
                // sichtbar ist; parse_categories unescaped pro Token.
                card.tags.extend(parse_categories(raw_value));
            }
            _ => {}
        }
    }
    // Letzte Karte auch noch bedienen falls END:VCARD fehlt.
    if let Some(card) = current.take() {
        if !card.display_name.trim().is_empty() {
            results.push(card);
        }
    }
    results
}

/// Line-folding rückgängig machen: jede Zeile die mit ' ' oder '\t'
/// startet hängt an die vorherige an (ohne den Whitespace).
fn unfold_vcard(input: &str) -> String {
    let mut out = String::new();
    for line in input.lines() {
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            out.push_str(&line[1..]);
        } else {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
        }
    }
    out
}

fn unescape_vcf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&'n') | Some(&'N') => {
                    out.push('\n');
                    chars.next();
                }
                Some(&';') => {
                    out.push(';');
                    chars.next();
                }
                Some(&',') => {
                    out.push(',');
                    chars.next();
                }
                Some(&'\\') => {
                    out.push('\\');
                    chars.next();
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn nonempty_str(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn parse_categories(s: &str) -> Vec<String> {
    // Split bei Komma, aber `\,` als Escape respektieren. Eingabe ist
    // der RAW-Value (vor dem globalen Unescape) — wir lösen `\,` zum
    // wörtlichen Komma direkt im Token, der Rest (`\\`, `\n`, `\;`)
    // wird per Token nachträglich unescaped.
    let mut tokens: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&',') {
            buf.push('\\');
            buf.push(',');
            chars.next();
        } else if c == ',' {
            tokens.push(std::mem::take(&mut buf));
        } else {
            buf.push(c);
        }
    }
    tokens.push(buf);
    // Pro Token full-unescape (löst dann auch das `\,` zu `,`).
    tokens
        .into_iter()
        .map(|t| unescape_vcf(t.trim()).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

// ─── CSV Export ──────────────────────────────────────────────────────

/// CSV-Writer mit Google-Contacts-kompatiblen Headern. Mehrere E-Mails
/// landen in `Email 1`, `Email 2`, …; max 4 Spalten — drüber wird
/// getrunkt (extrem selten und Google selber ehrlich gesagt hört bei
/// 3 auf).
pub fn write_csv(contacts: &[ContactDetail]) -> Result<String, String> {
    let max_emails = contacts.iter().map(|c| c.emails.len()).max().unwrap_or(0).min(4);
    let mut wtr = csv::WriterBuilder::new()
        .has_headers(true)
        .from_writer(Vec::new());

    let mut headers = vec![
        "Display Name",
        "Organization",
        "Job Title",
        "Phone",
        "Mobile",
        "Street",
        "ZIP",
        "City",
        "Country",
        "Website",
        "Notes",
        "Tags",
    ];
    let email_headers: Vec<String> =
        (1..=max_emails).map(|i| format!("Email {i}")).collect();
    for h in &email_headers {
        headers.push(h.as_str());
    }
    wtr.write_record(&headers).map_err(|e| e.to_string())?;

    for c in contacts {
        let core = &c.contact;
        let mut row: Vec<String> = vec![
            core.display_name.clone(),
            core.organization.clone().unwrap_or_default(),
            core.job_title.clone().unwrap_or_default(),
            core.phone.clone().unwrap_or_default(),
            core.mobile.clone().unwrap_or_default(),
            core.street.clone().unwrap_or_default(),
            core.zip.clone().unwrap_or_default(),
            core.city.clone().unwrap_or_default(),
            core.country.clone().unwrap_or_default(),
            core.website.clone().unwrap_or_default(),
            core.notes.clone(),
            c.tags
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ];
        for i in 0..max_emails {
            row.push(c.emails.get(i).map(|e| e.email.clone()).unwrap_or_default());
        }
        wtr.write_record(&row).map_err(|e| e.to_string())?;
    }
    wtr.flush().map_err(|e| e.to_string())?;
    let bytes = wtr.into_inner().map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

// ─── CSV Import ──────────────────────────────────────────────────────

/// Liste von Header-Aliasen → kanonischer Feld-Key. Tolerant zu
/// Outlook-/Google-Variationen.
fn header_alias(header: &str) -> Option<&'static str> {
    let h = header.trim().to_lowercase();
    let h = h.replace('_', " ").replace('-', " ");
    match h.as_str() {
        // Display Name
        "display name" | "name" | "full name" | "anzeigename" => Some("name"),
        "first name" | "given name" | "vorname" => Some("first"),
        "last name" | "family name" | "surname" | "nachname" => Some("last"),
        // Organization
        "organization" | "company" | "company name" | "firma" | "organisation" => {
            Some("org")
        }
        // Job title
        "job title" | "title" | "position" | "rolle" => Some("title"),
        // Phones
        "phone" | "phone 1 - value" | "telefon" | "tel" | "primary phone"
        | "business phone" | "work phone" => Some("phone"),
        "mobile" | "cell" | "cell phone" | "mobile phone" | "handy" => Some("mobile"),
        // Address
        "street" | "address" | "address 1" | "straße" | "strasse" => Some("street"),
        "zip" | "postal code" | "post code" | "postleitzahl" | "plz" => Some("zip"),
        "city" | "town" | "ort" | "stadt" => Some("city"),
        "country" | "land" => Some("country"),
        "website" | "homepage" | "url" | "web" => Some("website"),
        "notes" | "note" | "comment" | "comments" | "notizen" => Some("notes"),
        "tags" | "categories" | "labels" | "kategorien" => Some("tags"),
        _ => {
            // Email N → mappt auf "email"
            if h.starts_with("email")
                || h.starts_with("e-mail")
                || h.starts_with("e mail")
                || h == "mail"
            {
                Some("email")
            } else {
                None
            }
        }
    }
}

pub fn parse_csv(input: &str) -> Result<Vec<ContactImport>, String> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(input.as_bytes());

    let headers = rdr.headers().map_err(|e| e.to_string())?.clone();
    let header_keys: Vec<Option<&'static str>> =
        headers.iter().map(header_alias).collect();

    let mut out = Vec::new();
    for record in rdr.records() {
        let record = record.map_err(|e| e.to_string())?;
        let mut imp = ContactImport::default();
        // Zwischenpuffer für first/last falls kein "name"-Header da ist.
        let mut first = String::new();
        let mut last = String::new();
        for (i, key) in header_keys.iter().enumerate() {
            let Some(key) = key else { continue };
            let Some(val) = record.get(i) else { continue };
            let val = val.trim();
            if val.is_empty() {
                continue;
            }
            match *key {
                "name" => imp.display_name = val.to_string(),
                "first" => first = val.to_string(),
                "last" => last = val.to_string(),
                "org" => imp.organization = Some(val.to_string()),
                "title" => imp.job_title = Some(val.to_string()),
                "phone" => {
                    if imp.phone.is_none() {
                        imp.phone = Some(val.to_string());
                    }
                }
                "mobile" => imp.mobile = Some(val.to_string()),
                "street" => imp.street = Some(val.to_string()),
                "zip" => imp.zip = Some(val.to_string()),
                "city" => imp.city = Some(val.to_string()),
                "country" => imp.country = Some(val.to_string()),
                "website" => imp.website = Some(val.to_string()),
                "notes" => imp.notes = val.to_string(),
                "tags" => {
                    imp.tags
                        .extend(val.split(',').map(|t| t.trim().to_string()).filter(|s| !s.is_empty()));
                }
                "email" => {
                    if val.contains('@') {
                        imp.emails.push(val.to_string());
                    }
                }
                _ => {}
            }
        }
        // Falls kein expliziter Display Name gesetzt ist, aus first+last bauen.
        if imp.display_name.trim().is_empty() {
            imp.display_name = match (first.is_empty(), last.is_empty()) {
                (true, true) => continue, // Skip — keine Identität, kein Datensatz.
                (true, false) => last,
                (false, true) => first,
                (false, false) => format!("{first} {last}"),
            };
        }
        out.push(imp);
    }
    Ok(out)
}

// ─── Import-Apply-Helper ─────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ImportReport {
    pub created: i64,
    pub skipped_existing_email: i64,
    pub tags_created: i64,
    pub skipped_invalid: i64,
    /// E-Mail-Adressen die bereits einem anderen Contact gehörten und
    /// daher nicht angelegt werden konnten — Hint fürs UI welche
    /// Datensätze geskippt wurden.
    pub skipped_addresses: Vec<String>,
}

/// Aggregiert Tags eindeutig (case-insensitive) für den DB-Upsert-Pfad
/// damit ein Massen-Import nicht 100x denselben Tag upsert't.
pub fn distinct_tag_names(records: &[ContactImport]) -> Vec<String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for r in records {
        for t in &r.tags {
            let key = t.to_lowercase();
            map.entry(key).or_insert_with(|| t.clone());
        }
    }
    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_import() -> ContactImport {
        ContactImport {
            display_name: "Anna Schmidt".into(),
            organization: Some("Acme GmbH".into()),
            job_title: Some("CTO".into()),
            phone: Some("+49 711 123".into()),
            mobile: Some("+49 170 999".into()),
            street: Some("Hauptstr. 1".into()),
            zip: Some("70173".into()),
            city: Some("Stuttgart".into()),
            country: Some("DE".into()),
            website: Some("https://acme.example".into()),
            notes: "Wichtig.\nMit Mehrzeiler.".into(),
            emails: vec!["anna@acme.example".into(), "private@a.de".into()],
            tags: vec!["Kunde".into(), "VIP".into()],
        }
    }

    #[test]
    fn vcf_export_import_roundtrip() {
        // Kein DB-Access nötig: wir bauen einen ContactDetail-Mock,
        // exportieren, parsen, vergleichen.
        let import = fresh_import();
        // ContactDetail-Mock — bei import-Tests genügt write_vcf ein
        // vereinfachter Pfad. Da der Writer ein ContactDetail erwartet,
        // bauen wir kurz einen.
        use crate::domain::contact::{
            Contact, ContactDetail, ContactEmail, ContactId, ContactOrigin, Tag, TagId,
        };
        use chrono::Utc;
        use uuid::Uuid;
        let now = Utc::now();
        let detail = ContactDetail {
            contact: Contact {
                id: ContactId(Uuid::new_v4()),
                display_name: import.display_name.clone(),
                organization: import.organization.clone(),
                job_title: import.job_title.clone(),
                phone: import.phone.clone(),
                mobile: import.mobile.clone(),
                street: import.street.clone(),
                zip: import.zip.clone(),
                city: import.city.clone(),
                country: import.country.clone(),
                website: import.website.clone(),
                notes: import.notes.clone(),
                origin: ContactOrigin::User,
                pinned: false,
                last_extracted_envelope_id: None,
                created_at: now,
                updated_at: now,
            },
            emails: import
                .emails
                .iter()
                .enumerate()
                .map(|(i, e)| ContactEmail {
                    id: i as i64,
                    contact_id: ContactId(Uuid::new_v4()),
                    email: e.clone(),
                    is_primary: i == 0,
                })
                .collect(),
            tags: import
                .tags
                .iter()
                .map(|t| Tag {
                    id: TagId(Uuid::new_v4()),
                    name: t.clone(),
                    color: None,
                    created_at: now,
                })
                .collect(),
            message_count: 0,
            last_message_at: None,
        };
        let vcf = write_vcf(&[detail.clone()]);
        let parsed = parse_vcf(&vcf);
        assert_eq!(parsed.len(), 1);
        let p = &parsed[0];
        assert_eq!(p.display_name, "Anna Schmidt");
        assert_eq!(p.organization.as_deref(), Some("Acme GmbH"));
        assert_eq!(p.job_title.as_deref(), Some("CTO"));
        assert_eq!(p.phone.as_deref(), Some("+49 711 123"));
        assert_eq!(p.mobile.as_deref(), Some("+49 170 999"));
        assert_eq!(p.street.as_deref(), Some("Hauptstr. 1"));
        assert_eq!(p.zip.as_deref(), Some("70173"));
        assert_eq!(p.city.as_deref(), Some("Stuttgart"));
        assert_eq!(p.country.as_deref(), Some("DE"));
        assert_eq!(p.website.as_deref(), Some("https://acme.example"));
        assert_eq!(p.emails.len(), 2);
        assert_eq!(p.emails[0], "anna@acme.example");
        assert_eq!(p.tags, vec!["Kunde", "VIP"]);
        // Mehrzeiler-Notes sollten geescaped + decodet werden.
        assert!(p.notes.contains("Mehrzeiler"));
    }

    #[test]
    fn csv_parse_handles_google_export_format() {
        let raw = "Name,Organization,Email 1,Phone\n\
                   Bob Tester,Acme,bob@acme.example,+49 1234\n\
                   Carol,,carol@x.de,\n";
        let parsed = parse_csv(raw).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].display_name, "Bob Tester");
        assert_eq!(parsed[0].organization.as_deref(), Some("Acme"));
        assert_eq!(parsed[0].emails, vec!["bob@acme.example"]);
        assert_eq!(parsed[0].phone.as_deref(), Some("+49 1234"));
        assert_eq!(parsed[1].display_name, "Carol");
        assert!(parsed[1].organization.is_none());
    }

    #[test]
    fn csv_parse_first_last_compose_to_name() {
        let raw = "First Name,Last Name,Email\n\
                   Alice,Mueller,alice@a.de\n";
        let parsed = parse_csv(raw).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].display_name, "Alice Mueller");
    }

    #[test]
    fn csv_parse_tags_split() {
        // Mehrere Tags MÜSSEN per CSV-Quotierung in eine Zelle —
        // sonst sind die Kommas Feld-Trenner und nicht Tag-Trenner.
        // Genau das ist die Format-Regel von Google Contacts /
        // Outlook beim Export.
        let raw = "Name,Tags,Email\n\
                   D,\"Kunde, VIP\",d@x.de\n";
        let parsed = parse_csv(raw).unwrap();
        assert_eq!(parsed[0].tags, vec!["Kunde", "VIP"]);
    }

    #[test]
    fn vcf_parse_categories_with_escaped_comma() {
        let raw = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nCATEGORIES:A\\,B,C\r\nEND:VCARD\r\n";
        let parsed = parse_vcf(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].tags, vec!["A,B", "C"]);
    }

    #[test]
    fn vcf_parse_skips_card_without_fn() {
        let raw = "BEGIN:VCARD\r\nVERSION:3.0\r\nORG:Acme\r\nEND:VCARD\r\n";
        let parsed = parse_vcf(raw);
        assert!(parsed.is_empty(), "Karten ohne FN/N-Fallback müssen geskippt werden");
    }

    #[test]
    fn distinct_tags_dedup_case_insensitive() {
        let recs = vec![
            ContactImport {
                display_name: "A".into(),
                tags: vec!["Kunde".into(), "VIP".into()],
                ..Default::default()
            },
            ContactImport {
                display_name: "B".into(),
                tags: vec!["kunde".into(), "Lieferant".into()],
                ..Default::default()
            },
        ];
        let mut tags = distinct_tag_names(&recs);
        tags.sort();
        assert_eq!(tags, vec!["Kunde", "Lieferant", "VIP"]);
    }
}
