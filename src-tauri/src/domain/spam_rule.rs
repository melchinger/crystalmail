use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::account::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpamRuleId(pub Uuid);

/// Pattern types the rule engine understands. Closed enum so a typo in the
/// UI can't introduce a match path we haven't implemented.
///
/// Deliberately small: four flavors cover 95 % of the patterns pi will
/// propose and the user can write by hand. Additional types (header match,
/// SPF failure, etc.) can be added when a concrete need arises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpamPatternType {
    /// Full sender address, case-insensitive exact match.
    FromEmail,
    /// Domain portion of sender (after `@`), case-insensitive exact match.
    FromDomain,
    /// Substring match in subject, case-insensitive.
    SubjectContains,
    /// Regex match against subject. Validated via `regex::Regex::new` at save time.
    SubjectRegex,
    /// Substring match in plain-text body preview (first 500 chars), case-insensitive.
    BodyContains,
    /// Substring match against the RFC 5322 header block (`X-Spam-Flag: YES`,
    /// `Authentication-Results: dkim=fail`, `List-Unsubscribe:` presence, …).
    /// Header names are case-insensitive per RFC, so we lowercase both
    /// sides before matching. Pattern example: `x-spam-status: yes`.
    HeaderContains,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpamRule {
    pub id: SpamRuleId,
    /// `None` = global rule applying to all accounts. `Some(_)` narrows it.
    pub account_id: Option<AccountId>,
    pub pattern_type: SpamPatternType,
    pub pattern: String,
    pub enabled: bool,
    /// pi's confidence 0..1 when the rule was proposed; `None` for hand-written.
    pub confidence: Option<f64>,
    /// Rationale from pi or the user — shown as tooltip in the rule list.
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub hit_count: u64,
}
