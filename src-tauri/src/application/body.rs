// On-demand body download. Called the first time the user opens a message
// whose envelope is in the store but whose body hasn't been fetched yet.
//
// Flow: connect → LOGIN → SELECT <folder> → UID FETCH BODY.PEEK[] → MIME parse
// → StoreBody. The .PEEK variant is important: plain BODY[] would mark the
// message \Seen on the server, which should only happen once the user
// actually reads it.

use futures_util::StreamExt;
use mail_parser::MessageParser;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use crate::application::attachments::{self, AttachmentMeta};
use crate::domain::message::MessageId;
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::imap_client;
use crate::infrastructure::queries::{self, EnvelopeDetail};
use crate::state::AppState;

const KEYRING_SERVICE: &str = "crystalmail";

#[derive(Debug, Clone)]
pub struct ParsedBody {
    pub plain: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<AttachmentMeta>,
}

/// Sentinel error returned when the caller (archive/delete/move) signalled
/// that the body fetch is no longer needed. Callers of `fetch_and_store`
/// can match on this to avoid surfacing it as a user-visible error.
pub const CANCELLED_ERR: &str = "cancelled";

pub async fn fetch_and_store(
    app: &AppHandle,
    db: &DbHandle,
    id: MessageId,
) -> Result<ParsedBody, String> {
    let envelope = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::get_envelope(&conn, &id)
            .map_err(|e| e.to_string())?
            .ok_or("envelope not found")?
    };

    // Load the parent account + folder name so we know where to SELECT.
    let (account, folder_name) = load_context(db, &envelope).await?;

    let entry_name = format!("imap::{}", account.id.0);
    let password = keyring::Entry::new(KEYRING_SERVICE, &entry_name)
        .map_err(|e| format!("keyring::Entry::new: {e}"))?
        .get_password()
        .map_err(|e| format!("keyring get_password: {e}"))?;

    // Register a cancel-token for this message id. Archive/delete/move
    // can fire it via `cancel_pending_fetch` to drop our session early —
    // saves bandwidth on a body we're about to throw away, and releases
    // the IMAP connection slot so the deletion op doesn't queue behind.
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    {
        let state = app.state::<AppState>();
        let lock_result = state.pending_fetch_cancels.lock();
        if let Ok(mut map) = lock_result {
            // If a prior fetch for this id is still registered (rapid
            // click-reclick), displace it — the new fetch owns the token.
            map.insert(id, cancel_tx);
        }
    }
    // Guard removes the entry regardless of how we exit.
    let _guard = CancelGuard {
        app: app.clone(),
        message_id: id,
    };
    tokio::pin!(cancel_rx);

    let client = imap_client::connect_tls(&account.imap_host, account.imap_port).await?;
    let mut session = client
        .login(&account.address, &password)
        .await
        .map_err(|(e, _)| format!("LOGIN: {e}"))?;

    // Every await below is wrapped in `select!` against the cancel
    // receiver. In the cancel branches we don't attempt `session.logout()`
    // — the stream borrow and the session borrow fight the borrow checker,
    // and letting the session drop naturally closes the TCP connection
    // just as effectively (it might not send the polite BYE, but the
    // server copes with abrupt close on idle sessions).
    tokio::select! {
        r = session.select(&folder_name) => {
            r.map_err(|e| format!("SELECT {folder_name}: {e}"))?;
        }
        _ = &mut cancel_rx => {
            return Err(CANCELLED_ERR.into());
        }
    }

    let uid = envelope.imap_uid.to_string();
    // Track whether the server gave us *any* FETCH response. Distinguishes
    // the UID-gone case (no responses at all → message was expunged or
    // moved on the server side) from the rare "responded but no BODY[]"
    // case. The two warrant different recovery behaviour: UID-gone means
    // the local DB row is stale and should be soft-deleted so the user
    // doesn't keep clicking it; "no body" with responses is a server
    // quirk we just surface.
    let mut fetch_response_count: u32 = 0;
    let raw = {
        let stream = tokio::select! {
            r = session.uid_fetch(&uid, "BODY.PEEK[]") => {
                r.map_err(|e| format!("UID FETCH: {e}"))?
            }
            _ = &mut cancel_rx => {
                return Err(CANCELLED_ERR.into());
            }
        };
        tokio::pin!(stream);

        // Drain the stream but bail on cancel at every chunk boundary.
        let mut raw: Option<Vec<u8>> = None;
        loop {
            tokio::select! {
                next = stream.next() => {
                    match next {
                        Some(Ok(fetch)) => {
                            fetch_response_count += 1;
                            if let Some(body) = fetch.body() {
                                raw = Some(body.to_vec());
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            return Err(format!("fetch stream: {e}"));
                        }
                        None => break,
                    }
                }
                _ = &mut cancel_rx => {
                    return Err(CANCELLED_ERR.into());
                }
            }
        }
        raw
    };
    let _ = session.logout().await;

    let Some(raw) = raw else {
        // Two distinct failure modes share this branch.
        if fetch_response_count == 0 {
            // Zero FETCH responses + clean stream completion = the
            // server processed the FETCH and found nothing under this
            // UID. Strong signal that the message was moved/deleted
            // server-side (another client, server-side rule, etc.).
            //
            // We don't auto-purge from here. The user has a clean,
            // explicit path now: hitting Delete or Archive on the row
            // runs through `message_ops`, which falls back to a
            // local-only cleanup when it confirms the UID is gone.
            // Auto-purging on a body-fetch failure looked tempting
            // but mixes two different concerns: a transient server
            // hiccup that *also* yields 0 FETCH responses (rare but
            // possible — a mid-fetch session reset on a busy server)
            // would silently destroy the cached envelope.
            tracing::warn!(
                account = %envelope.account_id.0,
                folder = %folder_name,
                uid = %envelope.imap_uid,
                "body fetch: UID not on server (0 FETCH responses)"
            );
            return Err(format!(
                "Mail nicht mehr unter UID {} in '{}' abrufbar - vermutlich auf einem anderen Gerät verschoben oder gelöscht. Mit Delete oder Archive entfernen, um den Eintrag aufzuräumen.",
                envelope.imap_uid, folder_name
            ));
        }
        // FETCH responses came back but none carried a BODY[] section,
        // even though we asked for `BODY.PEEK[]`. That's a server quirk
        // (or an async-imap parsing edge case), not a vanished message
        // — leave the DB row alone and let the user retry.
        tracing::warn!(
            account = %envelope.account_id.0,
            folder = %folder_name,
            uid = %envelope.imap_uid,
            responses = fetch_response_count,
            "body fetch: server returned FETCH responses but no BODY[]"
        );
        return Err(format!(
            "Server lieferte für UID {} in '{}' keinen Mail-Body (trotz {} FETCH-Antwort(en)).",
            envelope.imap_uid, folder_name, fetch_response_count
        ));
    };

    // Last chance to bail before we spend CPU on parsing + a DB write.
    // After this point we'd rather commit what we have — the user might
    // have already moved on, but the cached body is useful if they come
    // back, and the write is cheap.
    if cancel_rx.try_recv().is_ok() {
        return Err(CANCELLED_ERR.into());
    }

    let parsed = parse_body(&raw);
    // Authoritative attachment detection: walk the decoded MIME tree.
    // Inline parts (e.g. `cid:` images embedded in the HTML body) are
    // excluded so we don't misfire the paperclip on signature logos.
    let attachments = attachments::parse_metas(&raw);
    let has_attachments = attachments.iter().any(|a| !a.is_inline);

    // Persist: writer stores raw + plain + html + updates FTS body_text.
    let (tx, rx) = oneshot::channel();
    db.writer
        .send(WriteCmd::StoreBody {
            message_id: id,
            raw_rfc822: raw.clone(),
            plain_text: parsed.plain.clone(),
            html_text: parsed.html.clone(),
            has_attachments,
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed")?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("store body: {e}"))?;

    // Body is cached ⇒ every workflow-rule predicate (envelope +
    // attachments) is now resolvable. Fire the matcher async so a
    // slow rule doesn't delay the reader — the reader already has
    // the in-memory parsed body below, it's the DB cache that
    // needed the commit.
    tauri::async_runtime::spawn(super::workflow_rules::evaluate_and_trigger(
        app.clone(),
        db.clone(),
        id,
    ));

    // `raw` was already persisted via `WriteCmd::StoreBody` above, and the
    // caller (`open_message`) only consumes plain/html/attachments — no
    // need to ship the bytes back through the Tauri boundary as well.
    Ok(ParsedBody {
        plain: parsed.plain,
        html: parsed.html,
        attachments,
    })
}

struct CancelGuard {
    app: AppHandle,
    message_id: MessageId,
}
impl Drop for CancelGuard {
    fn drop(&mut self) {
        // Mirrors the pattern used in prefetch::RunningGuard — bind the
        // state lock to a named var first so it survives the `if let`.
        let state = self.app.state::<AppState>();
        let lock_result = state.pending_fetch_cancels.lock();
        if let Ok(mut map) = lock_result {
            map.remove(&self.message_id);
        }
    }
}

async fn load_context(
    db: &DbHandle,
    envelope: &EnvelopeDetail,
) -> Result<(queries::AccountSummary, String), String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    let account = queries::get_account(&conn, &envelope.account_id)
        .map_err(|e| e.to_string())?
        .ok_or("account for envelope no longer exists")?;

    let folder_name: String = conn
        .query_row(
            "SELECT name FROM folders WHERE id = ?1",
            rusqlite::params![envelope.folder_id.0.to_string()],
            |row| row.get(0),
        )
        .map_err(|e| format!("folder lookup: {e}"))?;

    Ok((account, folder_name))
}

struct ParsedParts {
    plain: Option<String>,
    html: Option<String>,
}

fn parse_body(raw: &[u8]) -> ParsedParts {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return ParsedParts {
            plain: None,
            html: None,
        };
    };
    let plain = msg.body_text(0).map(|c| c.to_string());
    let html = msg.body_html(0).map(|c| c.to_string());
    ParsedParts { plain, html }
}

/// Read-only helper: return whatever body is already cached. Used when the
/// envelope's `body_cached` flag is true to skip the IMAP round-trip.
pub fn cached(db: &DbHandle, id: &MessageId) -> Result<Option<queries::Body>, String> {
    let conn = db.reads.get().map_err(|e| e.to_string())?;
    queries::get_body(&conn, id).map_err(|e| e.to_string())
}
