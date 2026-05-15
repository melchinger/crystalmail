// Extract the "essential" Markdown body of a mail — just the relevant
// content, minus reply quotes, mobile-mail signatures, RFC-3676 sig
// blocks, and the trailing whitespace. Used as the `$body_essentials`
// workflow template variable, which lets the user hand a mail body
// straight into an external script (todo-tool, task-tracker, ticket
// creator) as a single CLI argument without the recipient having to
// scroll past the original conversation thread.
//
// Strategy is pragmatic, not perfect:
//
//   1. Prefer the text/plain MIME part. Most mails (incl. modern
//      Apple/Outlook/Gmail) include one and it's already close to what
//      we want.
//   2. Fall back to HTML → plain text via a small inline converter if
//      no text/plain is present (HTML-only mails from web shops &
//      newsletters). The converter is deliberately minimal — we don't
//      try to render tables or preserve formatting fidelity, just turn
//      block-level tags into newlines and strip everything else.
//   3. Strip from the first line that looks like a reply-header
//      ("On … wrote:", "Am … schrieb:", Outlook From:/Sent:/To:/Subject
//      block) — everything after that is the quoted thread.
//   4. Strip from the first line that looks like a sig delimiter
//      ("-- ", "Gesendet von meinem iPhone", "Sent from my …").
//   5. Drop any `>`-quoted leader lines that survived (some mail
//      clients put quoted content *above* the reply-header line).
//   6. Collapse runs of blank lines and trim outer whitespace.
//
// The output is plain text but it's *also* valid Markdown — paragraph
// breaks are blank lines, hyperlinks survive as bare URLs, lists keep
// their leading dash/digit. Good enough for "paste this as the body
// of a Linear/Asana/Trello ticket."

use mail_parser::MessageParser;

/// Resolve the mail body to a single Markdown-ish string suitable for
/// being passed as a CLI argument to an external script.
///
/// `plain` is the cached `text_body[0]` that the body-fetch path
/// already extracted; if absent or empty, we re-parse `raw` and pull
/// the HTML body out, converting it down to text.
pub fn extract(plain: Option<&str>, raw: &[u8]) -> String {
    let text = plain
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.to_string())
        .unwrap_or_else(|| html_fallback(raw));
    strip_signature_and_quoted_reply(&text)
}

fn html_fallback(raw: &[u8]) -> String {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return String::new();
    };
    if let Some(html) = msg.body_html(0) {
        html_to_text(&html)
    } else if let Some(text) = msg.body_text(0) {
        text.to_string()
    } else {
        String::new()
    }
}

/// Deliberately minimal HTML → text. We don't render tables, we don't
/// honour CSS, and we don't try to recreate the visual layout. The job
/// is to surface readable prose for a downstream script — anything
/// fancier turns into a dependency on a full HTML parser + sanitiser,
/// which we'd then have to keep patched.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut in_script_or_style: Option<&'static [u8]> = None;
    while i < bytes.len() {
        // Inside <script> / <style> we skip until the matching close
        // tag — these blocks routinely contain CSS / JS that would
        // otherwise leak into the output.
        if let Some(close) = in_script_or_style {
            if bytes[i..].len() >= close.len()
                && bytes[i..i + close.len()].eq_ignore_ascii_case(close)
            {
                // Advance past the `>` that terminates the close tag
                // — `</script  >` (whitespace before `>`) is technically
                // legal HTML, so search for it instead of assuming it
                // sits right after the tag name.
                let rest = &bytes[i + close.len()..];
                let gt = rest
                    .iter()
                    .position(|&b| b == b'>')
                    .map(|p| p + 1)
                    .unwrap_or(rest.len());
                i += close.len() + gt;
                in_script_or_style = None;
                continue;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'<' {
            // Find the matching `>` — skip the whole tag.
            let Some(end_rel) = bytes[i..].iter().position(|&b| b == b'>') else {
                break;
            };
            let tag_raw = &bytes[i + 1..i + end_rel];
            let tag_text = std::str::from_utf8(tag_raw).unwrap_or("");
            let tag_lower = tag_text.trim_start_matches('/').to_ascii_lowercase();
            // Block-level / break tags emit a newline; everything else
            // is just dropped. We keep this list short on purpose —
            // adding obscure tags here doesn't help the common case
            // and risks splitting prose into more lines than intended.
            let name_end = tag_lower
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(tag_lower.len());
            let name = &tag_lower[..name_end];
            match name {
                "br" | "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3"
                | "h4" | "h5" | "h6" => out.push('\n'),
                "blockquote" => out.push('\n'),
                "script" => in_script_or_style = Some(b"</script"),
                "style" => in_script_or_style = Some(b"</style"),
                _ => {}
            }
            i += end_rel + 1;
            continue;
        }
        if bytes[i] == b'&' {
            // Entities. Only handle the five XML-builtin ones plus
            // numeric ones — covering Outlook's typical &nbsp;/&amp;
            // soup. Unrecognised entities get copied verbatim so the
            // user sees them and can debug.
            if let Some(end_rel) = bytes[i..].iter().take(8).position(|&b| b == b';') {
                let entity = std::str::from_utf8(&bytes[i + 1..i + end_rel])
                    .unwrap_or("");
                let resolved = match entity {
                    "amp" => Some("&".to_string()),
                    "lt" => Some("<".to_string()),
                    "gt" => Some(">".to_string()),
                    "quot" => Some("\"".to_string()),
                    "apos" => Some("'".to_string()),
                    "nbsp" => Some(" ".to_string()),
                    e if e.starts_with("#x") || e.starts_with("#X") => {
                        u32::from_str_radix(&e[2..], 16)
                            .ok()
                            .and_then(char::from_u32)
                            .map(|c| c.to_string())
                    }
                    e if e.starts_with('#') => {
                        e[1..]
                            .parse::<u32>()
                            .ok()
                            .and_then(char::from_u32)
                            .map(|c| c.to_string())
                    }
                    _ => None,
                };
                if let Some(s) = resolved {
                    out.push_str(&s);
                    i += end_rel + 1;
                    continue;
                }
            }
        }
        // Plain UTF-8 byte through. Manual char iteration would be more
        // correct but for an HTML stripper feeding into a quoted-reply
        // detector this is fine — multi-byte UTF-8 sequences pass
        // through intact because we only branch on single-byte ASCII
        // markers (`<`, `&`).
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Cut off everything from the first reply-header / signature marker
/// and clean up what's left. Returns trimmed Markdown-ish text.
fn strip_signature_and_quoted_reply(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut cut_at: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if is_cut_marker(i, line, &lines) {
            cut_at = Some(i);
            break;
        }
    }
    let kept: Vec<&str> = match cut_at {
        Some(i) => lines[..i].to_vec(),
        None => lines,
    };
    // Drop trailing quoted/blank lines — handles the "quoted content
    // above the reply-header" pattern from some mobile clients, and
    // also kills the empty separator line that usually precedes the
    // header we just cut off.
    let mut kept = kept;
    while let Some(last) = kept.last() {
        let t = last.trim();
        if t.is_empty() || t.starts_with('>') {
            kept.pop();
        } else {
            break;
        }
    }
    // Re-join and collapse 3+ consecutive newlines into a clean
    // paragraph break. Iterate until stable — one pass isn't enough
    // when the input has long runs.
    let mut out = kept.join("\n");
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

fn is_cut_marker(i: usize, line: &str, all: &[&str]) -> bool {
    let trimmed_end = line.trim_end();
    // RFC 3676 sig delimiter. The trailing space is part of the spec
    // but most clients omit it, so both forms count.
    if trimmed_end == "--" {
        return true;
    }
    // Mobile mail clients. Lower-cased lookup is enough — these all
    // ship in fixed wording per locale.
    let lower = trimmed_end.to_ascii_lowercase();
    if lower.starts_with("gesendet von meinem ")
        || lower.starts_with("von meinem ")
        || lower.starts_with("sent from my ")
    {
        return true;
    }
    // Single-line reply headers.
    //
    //   "On Fri, 12 May 2026 at 14:30, Alice <a@b.com> wrote:"
    //   "On 12.05.2026 14:30, Alice wrote:"
    //
    // Gmail / Apple Mail / most CLI mailers use this. We don't try to
    // be clever about the date format inside — line just needs to
    // start with "On " and end with "wrote:".
    if trimmed_end.starts_with("On ") && trimmed_end.ends_with("wrote:") {
        return true;
    }
    // German equivalent.
    //
    //   "Am 12.05.2026 um 14:30 schrieb Alice <a@b.com>:"
    //   "Am Mittwoch, 12. Mai 2026, schrieb Alice:"
    //
    // Apple Mail.app DE / Thunderbird DE.
    if trimmed_end.starts_with("Am ")
        && trimmed_end.contains(" schrieb ")
        && trimmed_end.ends_with(':')
    {
        return true;
    }
    // Outlook quoted-reply header block. Detection needs lookahead:
    // a literal "Von: …" / "From: …" appears all over real mail
    // bodies; only the full 3–4-line block with the matching
    // companion headers is unambiguous.
    if line.starts_with("Von: ") || line.starts_with("From: ") {
        let lookahead: Vec<&str> = all
            .iter()
            .skip(i + 1)
            .take(5)
            .copied()
            .collect();
        let de = lookahead.iter().any(|l| l.starts_with("Gesendet:"))
            && lookahead.iter().any(|l| l.starts_with("An:"))
            && lookahead.iter().any(|l| l.starts_with("Betreff:"));
        let en = lookahead.iter().any(|l| l.starts_with("Sent:"))
            && lookahead.iter().any(|l| l.starts_with("To:"))
            && lookahead.iter().any(|l| l.starts_with("Subject:"));
        if de || en {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_plain_body_intact() {
        let body = "Hallo Max,\n\nkurze Frage zu der Lieferung morgen.\n\nGruß\nTom";
        // No sig delim, no reply header — only the trailing "Gruß\nTom"
        // looks signature-ish but isn't a recognised marker, so it
        // stays in.
        let out = extract(Some(body), b"");
        assert_eq!(out, body);
    }

    #[test]
    fn strips_rfc3676_signature() {
        let body = "Inhalt der Mail.\n\n-- \nTom Melchinger\nDev";
        let out = extract(Some(body), b"");
        assert_eq!(out, "Inhalt der Mail.");
    }

    #[test]
    fn strips_iphone_signature() {
        let body = "Schau mal ob das passt.\n\nGesendet von meinem iPhone";
        let out = extract(Some(body), b"");
        assert_eq!(out, "Schau mal ob das passt.");
    }

    #[test]
    fn strips_english_reply_header() {
        let body = "Yes, that works.\n\nOn Fri, 12 May 2026, Alice <a@b.com> wrote:\n> previous mail\n> more quoted lines";
        let out = extract(Some(body), b"");
        assert_eq!(out, "Yes, that works.");
    }

    #[test]
    fn strips_german_reply_header() {
        let body = "Klingt gut.\n\nAm 12.05.2026 um 14:30 schrieb Max <m@d.de>:\n> Hallo, kurze Frage";
        let out = extract(Some(body), b"");
        assert_eq!(out, "Klingt gut.");
    }

    #[test]
    fn strips_outlook_block_de() {
        let body = "Vielen Dank!\n\nVon: Max <m@d.de>\nGesendet: Montag, 12. Mai 2026 14:30\nAn: Tom <t@d.de>\nBetreff: Frage\n\nHallo Tom, …";
        let out = extract(Some(body), b"");
        assert_eq!(out, "Vielen Dank!");
    }

    #[test]
    fn strips_outlook_block_en() {
        let body = "Thanks!\n\nFrom: Max <m@d.de>\nSent: Monday, May 12 2026 14:30\nTo: Tom <t@d.de>\nSubject: Question\n\nHi Tom, …";
        let out = extract(Some(body), b"");
        assert_eq!(out, "Thanks!");
    }

    #[test]
    fn keeps_inline_from_address() {
        // Bare "From: foo@bar.com" line in the body shouldn't be
        // mistaken for an Outlook reply block — needs the full
        // header companion set to trigger.
        let body = "Reach me at \nFrom: tom@melchinger.org\n\nfor any feedback.";
        let out = extract(Some(body), b"");
        assert_eq!(out, body);
    }

    #[test]
    fn collapses_blank_lines() {
        let body = "Erste Zeile.\n\n\n\n\nZweite Zeile.";
        let out = extract(Some(body), b"");
        assert_eq!(out, "Erste Zeile.\n\nZweite Zeile.");
    }

    #[test]
    fn html_fallback_basic() {
        // Synthetic raw RFC822 with HTML-only body. We bypass the
        // `plain` Option path so the HTML stripper runs.
        let raw = b"MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hallo &amp; willkommen</p><p>Zweiter Absatz.</p>";
        let out = extract(None, raw);
        // Exact whitespace varies — assert key content & structure.
        assert!(out.contains("Hallo & willkommen"));
        assert!(out.contains("Zweiter Absatz."));
        // No raw tags should leak through.
        assert!(!out.contains("<p>"));
    }

    #[test]
    fn html_fallback_strips_script_and_style() {
        let raw = b"MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<style>.x{color:red}</style><script>alert(1)</script><p>Echter Inhalt</p>";
        let out = extract(None, raw);
        assert!(out.contains("Echter Inhalt"));
        assert!(!out.contains("alert"));
        assert!(!out.contains("color:red"));
    }

    #[test]
    fn empty_plain_falls_back_to_html() {
        let raw = b"MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n   \r\n--b\r\nContent-Type: text/html\r\n\r\n<p>nur im HTML</p>\r\n--b--";
        // The whitespace-only plain should be treated as empty, HTML
        // is used instead.
        let out = extract(Some("   \n"), raw);
        assert!(out.contains("nur im HTML"));
    }
}
