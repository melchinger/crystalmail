use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::account::AccountId;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct FolderId(pub Uuid);

/// IMAP folder (mailbox) state tracked for incremental sync.
///
/// `uid_validity` pinning: if the server reports a different value than we
/// stored, every envelope in that folder is stale and must be re-fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub name: String,
    pub uid_validity: u32,
    pub uid_next: u32,
    pub last_sync_ts: Option<DateTime<Utc>>,
}
