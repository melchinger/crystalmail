// Attachment handling — parse from raw RFC822 and save to disk on demand.
//
// We deliberately don't persist the decoded bytes: the raw RFC822 lives in the
// `bodies.raw_rfc822` BLOB, and mail-parser re-decodes on demand (base64 over a
// few MB is negligible next to the IMAP fetch). Metadata (filename, MIME, size,
// cid, inline flag) is surfaced in `MessageDetail.attachments` so the UI can
// render the attachment bar without any extra round-trip.

use std::path::{Path, PathBuf};

use mail_parser::{MessageParser, MimeHeaders, PartType};
use serde::Serialize;

use crate::domain::message::MessageId;
use crate::infrastructure::db::DbHandle;
use crate::infrastructure::queries;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    /// Index into `msg.attachments()` — stable within a given RFC822 blob.
    pub part_idx: u32,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u32,
    /// RFC 2392 Content-Id without the angle brackets, for `cid:` HTML refs.
    pub content_id: Option<String>,
    /// True when the part is referenced from the HTML body (typically an
    /// inline image). Inline attachments are still listed so the user can
    /// download them, but the Reader hides them from the main chip row.
    pub is_inline: bool,
}

/// Extract attachment metadata from a raw RFC822 byte slice.
pub fn parse_metas(raw: &[u8]) -> Vec<AttachmentMeta> {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, part) in msg.attachments().enumerate() {
        let bytes_len = match &part.body {
            PartType::Text(s) | PartType::Html(s) => s.as_bytes().len(),
            PartType::Binary(b) | PartType::InlineBinary(b) => b.len(),
            PartType::Message(_) => 0,
            PartType::Multipart(_) => 0,
        };
        let filename = part
            .attachment_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| default_name(&part.content_type().and_then(|ct| {
                match (ct.ctype(), ct.subtype()) {
                    (t, Some(s)) => Some(format!("{t}/{s}")),
                    (t, None) => Some(t.to_string()),
                }
            }).unwrap_or_else(|| "application/octet-stream".into()), idx));
        let mime_type = part
            .content_type()
            .and_then(|ct| match (ct.ctype(), ct.subtype()) {
                (t, Some(s)) => Some(format!("{t}/{s}")),
                (t, None) => Some(t.to_string()),
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let content_id = part.content_id().map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string());
        // mail-parser exposes Content-Disposition implicitly via is_inline/is_attachment;
        // matching InlineBinary body type is the reliable signal.
        let is_inline = matches!(part.body, PartType::InlineBinary(_));
        out.push(AttachmentMeta {
            part_idx: idx as u32,
            filename,
            mime_type,
            size_bytes: bytes_len as u32,
            content_id,
            is_inline,
        });
    }
    out
}

fn default_name(mime: &str, idx: usize) -> String {
    let ext = match mime.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/zip" => "zip",
        _ => "bin",
    };
    format!("attachment-{}.{ext}", idx + 1)
}

/// Return the decoded bytes for a specific attachment part index, by
/// re-parsing the cached raw RFC822 blob. Used by `save_attachment` and by
/// inline-image resolution for the HTML sandbox iframe.
pub fn bytes(
    db: &DbHandle,
    message_id: &MessageId,
    part_idx: u32,
) -> Result<(Vec<u8>, String, String), String> {
    let raw = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        let body = queries::get_body_raw(&conn, message_id).map_err(|e| e.to_string())?;
        body.ok_or("body not cached — open the message first")?
    };
    let msg = MessageParser::default()
        .parse(&raw)
        .ok_or("failed to parse cached RFC822 body")?;
    let part = msg
        .attachments()
        .nth(part_idx as usize)
        .ok_or("attachment index out of range")?;
    let data: Vec<u8> = match &part.body {
        PartType::Text(s) | PartType::Html(s) => s.as_bytes().to_vec(),
        PartType::Binary(b) | PartType::InlineBinary(b) => b.to_vec(),
        _ => return Err("unsupported attachment part type".into()),
    };
    let filename = part
        .attachment_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("attachment-{}", part_idx + 1));
    let mime_type = part
        .content_type()
        .and_then(|ct| match (ct.ctype(), ct.subtype()) {
            (t, Some(s)) => Some(format!("{t}/{s}")),
            (t, None) => Some(t.to_string()),
        })
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok((data, filename, mime_type))
}

/// Save a single attachment to `destination`. The caller is expected to have
/// obtained the destination via a platform save dialog (`@tauri-apps/plugin-dialog`).
pub fn save_to(
    db: &DbHandle,
    message_id: &MessageId,
    part_idx: u32,
    destination: &Path,
) -> Result<PathBuf, String> {
    let (data, _, _) = bytes(db, message_id, part_idx)?;
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create parent dir: {e}"))?;
        }
    }
    std::fs::write(destination, &data).map_err(|e| format!("write file: {e}"))?;
    Ok(destination.to_path_buf())
}

/// Write the attachment to a stable per-message temp directory and hand the
/// path to the OS default application (PDF viewer, Word, image viewer …).
///
/// Why not stream directly into a viewer pipe: most desktop viewers expect a
/// real file path, not stdin. Why a per-message subdir: the user's existing
/// viewer instance (e.g. Adobe with tabs open) sees a stable filename when
/// they reopen the same attachment, instead of a churning `tmpXXXX` name.
/// The temp tree lives under the OS temp dir, so the OS handles eventual
/// cleanup; we don't try to delete on close because the viewer might still
/// have the file mapped.
///
/// Idempotent within a session: re-opening the same attachment overwrites
/// the same file, so disk usage doesn't grow with each click.
pub fn open_with_default(
    db: &DbHandle,
    message_id: &MessageId,
    part_idx: u32,
) -> Result<PathBuf, String> {
    let (data, filename, _mime) = bytes(db, message_id, part_idx)?;

    // Stable per-message folder. Filename is taken from the attachment but
    // sanitised so a malicious sender can't punch out of the temp dir or
    // create OS-illegal characters that break ShellExecute / xdg-open.
    let safe_name = sanitize_filename(&filename);
    let dir = std::env::temp_dir()
        .join("crystalmail")
        .join("attachments")
        .join(message_id.0.to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create temp dir: {e}"))?;
    let path = dir.join(&safe_name);

    // Overwrite on every open — re-decoding is cheap (a few MB of base64),
    // and we want the bytes to match what's currently in the DB even if
    // the user re-fetched the body in between.
    std::fs::write(&path, &data).map_err(|e| format!("write temp file: {e}"))?;

    opener::open(&path).map_err(|e| format!("open with default app: {e}"))?;
    Ok(path)
}

/// Strip path separators, control chars, and Windows-reserved characters
/// from a filename so it's safe to use as the leaf of a system path. We
/// don't strictly need to handle reserved-name edge cases (CON, PRN, …)
/// since we're always inside a per-message subdir — those names are only
/// problematic at root. Empty result falls back to `attachment.bin`.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            // Disallowed on Windows + path separators on every OS.
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            // Control chars + DEL.
            c if (c as u32) < 0x20 || c == '\u{7f}' => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "attachment.bin".to_string()
    } else {
        trimmed.to_string()
    }
}
