// Batch "mark as read" implementation. Naive approach (flags::apply per
// message) would open one IMAP session per mail — 100 mails in the
// inbox = 100 TCP+TLS handshakes + LOGIN roundtrips, minutes of delay.
//
// Instead we group the candidate ids by (account, folder), open exactly
// one session per group, and issue a single `UID STORE +FLAGS (\Seen)`
// with a comma-separated UID set. Local DB gets updated in one
// transaction per message via the writer actor — cheap by comparison.

use std::collections::HashMap;

use tokio::sync::oneshot;

use crate::domain::account::AccountId;
use crate::domain::message::{Flags, MessageId};
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::imap_client;
use crate::infrastructure::queries::{self, EnvelopeDetail};

const KEYRING_SERVICE: &str = "crystalmail";

/// Group key identifying "one IMAP session can handle these UIDs
/// together". Folders live inside an account, so the tuple is enough.
#[derive(Clone, Eq, PartialEq, Hash)]
struct GroupKey {
    account_id: AccountId,
    folder_name: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkReadReport {
    /// How many ids reached the server (IMAP STORE batches that
    /// completed without error). Messages that were already `\Seen`
    /// are still counted as "affected" — the command is idempotent.
    pub marked: u32,
    /// Failures — either the envelope couldn't be loaded, the IMAP
    /// session failed, or the DB write failed. Each group's failure
    /// is logged at warn level; the count here is the rollup.
    pub failed: u32,
    /// Total candidate count the caller passed in (so the frontend
    /// can spot "we wanted 120 but only got 118").
    pub requested: u32,
}

pub async fn mark_messages_read(
    db: &DbHandle,
    ids: Vec<MessageId>,
) -> Result<MarkReadReport, String> {
    let mut report = MarkReadReport {
        requested: ids.len() as u32,
        ..Default::default()
    };
    if ids.is_empty() {
        return Ok(report);
    }

    // Step 1: resolve every id to (account, folder_name, uid). Envelopes
    // that are already `\Seen` are dropped here — no need to ship them.
    let mut envelopes: Vec<EnvelopeDetail> = Vec::with_capacity(ids.len());
    {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        for id in &ids {
            match queries::get_envelope(&conn, id) {
                Ok(Some(env)) if !env.seen => envelopes.push(env),
                Ok(Some(_)) => {
                    // Already read locally — count as "no-op success"
                    // so the user sees the expected total in the report.
                    report.marked += 1;
                }
                Ok(None) => {
                    // Envelope vanished — probably moved/deleted while
                    // the UI was assembling the batch. Not an error
                    // worth surfacing, just a miss.
                    report.failed += 1;
                }
                Err(e) => {
                    tracing::warn!("mark_read: load envelope {id:?}: {e}");
                    report.failed += 1;
                }
            }
        }
    }

    // Step 2: group by (account, folder_name).
    let mut groups: HashMap<GroupKey, Vec<EnvelopeDetail>> = HashMap::new();
    for env in envelopes {
        let key = GroupKey {
            account_id: env.account_id,
            folder_name: env.folder_name.clone(),
        };
        groups.entry(key).or_default().push(env);
    }

    // Step 3: per-group IMAP session + batch STORE.
    for (key, group) in groups {
        // Load the account once per group.
        let account = {
            let conn = db.reads.get().map_err(|e| e.to_string())?;
            match queries::get_account(&conn, &key.account_id) {
                Ok(Some(a)) => a,
                _ => {
                    tracing::warn!(
                        "mark_read: account {:?} vanished, skipping {} mails",
                        key.account_id,
                        group.len()
                    );
                    report.failed += group.len() as u32;
                    continue;
                }
            }
        };

        let password = match keyring_password(&account.id.0.to_string()) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "mark_read: keyring for {}: {e}",
                    account.address
                );
                report.failed += group.len() as u32;
                continue;
            }
        };

        let uid_set = group
            .iter()
            .map(|e| e.imap_uid.to_string())
            .collect::<Vec<_>>()
            .join(",");

        let store_ok = run_imap_store_seen(
            &account.imap_host,
            account.imap_port,
            &account.address,
            &password,
            &key.folder_name,
            &uid_set,
        )
        .await;

        if let Err(e) = store_ok {
            tracing::warn!(
                folder = %key.folder_name,
                account = %account.address,
                "mark_read: IMAP STORE failed: {e}"
            );
            report.failed += group.len() as u32;
            continue;
        }

        // Update local DB per envelope. The writer actor serializes
        // these, so the fan-out is sequential but fast.
        for env in group {
            let desired = Flags {
                seen: true,
                answered: env.answered,
                flagged: env.flagged,
                forwarded: env.forwarded,
                junk: env.junk,
                draft: false,
                deleted: false,
            };
            let (tx, rx) = oneshot::channel();
            if db
                .writer
                .send(WriteCmd::UpdateFlags {
                    message_id: env.id,
                    flags: desired,
                    ack: tx,
                })
                .await
                .is_err()
            {
                report.failed += 1;
                continue;
            }
            match rx.await {
                Ok(Ok(())) => report.marked += 1,
                _ => report.failed += 1,
            }
        }
    }

    Ok(report)
}

fn keyring_password(account_uuid: &str) -> Result<String, String> {
    let entry_name = format!("imap::{account_uuid}");
    keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get_password: {e}"))
}

async fn run_imap_store_seen(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    folder: &str,
    uid_set: &str,
) -> Result<(), String> {
    use futures_util::StreamExt;

    let client = imap_client::connect_tls(host, port).await?;
    let mut session = client
        .login(user, password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;
    session
        .select(folder)
        .await
        .map_err(|e| format!("SELECT {folder}: {e}"))?;

    // Drain the STORE response stream inside its own scope so `stream`
    // drops before we try to reuse `session` for logout. Otherwise the
    // outstanding mutable borrow on session conflicts with the logout
    // call. Same pattern as message_ops.
    {
        let stream = session
            .uid_store(uid_set, "+FLAGS.SILENT (\\Seen)")
            .await
            .map_err(|e| format!("UID STORE +\\Seen: {e}"))?;
        tokio::pin!(stream);
        while let Some(result) = stream.next().await {
            if let Err(e) = result {
                tracing::warn!("mark_read: STORE stream: {e}");
            }
        }
    }

    let _ = session.logout().await;
    Ok(())
}
