use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::account::AccountId;
use super::folder::FolderId;

/// Internal stable ID for a message. Distinct from IMAP UID (per-folder) and
/// Message-Id (untrusted, may repeat).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct MessageId(pub Uuid);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub id: MessageId,
    pub account_id: AccountId,
    pub folder_id: FolderId,
    pub imap_uid: u32,
    /// Raw RFC822 Message-Id header (if present).
    pub message_id_header: Option<String>,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub date: DateTime<Utc>,
    pub flags: Flags,
    /// `References` + `In-Reply-To` merged, in order. Feeds the JWZ threader.
    pub references: Vec<String>,
    pub size_bytes: u32,
    /// true once the full body has been downloaded into the blob store.
    pub body_cached: bool,
    /// True when the message carries at least one non-inline attachment.
    /// During sync this is a heuristic from the top-level Content-Type
    /// header (only `multipart/mixed` flips it to true — the standard
    /// MIME wrapper for "main content + attachments"); it gets corrected
    /// authoritatively when the body is fetched and `parse_metas` walks
    /// the decoded MIME tree.
    pub has_attachments: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flags {
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub draft: bool,
    pub deleted: bool,
    /// `$Forwarded` IMAP keyword — set when the user has forwarded the message.
    pub forwarded: bool,
    /// `$Junk` IMAP keyword (RFC 5788) — set when the user (or a server
    /// filter) marked the message as spam. Distinct from "lives in the
    /// Spam folder": a $Junk-flagged mail in the Inbox is the
    /// "server missed it, user corrected" signal feeding the
    /// filter-builder.
    pub junk: bool,
}

/// Sparse flag delta for partial updates: `None` means "keep current value",
/// `Some(_)` means "set to this value". Used by `set_message_flags` so the UI
/// can toggle one flag without re-sending the whole set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagChanges {
    pub seen: Option<bool>,
    pub answered: Option<bool>,
    pub flagged: Option<bool>,
    pub forwarded: Option<bool>,
    pub junk: Option<bool>,
}
