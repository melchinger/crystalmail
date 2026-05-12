// Read-only third-party iCal calendar subscriptions.
//
// Design (decided 2026-05-11 — see chat thread "C with App-start + Manual
// button + every X minutes default 60"):
//
//   * The list of subscriptions is persisted in `calendar_subscriptions.json`
//     next to `calendar_config.json` in the app's data dir.
//   * The fetched ICS body of each subscription is cached on disk in
//     `subscriptions/{id}.ics` under the app's *cache* dir. ETag and
//     Last-Modified live in the JSON state so re-fetches send the
//     conditional headers and skip a parse on 304.
//   * Events from those ICS files are NEVER inserted into the
//     `commitments` SQLite table. They live in an in-memory
//     `RwLock<HashMap<SubscriptionId, Vec<Commitment>>>` cache and get
//     mixed into `cal_list_in_range` at query time. This keeps a 5 MB
//     Google-Calendar share from doubling our SQLite-row count, and
//     makes unsubscribing a one-line cache `.remove()` instead of a
//     cascading DELETE.
//   * Read-only is enforced in the UI: the editor refuses to save/delete
//     events whose `subscription_id` is set. There is no protection at
//     the persistence layer because there is no persistence layer for
//     these rows.
//
// Concurrency:
//   * `persisted` (Vec<CalendarSubscription>) wraps the list state. A
//     single RwLock guards it; mutations grab the write lock briefly,
//     reads (the UI list, the periodic-tick scheduler) take the read
//     lock.
//   * `cache` is its own RwLock. Refresh does:  parse → take write lock
//     → swap → drop lock. Listing takes the read lock for the duration
//     of the in-range filter (microseconds).
//
// Out-of-scope for v1:
//   * CalDAV (push, deltas, authentication)
//   * BASIC/Bearer auth for URL subs (every public iCal feed I've seen
//     uses unauth URLs with a tokenized path)
//   * Subscribe-while-offline UX (we just fail the add and the user
//     re-tries)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::domain::Commitment;
use super::ics;

/// User-visible subscription record. Persisted to disk; surfaced 1:1 to
/// the frontend in the settings panel. ETag/LastModified live alongside
/// because they're effectively part of "what does the next refresh send".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarSubscription {
    pub id: String,
    pub name: String,
    pub source: SubscriptionSource,
    /// Auto-refresh cadence in minutes. Floor 5 (anything tighter and
    /// public iCal hosts start rate-limiting). 0 = manual only.
    pub refresh_interval_minutes: u32,
    pub enabled: bool,
    /// Hex color (`#rrggbb`) used to tint this calendar's events in the
    /// week/month views. Assigned by round-robin from a fixed palette on
    /// add; user-editable. `#[serde(default)]` so older records load
    /// without the field — they get the default color on first read.
    #[serde(default = "default_color")]
    pub color: String,
    /// UTC timestamp of the last completed refresh (success or 304). `None`
    /// before the first refresh happens.
    pub last_refreshed: Option<DateTime<Utc>>,
    /// Last error message (raw). Cleared on success.
    pub last_error: Option<String>,
    /// Event count from the most recent successful parse — handy in the UI
    /// so the user can confirm "I subscribed and 47 events showed up".
    pub last_event_count: Option<usize>,
    /// HTTP cache validators, persisted so a relaunch keeps the
    /// conditional-request optimisation. Both unused for `File` sources.
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Fixed 8-color palette for subscription badges. Picked for good
/// contrast against white text (used in week/month-view event bars) and
/// mutual distinguishability — no two adjacent palette entries are easy
/// to confuse. Slightly desaturated vs. pure web colors so a screen of
/// bars doesn't look like a clown convention.
const PALETTE: [&str; 8] = [
    "#3b82f6", // blue
    "#ef4444", // red
    "#10b981", // emerald
    "#f59e0b", // amber
    "#8b5cf6", // violet
    "#ec4899", // pink
    "#06b6d4", // cyan
    "#84cc16", // lime
];

fn default_color() -> String {
    PALETTE[0].to_string()
}

fn next_palette_color(existing_count: usize) -> String {
    PALETTE[existing_count % PALETTE.len()].to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubscriptionSource {
    File { path: String },
    Url { url: String },
}

/// Outcome of a single refresh attempt — exposed to the UI for inline
/// feedback. Either we got new data, the server said "still the same"
/// (304/file unchanged), or something blew up.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
pub enum RefreshReport {
    /// Fresh data parsed and cached. `event_count` is the in-memory count
    /// after RRULE expansion.
    Updated {
        subscription_id: String,
        event_count: usize,
    },
    /// Server returned 304 / file mtime unchanged. Cache untouched.
    NotModified { subscription_id: String },
    /// Request-level failure. The cached events from the previous
    /// successful run remain visible — partial outage shouldn't blank
    /// the user's calendar.
    Failed {
        subscription_id: String,
        error: String,
    },
}

pub struct SubscriptionStore {
    /// App data dir (state file lives here).
    data_dir: PathBuf,
    /// App cache dir (per-sub `.ics` blob lives here).
    cache_dir: PathBuf,
    persisted: RwLock<Vec<CalendarSubscription>>,
    /// `subscription_id` → already-expanded Commitments (RRULE flattened,
    /// `subscription_id` field stamped). Holding the parsed result here
    /// trades memory for not re-parsing on every list query.
    cache: RwLock<HashMap<String, Vec<Commitment>>>,
    http: reqwest::Client,
}

const STATE_FILE: &str = "calendar_subscriptions.json";
const CACHE_SUBDIR: &str = "subscriptions";
const MIN_INTERVAL_MIN: u32 = 5;

impl SubscriptionStore {
    /// Build a store and warm the in-memory cache from any `.ics` files
    /// left over from previous runs. Network is *not* touched here — we
    /// only re-parse what's on disk so the calendar shows events
    /// immediately on app launch, even offline.
    pub async fn load(data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        let _ = tokio::fs::create_dir_all(&data_dir).await;
        let _ = tokio::fs::create_dir_all(cache_dir.join(CACHE_SUBDIR)).await;

        let persisted: Vec<CalendarSubscription> = match tokio::fs::read(data_dir.join(STATE_FILE)).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        let http = reqwest::Client::builder()
            .user_agent(concat!("CrystalMail/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let store = Self {
            data_dir,
            cache_dir,
            persisted: RwLock::new(persisted),
            cache: RwLock::new(HashMap::new()),
            http,
        };
        store.warm_cache_from_disk().await;
        store
    }

    pub async fn list(&self) -> Vec<CalendarSubscription> {
        self.persisted.read().await.clone()
    }

    pub async fn add(
        &self,
        name: String,
        source: SubscriptionSource,
        refresh_interval_minutes: u32,
    ) -> Result<CalendarSubscription, String> {
        // Pick a color by round-robin so the Nth subscription gets the
        // Nth palette entry — no clashes until we hit the palette size.
        // User can change it later via `cal_subs_set_color`.
        let existing_count = self.persisted.read().await.len();
        let color = next_palette_color(existing_count);
        let sub = CalendarSubscription {
            id: Uuid::new_v4().to_string(),
            name,
            source,
            refresh_interval_minutes: refresh_interval_minutes.max(MIN_INTERVAL_MIN),
            enabled: true,
            color,
            last_refreshed: None,
            last_error: None,
            last_event_count: None,
            etag: None,
            last_modified: None,
            created_at: Utc::now(),
        };
        {
            let mut list = self.persisted.write().await;
            list.push(sub.clone());
        }
        self.persist().await?;
        Ok(sub)
    }

    pub async fn set_color(
        &self,
        id: &str,
        color: String,
    ) -> Result<CalendarSubscription, String> {
        // Light validation — `#rrggbb`, lowercase. We don't want to
        // accept arbitrary CSS color strings because the UI assumes
        // hex when computing contrast / dimming.
        let trimmed = color.trim().to_ascii_lowercase();
        let is_valid = trimmed.len() == 7
            && trimmed.starts_with('#')
            && trimmed[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !is_valid {
            return Err(format!("invalid color {color:?}, expected #rrggbb"));
        }
        let updated = {
            let mut list = self.persisted.write().await;
            let sub = list
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("subscription {id} not found"))?;
            sub.color = trimmed;
            sub.clone()
        };
        self.persist().await?;
        Ok(updated)
    }

    pub async fn remove(&self, id: &str) -> Result<(), String> {
        {
            let mut list = self.persisted.write().await;
            let before = list.len();
            list.retain(|s| s.id != id);
            if list.len() == before {
                return Err(format!("subscription {id} not found"));
            }
        }
        self.cache.write().await.remove(id);
        let _ = tokio::fs::remove_file(self.cache_file(id)).await;
        self.persist().await?;
        Ok(())
    }

    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<CalendarSubscription, String> {
        let updated = {
            let mut list = self.persisted.write().await;
            let sub = list
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("subscription {id} not found"))?;
            sub.enabled = enabled;
            sub.clone()
        };
        self.persist().await?;
        Ok(updated)
    }

    pub async fn set_interval(
        &self,
        id: &str,
        minutes: u32,
    ) -> Result<CalendarSubscription, String> {
        let updated = {
            let mut list = self.persisted.write().await;
            let sub = list
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| format!("subscription {id} not found"))?;
            // 0 stays 0 (manual-only); everything else floors to MIN.
            sub.refresh_interval_minutes = if minutes == 0 {
                0
            } else {
                minutes.max(MIN_INTERVAL_MIN)
            };
            sub.clone()
        };
        self.persist().await?;
        Ok(updated)
    }

    /// Refresh one subscription. Stamps `last_refreshed` and either
    /// `last_event_count` (on Updated/NotModified) or `last_error` (on
    /// Failed) inside the persisted entry. Always returns a report
    /// rather than `Err` — refresh failures are normal user-facing UX,
    /// not exceptional errors.
    pub async fn refresh(&self, id: &str) -> RefreshReport {
        let sub = {
            let list = self.persisted.read().await;
            match list.iter().find(|s| s.id == id) {
                Some(s) => s.clone(),
                None => {
                    return RefreshReport::Failed {
                        subscription_id: id.to_string(),
                        error: "subscription not found".into(),
                    };
                }
            }
        };
        let result = self.fetch_and_parse(&sub).await;
        self.apply_refresh_result(id, &result).await;
        result
    }

    /// Run `refresh` for every subscription whose `enabled == true`,
    /// `refresh_interval_minutes != 0`, and whose `last_refreshed` is
    /// older than `interval`. Designed to be polled every 60 s by a
    /// background task; cheap to call when nothing is due.
    pub async fn refresh_all_due(&self) -> Vec<RefreshReport> {
        let now = Utc::now();
        let due_ids: Vec<String> = {
            let list = self.persisted.read().await;
            list.iter()
                .filter(|s| s.enabled && s.refresh_interval_minutes > 0)
                .filter(|s| {
                    let interval = chrono::Duration::minutes(
                        i64::from(s.refresh_interval_minutes),
                    );
                    match s.last_refreshed {
                        None => true,
                        Some(t) => now.signed_duration_since(t) >= interval,
                    }
                })
                .map(|s| s.id.clone())
                .collect()
        };
        let mut out = Vec::with_capacity(due_ids.len());
        for id in due_ids {
            out.push(self.refresh(&id).await);
        }
        out
    }

    /// Merge subscription events overlapping `[from, to)` into a single
    /// Vec. Used by `cal_list_in_range` to overlay subscriptions onto
    /// the SQLite-backed events without persisting them.
    pub async fn events_in_range(&self, from: &str, to: &str) -> Vec<Commitment> {
        let enabled_ids: Vec<String> = {
            let list = self.persisted.read().await;
            list.iter()
                .filter(|s| s.enabled)
                .map(|s| s.id.clone())
                .collect()
        };
        let cache = self.cache.read().await;
        let mut out = Vec::new();
        for id in &enabled_ids {
            let Some(events) = cache.get(id) else { continue };
            for ev in events {
                // Half-open `[from, to)` overlap test: keep events that
                // touch the window. String comparison works because
                // start_at / end_at are RFC 3339 — sortable as plain
                // ASCII.
                if ev.start_at.as_str() < to && ev.end_at.as_str() > from {
                    out.push(ev.clone());
                }
            }
        }
        out
    }

    // ─── internals ────────────────────────────────────────────────────────

    fn state_file(&self) -> PathBuf {
        self.data_dir.join(STATE_FILE)
    }

    fn cache_file(&self, id: &str) -> PathBuf {
        self.cache_dir.join(CACHE_SUBDIR).join(format!("{id}.ics"))
    }

    async fn persist(&self) -> Result<(), String> {
        let snapshot = self.persisted.read().await.clone();
        let bytes = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| format!("serialize subscriptions: {e}"))?;
        let path = self.state_file();
        // Write atomically via tmp+rename so a crash mid-write doesn't
        // corrupt the state file.
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|e| format!("write {tmp:?}: {e}"))?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|e| format!("rename {tmp:?} -> {path:?}: {e}"))?;
        Ok(())
    }

    async fn warm_cache_from_disk(&self) {
        let subs = self.persisted.read().await.clone();
        for sub in subs {
            let path = self.cache_file(&sub.id);
            if !path.exists() {
                continue;
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => {
                    if let Ok(events) = parse_and_expand(&bytes, &sub.id) {
                        self.cache.write().await.insert(sub.id.clone(), events);
                    }
                }
                Err(_) => continue,
            }
        }
    }

    async fn fetch_and_parse(&self, sub: &CalendarSubscription) -> RefreshReport {
        match &sub.source {
            SubscriptionSource::File { path } => self.refresh_from_file(sub, Path::new(path)).await,
            SubscriptionSource::Url { url } => self.refresh_from_url(sub, url).await,
        }
    }

    async fn refresh_from_file(
        &self,
        sub: &CalendarSubscription,
        path: &Path,
    ) -> RefreshReport {
        let bytes = match tokio::fs::read(path).await {
            Ok(b) => b,
            Err(e) => {
                return RefreshReport::Failed {
                    subscription_id: sub.id.clone(),
                    error: format!("read {path:?}: {e}"),
                };
            }
        };
        // Cheap unchanged-detection for file sources: compare bytes to
        // the previously cached blob. Avoids re-parsing identical files
        // on every poll for users who locally export their calendar.
        let cache_path = self.cache_file(&sub.id);
        if let Ok(prev) = tokio::fs::read(&cache_path).await {
            if prev == bytes {
                return RefreshReport::NotModified {
                    subscription_id: sub.id.clone(),
                };
            }
        }
        self.persist_cache_and_parse(sub, &bytes).await
    }

    async fn refresh_from_url(
        &self,
        sub: &CalendarSubscription,
        url: &str,
    ) -> RefreshReport {
        // Normalize webcal:// → https:// (Apple/Outlook share format that
        // every browser maps to HTTPS under the hood).
        let normalized = if let Some(rest) = url.strip_prefix("webcal://") {
            format!("https://{rest}")
        } else if let Some(rest) = url.strip_prefix("webcals://") {
            format!("https://{rest}")
        } else {
            url.to_string()
        };
        let mut req = self.http.get(&normalized);
        if let Some(etag) = &sub.etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(lm) = &sub.last_modified {
            req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return RefreshReport::Failed {
                    subscription_id: sub.id.clone(),
                    error: format!("http: {e}"),
                };
            }
        };
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return RefreshReport::NotModified {
                subscription_id: sub.id.clone(),
            };
        }
        if !resp.status().is_success() {
            return RefreshReport::Failed {
                subscription_id: sub.id.clone(),
                error: format!("HTTP {}", resp.status()),
            };
        }
        let new_etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let new_last_modified = resp
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                return RefreshReport::Failed {
                    subscription_id: sub.id.clone(),
                    error: format!("read body: {e}"),
                };
            }
        };
        // Stash validators before persisting+parsing so the next call
        // sees them even if the parse fails. (We want to NOT redownload
        // a known-bad blob just because parse errored.)
        {
            let mut list = self.persisted.write().await;
            if let Some(s) = list.iter_mut().find(|s| s.id == sub.id) {
                s.etag = new_etag;
                s.last_modified = new_last_modified;
            }
        }
        self.persist_cache_and_parse(sub, &bytes).await
    }

    /// Common tail of refresh_from_file/url: write bytes to the cache
    /// file, parse them, swap the memory cache.
    async fn persist_cache_and_parse(
        &self,
        sub: &CalendarSubscription,
        bytes: &[u8],
    ) -> RefreshReport {
        let cache_path = self.cache_file(&sub.id);
        if let Some(parent) = cache_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if let Err(e) = tokio::fs::write(&cache_path, bytes).await {
            return RefreshReport::Failed {
                subscription_id: sub.id.clone(),
                error: format!("write cache: {e}"),
            };
        }
        match parse_and_expand(bytes, &sub.id) {
            Ok(events) => {
                let count = events.len();
                self.cache.write().await.insert(sub.id.clone(), events);
                RefreshReport::Updated {
                    subscription_id: sub.id.clone(),
                    event_count: count,
                }
            }
            Err(e) => RefreshReport::Failed {
                subscription_id: sub.id.clone(),
                error: format!("parse: {e}"),
            },
        }
    }

    async fn apply_refresh_result(&self, id: &str, result: &RefreshReport) {
        let now = Utc::now();
        {
            let mut list = self.persisted.write().await;
            let Some(sub) = list.iter_mut().find(|s| s.id == id) else { return };
            match result {
                RefreshReport::Updated { event_count, .. } => {
                    sub.last_refreshed = Some(now);
                    sub.last_event_count = Some(*event_count);
                    sub.last_error = None;
                }
                RefreshReport::NotModified { .. } => {
                    sub.last_refreshed = Some(now);
                    sub.last_error = None;
                }
                RefreshReport::Failed { error, .. } => {
                    sub.last_error = Some(error.clone());
                }
            }
        }
        let _ = self.persist().await;
    }
}

/// Parse an ICS blob and turn every VEVENT (including RRULE expansions)
/// into a `Commitment` tagged with the subscription id. Read-only — the
/// `id` of each row is a fresh UUID and the rows never enter the DB.
fn parse_and_expand(bytes: &[u8], subscription_id: &str) -> Result<Vec<Commitment>, String> {
    let parsed = ics::parse_all(bytes)?;
    let mut out = Vec::new();
    for ev in parsed {
        // Per-event RRULE expansion. `ics_to_commitments` already caps
        // occurrences at 200, so a pathological subscription can't blow
        // up our memory cache.
        let rows = match ics::ics_to_commitments(&ev, None, None, None) {
            Ok(rs) => rs,
            Err(_) => continue,
        };
        for mut c in rows {
            c.subscription_id = Some(subscription_id.to_string());
            out.push(c);
        }
    }
    Ok(out)
}

/// Spawn the periodic refresh task — ticks every 60 s and refreshes any
/// subscription whose `interval` has elapsed. Cancellation isn't wired:
/// the task lives for the app's lifetime, which matches the rest of our
/// `tokio::spawn`-and-forget background work.
pub fn spawn_background_refresh(store: Arc<SubscriptionStore>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        // Drop the first immediate tick — we don't need to slam every
        // subscription at startup because `load()` warmed the cache
        // from disk already.
        tick.tick().await;
        loop {
            tick.tick().await;
            let _ = store.refresh_all_due().await;
        }
    });
}
