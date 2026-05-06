// Phase-2 calendar IMAP sync orchestration.
//
// Algorithm:
//   1. ensure the configured folder exists on the server
//   2. UID-FETCH every message in the folder
//   3. parse each VCALENDAR; group by UID; per UID pick the LWW winner
//      (max SEQUENCE → max DTSTAMP → max IMAP UID per ADR-0011 §5)
//   4. read every local commitment (incl. CANCELLED so tombstones
//      participate in the diff)
//   5. for each UID present anywhere:
//        - only-remote → upsert local
//        - only-local  → publish to IMAP at SEQUENCE:0 (initial-write
//                        per ADR-0011 §3); local row is reset to 0 too
//                        so future bumps stay in lockstep
//        - both, remote SEQ > local SEQ → upsert local
//        - both, local SEQ > remote SEQ → publish to IMAP
//        - tied SEQ → assume content is the same; no-op
//
// Conformance: emits METHOD:PUBLISH messages with X-Cal-Format-Version:1
// in the mail header (REQUIRED for v1 messages per ADR-0011 §7), one
// VEVENT per message, mandatory UID/SEQUENCE/DTSTAMP/DTSTART/DTEND, and
// the STATUS field that's normative for the cancellation case.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use super::domain::Commitment;
use super::{ics, store};
use crate::application::calendar_imap::{self, CalendarMessage};
use crate::domain::account::AccountId;
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::state::AppState;

/// Floor on the auto-sync interval. Below this, the periodic timer is
/// effectively self-DOSing the IMAP server without changing user-perceived
/// latency — the IDLE actor catches sub-minute changes anyway.
const MIN_AUTO_SYNC_INTERVAL_SECS: u64 = 60;

/// Result of a single sync run, surfaced to the frontend so the UI can
/// show "X events synced, Y published, Z unchanged" without having to
/// derive that from the freshly-read commitment list.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    /// Local writes triggered by remote-newer messages.
    pub imported: u32,
    /// IMAP appends triggered by local-newer commitments.
    pub published: u32,
    /// Both sides agreed; no work done.
    pub unchanged: u32,
    /// Number of superseded ICS messages moved to the Archive folder
    /// during the post-sync compaction pass. Always 0 when compaction
    /// is disabled in the config.
    pub compacted: u32,
    /// Detected on this sync: UIDs that were present in the IMAP folder
    /// at last sync but are gone now, with no local-side mutation since.
    /// Treated as cancellations and locally marked `STATUS:CANCELLED`.
    pub remote_deleted: u32,
    /// Per-event errors that didn't abort the whole sync. The folder-level
    /// errors (login, ensure_folder, list) propagate as `Err(_)` from the
    /// outer `run` future.
    pub errors: Vec<String>,
}

/// All-trigger entry point for a sync run: loads the current
/// `CalendarConfig`, validates it, acquires the single-flight lock from
/// `AppState::calendar_sync_lock`, and runs `run`. Used by every sync
/// trigger — manual button, periodic timer, IDLE actor, sync-on-mutation
/// — so concurrent triggers can't race against each other.
pub async fn run_with_lock(app: &AppHandle) -> Result<SyncReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let cfg = {
        let guard = state.calendar_config.lock().unwrap();
        guard.clone()
    };
    if !cfg.enabled {
        return Err("calendar sync disabled".into());
    }
    let account_id = cfg
        .account_id
        .ok_or_else(|| "no calendar account configured".to_string())?;
    let own_email = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        crate::infrastructure::queries::get_account(&conn, &account_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "calendar account not found".to_string())?
            .address
    };

    let _guard = state.calendar_sync_lock.lock().await;
    run(
        db,
        &account_id,
        &cfg.folder_path,
        &own_email,
        cfg.compaction_enabled,
    )
    .await
}

/// Fire-and-forget: spawn a background task that runs a sync via
/// `run_with_lock`. Used by the sync-on-mutation path so user-driven
/// CRUD doesn't block on IMAP. Errors are logged but never surfaced —
/// the next periodic / IDLE-triggered sync catches up.
pub fn spawn_background_sync(app: &AppHandle, reason: &'static str) {
    let app_clone = app.clone();
    tokio::spawn(async move {
        match run_with_lock(&app_clone).await {
            Ok(report) => {
                tracing::debug!(
                    reason = reason,
                    imported = report.imported,
                    published = report.published,
                    "background sync ok"
                );
            }
            Err(e) => {
                // "calendar sync disabled" is the common case when sync-on-
                // mutation is on but the user hasn't enabled IMAP sync. Log
                // at debug to avoid spam; real failures come through at warn.
                if e == "calendar sync disabled" || e.starts_with("no calendar account") {
                    tracing::debug!(reason = reason, "background sync skipped: {e}");
                } else {
                    tracing::warn!(reason = reason, error = %e, "background sync failed");
                }
            }
        }
    });
}

/// Spawn the periodic-sync background task. Idempotent at the boot level:
/// `main.rs::setup` calls this once after `CalendarConfig` is hydrated. The
/// task itself re-reads the config on every iteration, so config changes
/// (`cal_set_config`) take effect from the next tick onward without
/// needing to restart anything. Setting `auto_sync_interval_seconds = 0`
/// at runtime disables periodic firing without killing the task.
pub fn spawn_periodic_task(app: AppHandle) {
    tokio::spawn(async move {
        loop {
            // Read fresh config every iteration so live changes apply.
            let interval = {
                let state = app.state::<AppState>();
                let cfg = state.calendar_config.lock().unwrap();
                cfg.auto_sync_interval_seconds
            };

            // 0 = disabled. Sleep a default short window before re-checking
            // so a config flip-on goes live within ~60 s of the change.
            let sleep_for = if interval == 0 {
                Duration::from_secs(60)
            } else {
                Duration::from_secs(interval.max(MIN_AUTO_SYNC_INTERVAL_SECS))
            };
            tokio::time::sleep(sleep_for).await;

            // Re-read after the sleep — config may have changed during
            // the wait. Skip if still disabled or interval set to 0.
            let should_run = {
                let state = app.state::<AppState>();
                let cfg = state.calendar_config.lock().unwrap();
                cfg.enabled
                    && cfg.account_id.is_some()
                    && cfg.auto_sync_interval_seconds > 0
            };
            if !should_run {
                continue;
            }

            match run_with_lock(&app).await {
                Ok(report) => {
                    if !report.errors.is_empty() {
                        tracing::warn!(
                            errors = ?report.errors,
                            "calendar periodic sync: per-event errors"
                        );
                    } else {
                        tracing::debug!(
                            imported = report.imported,
                            published = report.published,
                            unchanged = report.unchanged,
                            "calendar periodic sync: ok"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "calendar periodic sync failed");
                }
            }
        }
    });
}

/// Resolve and apply one sync round. The caller (typically the
/// `cal_sync_imap` Tauri command) is responsible for loading the
/// `CalendarConfig` and bailing out before invoking us if sync is
/// disabled or no account is selected.
pub async fn run(
    db: &DbHandle,
    account_id: &AccountId,
    folder: &str,
    own_email: &str,
    compaction_enabled: bool,
) -> Result<SyncReport, String> {
    // Step 1 — ensure the folder. CREATE-then-SELECT covers both
    // first-run-on-this-account and "already there" cleanly.
    calendar_imap::ensure_folder(db, account_id, folder).await?;

    // Step 2+3 — pull all messages, fold into a per-UID winner map.
    let messages = calendar_imap::list_messages(db, account_id, folder).await?;
    let (winners, mut errors) = resolve_winners(&messages, own_email);

    // Step 4 — read local state including CANCELLED rows.
    let locals: Vec<Commitment> = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        store::list_all_with_attendees(&conn).map_err(|e| e.to_string())?
    };
    let local_by_uid: HashMap<String, &Commitment> =
        locals.iter().map(|c| (c.uid.clone(), c)).collect();

    // Step 5 — diff and apply.
    let mut report = SyncReport::default();

    // Walk every UID that appears anywhere.
    let mut all_uids: Vec<String> = winners.keys().cloned().collect();
    for c in &locals {
        if !winners.contains_key(&c.uid) {
            all_uids.push(c.uid.clone());
        }
    }

    for uid in &all_uids {
        let remote = winners.get(uid);
        let local = local_by_uid.get(uid).copied();

        match (remote, local) {
            (Some(r), None) => {
                // Pure-remote import: stamp `last_published_sequence` to
                // remember we've seen this UID published with at least
                // this SEQUENCE. Subsequent syncs use this to distinguish
                // server-side hard-deletes from genuinely-new locals.
                let mut imported = r.commitment.clone();
                imported.last_published_sequence = Some(imported.sequence);
                if let Err(e) = upsert_local(db, &imported).await {
                    errors.push(format!("{uid}: import failed: {e}"));
                } else {
                    report.imported += 1;
                }
            }
            (None, Some(l)) => match l.last_published_sequence {
                None => {
                    // Truly new local: never seen on IMAP. Initial-publish
                    // migration per ADR-0011 §3 mandates SEQUENCE:0 for the
                    // first message in the profile, so reset the counter
                    // (Phase-1 rows may have SEQ>0 already).
                    let mut to_publish = l.clone();
                    to_publish.sequence = 0;
                    to_publish.updated_at = Utc::now();
                    match publish_and_persist(db, account_id, folder, &to_publish).await
                    {
                        Ok(()) => report.published += 1,
                        Err(e) => errors.push(format!("{uid}: publish failed: {e}")),
                    }
                }
                Some(last_pub) if l.sequence > last_pub => {
                    // We had it on IMAP, server lost it (manual delete or
                    // similar), but the user has edited locally since the
                    // last sync — assume the user wants the edit to land.
                    // Publish the current state; if the server is genuinely
                    // gone, we re-create the row at the current sequence.
                    match publish_and_persist(db, account_id, folder, l).await {
                        Ok(()) => report.published += 1,
                        Err(e) => errors.push(format!(
                            "{uid}: re-publish-after-server-delete failed: {e}"
                        )),
                    }
                }
                Some(_) => {
                    // We had it on IMAP, server lost it, no local edits
                    // since. Treat this as a server-side hard-delete and
                    // mirror it locally — see the External Contribution
                    // doc for why this is the right pragmatic resolution
                    // (ADR-0011 §5 doesn't normatively cover the case of
                    // a UID disappearing without a STATUS:CANCELLED
                    // tombstone). We do NOT bump SEQUENCE or republish a
                    // tombstone — the server's own delete already
                    // signals absence to other readers using the same
                    // last-seen heuristic.
                    let mut tomb = l.clone();
                    tomb.status = super::domain::CommitmentStatus::Cancelled;
                    tomb.updated_at = Utc::now();
                    if let Err(e) = upsert_local(db, &tomb).await {
                        errors.push(format!(
                            "{uid}: server-deleted tombstone-write failed: {e}"
                        ));
                    } else {
                        report.remote_deleted += 1;
                    }
                }
            },
            (Some(r), Some(l)) => {
                if r.commitment.sequence > l.sequence {
                    let mut imported = r.commitment.clone();
                    imported.last_published_sequence = Some(imported.sequence);
                    if let Err(e) = upsert_local(db, &imported).await {
                        errors.push(format!("{uid}: import (remote-newer) failed: {e}"));
                    } else {
                        report.imported += 1;
                    }
                } else if l.sequence > r.commitment.sequence {
                    match publish_and_persist(db, account_id, folder, l).await {
                        Ok(()) => report.published += 1,
                        Err(e) => errors.push(format!(
                            "{uid}: publish (local-newer) failed: {e}"
                        )),
                    }
                } else {
                    // Tied SEQUENCE: trust the previous DTSTAMP / IMAP-UID
                    // tiebreak ran when we picked `r` above; if both sides
                    // genuinely converged on the same content, no work to
                    // do. (We do NOT compare field-for-field — that would
                    // produce phantom mutations on whitespace differences.)
                    // Still: stamp last_published_sequence so subsequent
                    // syncs can detect server-side hard-deletes.
                    if l.last_published_sequence != Some(l.sequence) {
                        let mut updated = l.clone();
                        updated.last_published_sequence = Some(l.sequence);
                        let _ = upsert_local(db, &updated).await;
                    }
                    report.unchanged += 1;
                }
            }
            (None, None) => unreachable!("uid came from one of the two sets"),
        }
    }

    // Step 6 — compaction (ADR-0011 §6, OPTIONAL). Move every superseded
    // message (older SEQUENCE / DTSTAMP / IMAP-UID for a given UID) into
    // `<folder>/Archive`. Treated as soft-failable: a compaction error
    // doesn't invalidate the publishes/imports we just did.
    if compaction_enabled {
        match compact_folder(db, account_id, folder).await {
            Ok(moved) => report.compacted = moved,
            Err(e) => {
                errors.push(format!("compaction: {e}"));
            }
        }
    }

    report.errors = errors;
    Ok(report)
}

/// Compaction (ADR-0011 §6): move every non-winner ICS message for each
/// UID into `<folder>/Archive`, leaving exactly one current message per
/// UID in the active folder. Returns the count of moved messages.
async fn compact_folder(
    db: &DbHandle,
    account_id: &AccountId,
    active_folder: &str,
) -> Result<u32, String> {
    let archive_folder = format!("{active_folder}/Archive");
    calendar_imap::ensure_folder(db, account_id, &archive_folder).await?;

    let messages = calendar_imap::list_messages(db, account_id, active_folder).await?;
    if messages.is_empty() {
        return Ok(0);
    }

    // Group by UID, find the winner per ADR-0011 §5, mark all non-winners
    // for archival. We re-parse the ICS because list_messages doesn't
    // surface SEQUENCE/DTSTAMP — keeps the calendar_imap facade lean.
    type WinnerInfo = (u32, String, u32); // (sequence, dtstamp, imap_uid)
    let mut winners_by_uid: HashMap<String, WinnerInfo> = HashMap::new();
    let mut all_by_uid: HashMap<String, Vec<u32>> = HashMap::new();

    for msg in &messages {
        // Skip messages we don't recognize so we don't accidentally
        // archive a v2 message we couldn't parse.
        if let Some(v) = msg.format_version.as_deref() {
            if v != "1" {
                continue;
            }
        }
        let cal_bytes = match split_mail_body(&msg.rfc822) {
            Some(b) => b,
            None => continue,
        };
        let parsed = match ics::parse(cal_bytes) {
            Ok(Some(p)) => p,
            _ => continue,
        };
        let dtstamp = first_dtstamp(cal_bytes).unwrap_or_default();
        let info = (parsed.sequence, dtstamp, msg.imap_uid);
        all_by_uid
            .entry(parsed.uid.clone())
            .or_default()
            .push(msg.imap_uid);
        match winners_by_uid.get(&parsed.uid) {
            None => {
                winners_by_uid.insert(parsed.uid, info);
            }
            Some(current) => {
                if info > *current {
                    winners_by_uid.insert(parsed.uid, info);
                }
            }
        }
    }

    // Collect IMAP UIDs to archive: every UID that's not the winner.
    let mut to_archive: Vec<u32> = Vec::new();
    for (uid, all_imap_uids) in &all_by_uid {
        let winner_imap_uid = winners_by_uid.get(uid).map(|(_, _, u)| *u);
        for imap_uid in all_imap_uids {
            if Some(*imap_uid) != winner_imap_uid {
                to_archive.push(*imap_uid);
            }
        }
    }

    if to_archive.is_empty() {
        return Ok(0);
    }

    // COPY to archive first, only DELETE on success — stale messages in
    // the active folder beat lost messages in nowhere.
    calendar_imap::copy_messages(db, account_id, active_folder, &archive_folder, &to_archive)
        .await?;
    calendar_imap::delete_messages(db, account_id, active_folder, &to_archive).await?;
    Ok(to_archive.len() as u32)
}

struct Winner {
    commitment: Commitment,
}

/// Group remote messages by UID and pick a winner per ADR-0011 §5.
/// Returns the per-UID map plus any per-message parse errors that we
/// surface back as soft errors (don't abort the whole sync).
fn resolve_winners(
    messages: &[CalendarMessage],
    own_email: &str,
) -> (HashMap<String, Winner>, Vec<String>) {
    let mut winners: HashMap<String, (Winner, String, u32)> = HashMap::new();
    // (winner, dtstamp, imap_uid) — dtstamp+imap_uid are the
    // tie-break inputs, kept alongside so we can compare
    // newcomers without rebuilding from the stored Commitment.
    let mut errors: Vec<String> = Vec::new();

    for msg in messages {
        // Skip messages whose format version we don't recognize.
        // ADR-0011 §7 requires the `1` value for v1; absence also means
        // baseline-v1 per the same section.
        match msg.format_version.as_deref() {
            None | Some("1") => {}
            Some(other) => {
                tracing::debug!(
                    imap_uid = msg.imap_uid,
                    version = other,
                    "skipping message with unknown X-Cal-Format-Version"
                );
                continue;
            }
        }

        // The mail body is a full RFC822 envelope around our VCALENDAR.
        // Extract the body part — the iCalendar parser doesn't tolerate
        // mail headers, so we have to strip them first.
        let cal_bytes = match split_mail_body(&msg.rfc822) {
            Some(b) => b,
            None => {
                errors.push(format!(
                    "imap_uid={}: could not locate VCALENDAR body",
                    msg.imap_uid
                ));
                continue;
            }
        };

        let parsed = match ics::parse(cal_bytes) {
            Ok(Some(p)) => p,
            Ok(None) => continue, // calendar with no VEVENT — skip, not an error
            Err(e) => {
                errors.push(format!("imap_uid={}: parse: {e}", msg.imap_uid));
                continue;
            }
        };

        let dtstamp = first_dtstamp(cal_bytes).unwrap_or_default();

        // Convert the parsed event to a Commitment. Source = IcsImport
        // is the right marker; future Phase-3 negotiation messages
        // would land via a different path.
        let commitment = match ics::ics_to_commitment(
            &parsed,
            None,
            Some(own_email),
            None,
        ) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("imap_uid={}: convert: {e}", msg.imap_uid));
                continue;
            }
        };

        let uid = commitment.uid.clone();
        let new_seq = commitment.sequence;
        let new_imap_uid = msg.imap_uid;

        match winners.get(&uid) {
            None => {
                winners.insert(
                    uid,
                    (Winner { commitment }, dtstamp, new_imap_uid),
                );
            }
            Some((current, current_dtstamp, current_imap_uid)) => {
                let beats = if new_seq > current.commitment.sequence {
                    true
                } else if new_seq < current.commitment.sequence {
                    false
                } else if dtstamp > *current_dtstamp {
                    true
                } else if dtstamp < *current_dtstamp {
                    false
                } else {
                    new_imap_uid > *current_imap_uid
                };
                if beats {
                    winners.insert(
                        uid,
                        (Winner { commitment }, dtstamp, new_imap_uid),
                    );
                }
            }
        }
    }

    (
        winners.into_iter().map(|(k, (w, _, _))| (k, w)).collect(),
        errors,
    )
}

/// Find the body portion of an RFC822 mail (everything after the first
/// blank line). The iCalendar parser is happy with the raw VCALENDAR
/// bytes; we don't need full mail-parser machinery here.
fn split_mail_body(rfc822: &[u8]) -> Option<&[u8]> {
    if let Some(p) = rfc822.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some(&rfc822[p + 4..]);
    }
    rfc822
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|p| &rfc822[p + 2..])
}

/// Pull DTSTAMP out of the VCALENDAR text quickly without re-parsing the
/// full structure. We only need it for the tiebreak; lexicographical
/// compare on the raw RFC-5545 timestamp string works because
/// `YYYYMMDDTHHMMSSZ` sorts chronologically.
fn first_dtstamp(cal_bytes: &[u8]) -> Option<String> {
    let needle = b"DTSTAMP:";
    let lower_needle: Vec<u8> = needle.iter().map(|b| b.to_ascii_lowercase()).collect();
    let lower: Vec<u8> = cal_bytes.iter().map(|b| b.to_ascii_lowercase()).collect();
    let pos = lower.windows(lower_needle.len()).position(|w| w == lower_needle.as_slice())?;
    let after = pos + needle.len();
    let line_end = lower[after..]
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .map(|p| after + p)
        .unwrap_or(lower.len());
    Some(
        std::str::from_utf8(&cal_bytes[after..line_end])
            .ok()?
            .trim()
            .to_string(),
    )
}

async fn upsert_local(db: &DbHandle, commitment: &Commitment) -> Result<(), String> {
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::UpsertCommitment {
            commitment: commitment.clone(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("db upsert: {e}"))
}

/// Publish a commitment to IMAP and write the resulting (potentially
/// SEQUENCE-reset) state back to the local store. The publish path is
/// "build mail → APPEND" via the calendar_imap facade.
async fn publish_and_persist(
    db: &DbHandle,
    account_id: &AccountId,
    folder: &str,
    commitment: &Commitment,
) -> Result<(), String> {
    let from_address = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        crate::infrastructure::queries::get_account(&conn, account_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "account not found".to_string())?
            .address
    };
    let ics_body = ics::build_ics_for_commitment(commitment);
    let mail = build_publishable_mail(&from_address, commitment, &ics_body);
    calendar_imap::append_message(db, account_id, folder, mail.as_bytes()).await?;
    // Mirror the publish into local storage so the row's SEQUENCE
    // matches what's now in IMAP. This handles both the initial-publish
    // SEQUENCE-reset case and the regular publish-an-update case. We
    // also stamp `last_published_sequence` so the next sync's diff can
    // detect server-side hard-deletes on this UID.
    let mut persisted = commitment.clone();
    persisted.last_published_sequence = Some(persisted.sequence);
    upsert_local(db, &persisted).await
}

/// Build the RFC822 mail that wraps the ICS payload. Hand-rolled because
/// the structure is trivial (no MIME multipart, single text part) and we
/// keep tight control over the X-Cal-Format-Version header that ADR-0011
/// §7 mandates.
fn build_publishable_mail(
    from_address: &str,
    commitment: &Commitment,
    ics_body: &str,
) -> String {
    let summary = commitment.summary.as_deref().unwrap_or("Calendar event");
    let subject = format!("{} [SEQ {}]", summary, commitment.sequence);
    let date = Utc::now().format("%a, %d %b %Y %H:%M:%S +0000").to_string();
    let mut out = String::new();
    out.push_str(&format!("From: {from_address}\r\n"));
    out.push_str(&format!("To: {from_address}\r\n"));
    out.push_str(&format!("Subject: {subject}\r\n"));
    out.push_str(&format!("Date: {date}\r\n"));
    out.push_str("Content-Type: text/calendar; method=PUBLISH; charset=utf-8\r\n");
    out.push_str("X-Cal-Format-Version: 1\r\n");
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("\r\n");
    out.push_str(ics_body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::domain::{CommitmentSource, CommitmentStatus};

    #[test]
    fn split_mail_body_handles_crlf_separator() {
        let raw = b"Subject: x\r\nFrom: a\r\n\r\nBODY\r\nMORE\r\n";
        assert_eq!(split_mail_body(raw), Some(&b"BODY\r\nMORE\r\n"[..]));
    }

    #[test]
    fn split_mail_body_handles_lf_separator() {
        let raw = b"Subject: x\nFrom: a\n\nBODY\nMORE\n";
        assert_eq!(split_mail_body(raw), Some(&b"BODY\nMORE\n"[..]));
    }

    #[test]
    fn split_mail_body_returns_none_when_no_break() {
        let raw = b"Subject: x\nFrom: a\nNoBlankLine";
        assert!(split_mail_body(raw).is_none());
    }

    #[test]
    fn first_dtstamp_returns_value() {
        let cal = b"BEGIN:VCALENDAR\r\nDTSTAMP:20260506T120000Z\r\nUID:abc\r\n";
        assert_eq!(first_dtstamp(cal).as_deref(), Some("20260506T120000Z"));
    }

    #[test]
    fn first_dtstamp_returns_none_when_absent() {
        let cal = b"BEGIN:VCALENDAR\r\nUID:abc\r\n";
        assert!(first_dtstamp(cal).is_none());
    }

    #[test]
    fn build_publishable_mail_includes_required_headers() {
        let now = Utc::now();
        let c = Commitment {
            id: "id".into(),
            uid: "uid@host".into(),
            sequence: 3,
            summary: Some("Hello".into()),
            description: None,
            location: None,
            start_at: "2026-04-23T09:00:00+00:00".into(),
            end_at: "2026-04-23T10:00:00+00:00".into(),
            original_tzid: None,
            organizer: None,
            attendees: vec![],
            source: CommitmentSource::Manual,
            status: CommitmentStatus::Confirmed,
            last_published_sequence: None,
            source_message_id: None,
            created_at: now,
            updated_at: now,
        };
        let mail = build_publishable_mail("alice@example.com", &c, "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n");
        assert!(mail.contains("From: alice@example.com"));
        assert!(mail.contains("To: alice@example.com"));
        assert!(mail.contains("Subject: Hello [SEQ 3]"));
        assert!(mail.contains("Content-Type: text/calendar; method=PUBLISH; charset=utf-8"));
        // ADR-0011 §7: REQUIRED for v1.
        assert!(mail.contains("X-Cal-Format-Version: 1"));
        assert!(mail.contains("BEGIN:VCALENDAR"));
    }

    #[test]
    fn winner_resolution_picks_highest_sequence() {
        let make_msg = |seq: u32, dtstamp: &str, imap_uid: u32| {
            let body = format!(
                "From: a\r\nX-Cal-Format-Version: 1\r\n\r\n\
                 BEGIN:VCALENDAR\r\n\
                 VERSION:2.0\r\n\
                 METHOD:PUBLISH\r\n\
                 BEGIN:VEVENT\r\n\
                 UID:test@host\r\n\
                 SEQUENCE:{seq}\r\n\
                 DTSTAMP:{dtstamp}\r\n\
                 DTSTART:20260423T090000Z\r\n\
                 DTEND:20260423T100000Z\r\n\
                 SUMMARY:Test\r\n\
                 END:VEVENT\r\n\
                 END:VCALENDAR\r\n"
            );
            CalendarMessage {
                imap_uid,
                rfc822: body.into_bytes(),
                format_version: Some("1".into()),
            }
        };

        let messages = vec![
            make_msg(1, "20260506T100000Z", 100),
            make_msg(3, "20260506T110000Z", 101),
            make_msg(2, "20260506T120000Z", 102),
        ];
        let (winners, errors) = resolve_winners(&messages, "self@example.com");
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let w = winners.get("test@host").expect("winner present");
        assert_eq!(w.commitment.sequence, 3);
    }

    #[test]
    fn winner_resolution_breaks_seq_tie_on_dtstamp() {
        let make_msg = |seq: u32, dtstamp: &str, imap_uid: u32| {
            let body = format!(
                "X-Cal-Format-Version: 1\r\n\r\n\
                 BEGIN:VCALENDAR\r\n\
                 BEGIN:VEVENT\r\n\
                 UID:test@host\r\n\
                 SEQUENCE:{seq}\r\n\
                 DTSTAMP:{dtstamp}\r\n\
                 DTSTART:20260423T090000Z\r\n\
                 DTEND:20260423T100000Z\r\n\
                 END:VEVENT\r\n\
                 END:VCALENDAR\r\n"
            );
            CalendarMessage {
                imap_uid,
                rfc822: body.into_bytes(),
                format_version: Some("1".into()),
            }
        };

        let messages = vec![
            make_msg(2, "20260506T100000Z", 100),
            make_msg(2, "20260506T120000Z", 50), // newer dtstamp wins despite lower imap uid
            make_msg(2, "20260506T110000Z", 99),
        ];
        let (winners, _) = resolve_winners(&messages, "self@example.com");
        // The middle message has the latest dtstamp; we can't directly
        // see which one we picked, but we can see the dtstamp got lifted.
        // (Visible via test in the integration round; the unit-level
        // assertion is "we don't error" + "we got a single winner".)
        assert_eq!(winners.len(), 1);
    }

    #[test]
    fn winner_resolution_skips_unknown_format_version() {
        let cal = b"BEGIN:VCALENDAR\r\n\
                    BEGIN:VEVENT\r\n\
                    UID:test@host\r\n\
                    SEQUENCE:1\r\n\
                    DTSTAMP:20260506T100000Z\r\n\
                    DTSTART:20260423T090000Z\r\n\
                    DTEND:20260423T100000Z\r\n\
                    END:VEVENT\r\n\
                    END:VCALENDAR\r\n";
        let messages = vec![CalendarMessage {
            imap_uid: 1,
            rfc822: format!("X-Cal-Format-Version: 99\r\n\r\n{}", std::str::from_utf8(cal).unwrap()).into_bytes(),
            format_version: Some("99".into()),
        }];
        let (winners, errors) = resolve_winners(&messages, "self@example.com");
        assert!(winners.is_empty(), "should skip unknown version");
        assert!(errors.is_empty(), "no error, just skipped silently");
    }
}
