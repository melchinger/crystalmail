// Per-Konto-Actor. Pro Account läuft genau eine async Task, die je nach
// `account.sync_mode` eine langlebige IDLE-Verbindung führt, einen
// periodischen Polling-Timer fährt, oder beides parallel macht.
//
// Architektur-Entscheidung (a vs b im Conversation-Verlauf): wir machen
// den Refresh nach einem IDLE-Push **auf derselben Session**. Das
// bedeutet einmal Login + INBOX-SELECT pro Connect-Lifecycle, danach
// alle Server-Pushes ohne weiteren Login-Overhead. Bei Verbindungsabbruch
// gibt's exponentiellen Backoff und einen frischen Login.
//
// Lifecycle-Hooks (siehe `main.rs`):
//   * App-Start: spawnt Actors für alle bestehenden Accounts
//   * add_account: spawnt einen neuen Actor
//   * update_account: schickt `Updated(Account)` — Actor entscheidet
//     selbst, ob er die Verbindung neu aufbauen muss (sync_mode oder
//     IMAP-Server-Daten geändert) oder nur die Account-Daten updaten
//     (Display-Name etc.)
//   * delete_account: schickt `Shutdown`, der Actor macht graceful
//     LOGOUT und beendet sich
//   * App-Quit: alle Actors kriegen `Shutdown`
//
// Der Actor teilt mit dem User-getriggerten Sync-Pfad die Helper
// `application::sync::open_session_for_account` und
// `application::sync::fetch_recent_in_open_session` — gleicher Code-Pfad,
// kein Duplikat.

use std::time::Duration;

use tauri::AppHandle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::application::sync::{
    fetch_recent_in_open_session, open_session_for_account,
};
use crate::domain::account::{Account, AccountId};
use crate::domain::folder::{Folder, FolderId};
use crate::infrastructure::db::{DbHandle, WriteCmd};
use crate::infrastructure::queries;

/// Wie oft der Polling-Timer feuert, wenn `sync_mode` Polling enthält.
/// 2 min ist ein Kompromiss zwischen "schnell genug für aktive User"
/// und "nicht permanent IMAP-belasten". Frequenz fließt nicht in die
/// User-Settings ein — wenn jemand schneller will, soll er IDLE nutzen.
const POLL_INTERVAL: Duration = Duration::from_secs(120);

/// IDLE-Server-Timeout. Per RFC sollten Server eine 30-min-Timeout
/// einhalten; wir refreshen alle 28 min sicher davor.
const IDLE_REFRESH: Duration = Duration::from_secs(28 * 60);

/// Initial-Reconnect-Delay nach einem Verbindungsfehler. Verdoppelt sich
/// auf jedem weiteren Fehler bis zur Decke. Bei einer Wireguard-Reconnect-
/// Welle wollen wir nicht 100 Login-Versuche pro Sekunde produzieren.
const RECONNECT_INITIAL: Duration = Duration::from_secs(5);
const RECONNECT_MAX: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub enum ActorCmd {
    /// Account-Daten oder Sync-Modus haben sich geändert. Der Actor
    /// startet seinen aktiven Strategie-Loop neu, sodass der neue Modus
    /// sofort greift.
    Updated(Account),
    /// Externer Sync-Trigger. Heute nicht von außen gerufen — der UI-
    /// Refresh-Hotkey geht weiterhin über die `sync_*`-Tauri-Commands.
    /// Reserviert für künftige Erweiterungen (z.B. Auto-Sync nach
    /// gesendeter Mail über den Actor).
    #[allow(dead_code)]
    SyncNow,
    /// Graceful shutdown. Logout der IMAP-Session falls offen, dann exit.
    Shutdown,
}

#[derive(Debug)]
pub struct ActorHandle {
    pub tx: mpsc::Sender<ActorCmd>,
    /// Join-Handle der actor-Task. Beim App-Quit wartet `shutdown_all`
    /// darauf, damit die offene IDLE-Session noch ein sauberes LOGOUT
    /// rauspusten kann bevor der Prozess beendet.
    pub join: JoinHandle<()>,
}

/// Erzeugt den Per-Konto-Actor. Caller stash't das Handle in
/// `AppState::actor_handles` und schickt darüber Lifecycle-Commands.
pub fn spawn(app: AppHandle, db: DbHandle, account: Account) -> ActorHandle {
    let (tx, rx) = mpsc::channel::<ActorCmd>(8);
    let join = tokio::spawn(run_actor(app, db, account, rx));
    ActorHandle { tx, join }
}

async fn run_actor(
    app: AppHandle,
    db: DbHandle,
    initial: Account,
    mut rx: mpsc::Receiver<ActorCmd>,
) {
    tracing::info!(
        account = %initial.address,
        mode = ?initial.sync_mode,
        "actor: start"
    );
    let mut account = initial;

    'outer: loop {
        let outcome = run_strategy(&app, &db, &account, &mut rx).await;
        match outcome {
            StrategyOutcome::Updated(new) => {
                tracing::info!(
                    account = %new.address,
                    mode = ?new.sync_mode,
                    "actor: updated, restarting strategy"
                );
                account = new;
                continue 'outer;
            }
            StrategyOutcome::Shutdown => break 'outer,
        }
    }

    tracing::info!(account = %account.address, "actor: shutdown");
}

enum StrategyOutcome {
    Updated(Account),
    Shutdown,
}

/// Wählt den passenden Loop für den aktuellen `sync_mode` und delegiert.
async fn run_strategy(
    app: &AppHandle,
    db: &DbHandle,
    account: &Account,
    rx: &mut mpsc::Receiver<ActorCmd>,
) -> StrategyOutcome {
    // SyncMode hat genau drei Varianten und alle decken mindestens einen
    // Sync-Mechanismus ab — kein "off"-Modus.
    if account.sync_mode.uses_idle() && account.sync_mode.uses_polling() {
        run_idle_with_polling(app, db, account, rx).await
    } else if account.sync_mode.uses_idle() {
        run_idle_only(app, db, account, rx).await
    } else {
        run_polling_only(app, db, account, rx).await
    }
}

// ─── Strategie: nur Polling ─────────────────────────────────────────────

/// Periodischer Sync ohne langlebige Verbindung. Jeder Tick öffnet eine
/// frische Session, syncht die INBOX, schließt wieder. Wenig elegant,
/// aber bombenfest gegen Firewall-/NAT-Probleme die langlebige Sockets
/// killen.
async fn run_polling_only(
    app: &AppHandle,
    db: &DbHandle,
    account: &Account,
    rx: &mut mpsc::Receiver<ActorCmd>,
) -> StrategyOutcome {
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    // Erste tick() feuert sofort — beim Polling-Modus wollen wir das,
    // damit der User nach App-Start nicht 2 Min auf neue Mails wartet.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let _ = poll_once(app, db, account).await;
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(ActorCmd::Updated(new)) => return StrategyOutcome::Updated(new),
                    Some(ActorCmd::Shutdown) | None => return StrategyOutcome::Shutdown,
                    Some(ActorCmd::SyncNow) => {
                        let _ = poll_once(app, db, account).await;
                    }
                }
            }
        }
    }
}

/// Ein Poll-Tick: frische Session, INBOX-Folder ensure, fetch, logout.
/// Fehler werden geloggt aber nicht propagiert — der nächste Tick
/// versucht's wieder.
async fn poll_once(app: &AppHandle, db: &DbHandle, account: &Account) {
    let folder_id = match ensure_inbox_folder_id(db, &account.id).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(account = %account.address, error = %e, "poll: ensure INBOX folder failed");
            return;
        }
    };

    let (mut session, _account_name) = match open_session_for_account(db, &account.id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(account = %account.address, error = %e, "poll: open session failed");
            return;
        }
    };

    if let Err(e) = fetch_inbox_and_announce(app, db, &mut session, account, folder_id).await {
        tracing::warn!(account = %account.address, error = %e, "poll: fetch failed");
    }

    let _ = session.logout().await;
}

// ─── Strategie: nur IDLE ────────────────────────────────────────────────

/// IDLE-only. Eine persistente Session, IDLE auf INBOX, Refresh alle
/// 28 min (Server-Timeout-Schutz), Reconnect mit exponentiellem
/// Backoff bei Connection-Drops.
async fn run_idle_only(
    app: &AppHandle,
    db: &DbHandle,
    account: &Account,
    rx: &mut mpsc::Receiver<ActorCmd>,
) -> StrategyOutcome {
    let mut backoff = RECONNECT_INITIAL;
    loop {
        match idle_session_lifecycle(app, db, account, rx, None).await {
            LifecycleOutcome::Updated(new) => return StrategyOutcome::Updated(new),
            LifecycleOutcome::Shutdown => return StrategyOutcome::Shutdown,
            LifecycleOutcome::ConnectFailed | LifecycleOutcome::IdleFailed => {
                tracing::warn!(
                    account = %account.address,
                    backoff_secs = backoff.as_secs(),
                    "idle: connection failed, backing off",
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    cmd = rx.recv() => {
                        match cmd {
                            Some(ActorCmd::Updated(new)) => return StrategyOutcome::Updated(new),
                            Some(ActorCmd::Shutdown) | None => return StrategyOutcome::Shutdown,
                            Some(ActorCmd::SyncNow) => {} // restart immediately
                        }
                    }
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
            LifecycleOutcome::Restarting => {
                // 28-min refresh — kein Backoff, reconnect sofort.
                backoff = RECONNECT_INITIAL;
            }
        }
    }
}

// ─── Strategie: IDLE + Polling parallel ─────────────────────────────────

/// IDLE primär, daneben ein periodischer Poll-Timer als Sicherheitsnetz.
/// Sinnvoll für Provider, bei denen IDLE meistens funktioniert aber
/// gelegentlich hängenbleibt — der periodische Sync fängt verlorene
/// Push-Events nach.
async fn run_idle_with_polling(
    app: &AppHandle,
    db: &DbHandle,
    account: &Account,
    rx: &mut mpsc::Receiver<ActorCmd>,
) -> StrategyOutcome {
    let mut backoff = RECONNECT_INITIAL;
    let mut poll_tick = tokio::time::interval(POLL_INTERVAL);
    poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Skip the immediate first tick — IDLE wird ohnehin direkt bei
    // Lifecycle-Start einen initialen Fetch fahren.
    let _ = poll_tick.tick().await;

    loop {
        match idle_session_lifecycle(app, db, account, rx, Some(&mut poll_tick)).await {
            LifecycleOutcome::Updated(new) => return StrategyOutcome::Updated(new),
            LifecycleOutcome::Shutdown => return StrategyOutcome::Shutdown,
            LifecycleOutcome::ConnectFailed | LifecycleOutcome::IdleFailed => {
                tracing::warn!(
                    account = %account.address,
                    backoff_secs = backoff.as_secs(),
                    "idle+poll: IDLE-Verbindung fehlgeschlagen, Polling läuft weiter"
                );
                // Während des Backoffs läuft der Polling-Timer weiter — der
                // User sieht währenddessen wenigstens periodischen Sync.
                let backoff_deadline = tokio::time::sleep(backoff);
                tokio::pin!(backoff_deadline);
                loop {
                    tokio::select! {
                        _ = &mut backoff_deadline => break,
                        _ = poll_tick.tick() => {
                            let _ = poll_once(app, db, account).await;
                        }
                        cmd = rx.recv() => {
                            match cmd {
                                Some(ActorCmd::Updated(new)) => return StrategyOutcome::Updated(new),
                                Some(ActorCmd::Shutdown) | None => return StrategyOutcome::Shutdown,
                                Some(ActorCmd::SyncNow) => {
                                    let _ = poll_once(app, db, account).await;
                                }
                            }
                        }
                    }
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
            LifecycleOutcome::Restarting => {
                backoff = RECONNECT_INITIAL;
            }
        }
    }
}

// ─── Gemeinsamer IDLE-Kern ──────────────────────────────────────────────

enum LifecycleOutcome {
    /// Account wurde geändert — Caller startet Strategie neu.
    Updated(Account),
    /// Shutdown angefordert (oder Channel zu).
    Shutdown,
    /// Connection-Open / LOGIN / SELECT fehlgeschlagen — Caller macht Backoff.
    ConnectFailed,
    /// IDLE oder ein interner DONE/FETCH ging schief — Caller macht Backoff.
    IdleFailed,
    /// 28-Min-Refresh erreicht — Caller reconnects ohne Backoff.
    Restarting,
}

/// Eine komplette IDLE-Session-Lebenszeit: Connect → Select → Loop(IDLE
/// → Server-Push oder Timeout → DONE → fetch_recent → IDLE) → Logout.
///
/// Der optionale `poll_tick` wird mit-überwacht: wenn der Polling-Modus
/// parallel laufen soll, feuert der Tick einen zusätzlichen Refresh, ohne
/// dass IDLE unterbrochen werden muss (DONE/IDLE-Roundtrip mit
/// anschließendem Refetch).
async fn idle_session_lifecycle(
    app: &AppHandle,
    db: &DbHandle,
    account: &Account,
    rx: &mut mpsc::Receiver<ActorCmd>,
    mut poll_tick: Option<&mut tokio::time::Interval>,
) -> LifecycleOutcome {
    let folder_id = match ensure_inbox_folder_id(db, &account.id).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(account = %account.address, error = %e, "idle: ensure INBOX folder failed");
            return LifecycleOutcome::ConnectFailed;
        }
    };

    let (mut session, _account_name) =
        match open_session_for_account(db, &account.id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(account = %account.address, error = %e, "idle: open session failed");
                return LifecycleOutcome::ConnectFailed;
            }
        };

    // CAPABILITY-Check. Wenn der Server kein IDLE kann, geben wir auf —
    // der Caller kann (theoretisch) auf Polling switchen, aktuell loggen
    // wir nur und beenden den Actor.
    let caps = match session.capabilities().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(account = %account.address, error = %e, "idle: CAPABILITY failed");
            let _ = session.logout().await;
            return LifecycleOutcome::ConnectFailed;
        }
    };
    if !caps.has_str("IDLE") {
        tracing::warn!(
            account = %account.address,
            "idle: Server unterstützt IDLE nicht — Actor beendet sich. Account auf Polling-Modus stellen."
        );
        let _ = session.logout().await;
        return LifecycleOutcome::ConnectFailed;
    }
    drop(caps);

    // Initialer Fetch: holt alles ein, was zwischen vorigem Verbindungsende
    // und jetzt aufgelaufen ist. Erst danach gehen wir in IDLE.
    if let Err(e) = fetch_inbox_and_announce(app, db, &mut session, account, folder_id).await {
        tracing::warn!(account = %account.address, error = %e, "idle: initial fetch failed");
        let _ = session.logout().await;
        return LifecycleOutcome::IdleFailed;
    }

    // IDLE-Schleife: solange wir keine Updated/Shutdown-Command oder einen
    // 28-Min-Timeout erleben, wechseln wir zwischen "wait" und "fetch".
    loop {
        let mut handle = session.idle();
        if let Err(e) = handle.init().await {
            tracing::warn!(account = %account.address, error = %e, "idle: init failed");
            // handle.done() braucht init erfolgreich — direkter Logout
            // ist hier nicht sauber möglich, also einfach Drop.
            return LifecycleOutcome::IdleFailed;
        }

        // Drei mögliche Aufweck-Gründe: Server-Push / 28-Min-Timeout
        // (`wait_fut` resolved), Actor-Command (rx.recv) oder Polling-
        // Tick (im IdleAndPolling-Modus).
        enum Wakeup {
            ServerOrTimeout(Result<async_imap::extensions::idle::IdleResponse, async_imap::error::Error>),
            Cmd(Option<ActorCmd>),
            PollTick,
        }

        // Innerer Scope: `wait_fut` borrowed `handle` mutbar, also muss
        // die Pin GENAU hier aus dem Scope rausfallen, bevor wir `handle.done()`
        // (consume) aufrufen. Ohne den extra Block schreit der Borrow-Checker.
        let wakeup: Wakeup = {
            let (wait_fut, stop_source) = handle.wait_with_timeout(IDLE_REFRESH);
            tokio::pin!(wait_fut);

            let w = if let Some(ref mut tick) = poll_tick {
                tokio::select! {
                    res = &mut wait_fut => Wakeup::ServerOrTimeout(res),
                    cmd = rx.recv() => Wakeup::Cmd(cmd),
                    _ = tick.tick() => Wakeup::PollTick,
                }
            } else {
                tokio::select! {
                    res = &mut wait_fut => Wakeup::ServerOrTimeout(res),
                    cmd = rx.recv() => Wakeup::Cmd(cmd),
                }
            };

            // Bei Cmd / PollTick haben wir IDLE manuell unterbrochen.
            // `drop(stop_source)` bricht den IdleStream ab → wait_fut
            // resolvet zu `ManualInterrupt`. Vor dem `handle.done()` muss
            // wait_fut aber wirklich gedrained sein, sonst hängt DONE
            // auf einem ungelesenen Server-Response.
            let interrupted_via_cmd = matches!(w, Wakeup::Cmd(_) | Wakeup::PollTick);
            drop(stop_source);
            if interrupted_via_cmd {
                let _ = (&mut wait_fut).await;
            }
            w
            // wait_fut, stop_source droppen hier — handle ist wieder frei
        };

        // Beende IDLE mit DONE, kriege Session zurück.
        session = match handle.done().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(account = %account.address, error = %e, "idle: DONE failed");
                return LifecycleOutcome::IdleFailed;
            }
        };

        // Auf Server-Push oder PollTick: Refresh fahren. Bei Timeout
        // gleicher Refresh — wir wollen nichts verpasst haben während des
        // 28-Min-Fensters. Bei Cmd: handle und ggf. exit.
        match wakeup {
            Wakeup::ServerOrTimeout(Ok(async_imap::extensions::idle::IdleResponse::Timeout)) => {
                // Sauberer 28-Min-Refresh — kein Fetch nötig (im Timeout
                // ist nichts passiert per Definition), einfach reconnect.
                let _ = session.logout().await;
                return LifecycleOutcome::Restarting;
            }
            Wakeup::ServerOrTimeout(Ok(_)) | Wakeup::PollTick => {
                // Server hat was gepusht (NewData) ODER Polling-Tick
                // schießt ein Sicherheitsnetz-Fetch. In beiden Fällen
                // refetchen wir die INBOX und emiten sync-progress fürs
                // Frontend (Auto-Refresh-Listener).
                if let Err(e) =
                    fetch_inbox_and_announce(app, db, &mut session, account, folder_id).await
                {
                    tracing::warn!(account = %account.address, error = %e, "idle: post-event fetch failed");
                    let _ = session.logout().await;
                    return LifecycleOutcome::IdleFailed;
                }
            }
            Wakeup::ServerOrTimeout(Err(e)) => {
                tracing::warn!(account = %account.address, error = %e, "idle: wait failed");
                let _ = session.logout().await;
                return LifecycleOutcome::IdleFailed;
            }
            Wakeup::Cmd(Some(ActorCmd::Updated(new))) => {
                let _ = session.logout().await;
                return LifecycleOutcome::Updated(new);
            }
            Wakeup::Cmd(Some(ActorCmd::Shutdown)) | Wakeup::Cmd(None) => {
                let _ = session.logout().await;
                return LifecycleOutcome::Shutdown;
            }
            Wakeup::Cmd(Some(ActorCmd::SyncNow)) => {
                if let Err(e) =
                    fetch_inbox_and_announce(app, db, &mut session, account, folder_id).await
                {
                    tracing::warn!(account = %account.address, error = %e, "idle: SyncNow fetch failed");
                    let _ = session.logout().await;
                    return LifecycleOutcome::IdleFailed;
                }
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────

/// `fetch_recent_in_open_session` + emitting des `sync-progress`-Tauri-
/// Events danach. Das Frontend hängt seinen Auto-Refresh-Listener an
/// diesem Event auf — ohne den hier-emittierten Event sehen User
/// IDLE-/Polling-getriggerte neue Mails erst nach manuellem Refresh.
///
/// Der Tauri-Command-Pfad (`sync_folder_recent`) emittiert das Event
/// in seiner eigenen Wrapping-Funktion. Wir duplizieren die Konstruktion
/// hier bewusst statt der inneren Funktion ein "emit_after"-Argument
/// anzuhängen — die Symmetrie zwischen User-getriggertem und IDLE-
/// getriggertem Sync ist's wert klar zu zeigen.
async fn fetch_inbox_and_announce(
    app: &AppHandle,
    db: &DbHandle,
    session: &mut async_imap::Session<
        tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    >,
    account: &Account,
    folder_id: FolderId,
) -> Result<(), String> {
    let outcome = fetch_recent_in_open_session(
        app,
        db,
        session,
        &account.id,
        &account.display_name,
        folder_id,
        "INBOX",
        200,
    )
    .await?;

    use tauri::Emitter;
    let _ = app.emit(
        "sync-progress",
        crate::application::sync::SyncProgress {
            account_id: account.id.0.to_string(),
            account_name: account.display_name.clone(),
            folder: "INBOX".to_string(),
            fetched: outcome.fetched,
            total: outcome.fetched,
            done: true,
            new_in_inbox: outcome.new_in_inbox,
        },
    );

    // Body-Cache nachziehen. Symmetrie zum Tauri-Command-Pfad
    // `commands::mail::sync_account`, der nach jedem User-Sync
    // ebenfalls `prefetch::spawn` aufruft — die Actor-Pfade
    // (IDLE-Push + Polling) hatten das bisher nicht, daher musste
    // jede über IDLE eingegangene Mail beim ersten Anklicken
    // online nachgeladen werden („gefühlt muss er nun bei jeder
    // Mail nachladen"). Prefetch ist ohnehin self-debouncing über
    // `AppState::prefetch_running`, ein doppelter Trigger ist also
    // billig. Die Schwellwerte (prefetch_days, MAX_BODY_BYTES)
    // greifen wie immer.
    if outcome.new_in_inbox > 0 {
        tracing::debug!(
            account = %account.id.0,
            new = outcome.new_in_inbox,
            "actor: triggering body prefetch after IDLE/Polling sync"
        );
        crate::application::prefetch::spawn(app.clone(), account.id);
    }

    Ok(())
}

/// Stellt sicher, dass die INBOX in der lokalen `folders`-Tabelle
/// existiert, und liefert ihre `FolderId`. Bei einem fabrikneuen
/// Account, dessen erster Sync nie lief, fehlt der DB-Eintrag noch.
async fn ensure_inbox_folder_id(
    db: &DbHandle,
    account_id: &AccountId,
) -> Result<FolderId, String> {
    // Schnellster Pfad: lookup zuerst.
    {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        if let Ok(folders) = queries::list_account_folders(&conn, account_id) {
            if let Some(inbox) = folders
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case("INBOX"))
            {
                return Ok(inbox.id);
            }
        }
    }

    // Nicht in der DB → über den Writer einfügen lassen.
    let (tx, rx) = tokio::sync::oneshot::channel();
    db.writer
        .send(WriteCmd::EnsureFolder {
            account_id: *account_id,
            name: "INBOX".to_string(),
            ack: tx,
        })
        .await
        .map_err(|_| "writer channel closed".to_string())?;
    rx.await
        .map_err(|_| "writer dropped ack".to_string())?
        .map_err(|e| format!("ensure INBOX: {e}"))
}

// `Folder` is referenced via `crate::domain::folder::Folder` only for
// type clarity in the import block — it isn't constructed here, but
// keeping the use line lets future helpers reach for it without an
// extra import dance.
#[allow(dead_code)]
fn _phantom_folder() -> Option<Folder> {
    None
}

// ─── Lifecycle-Helpers ──────────────────────────────────────────────────
//
// Diese Helpers werden von `main.rs` (Startup), `commands::accounts`
// (Add/Update/Delete) und `commands::backup` (Import) aufgerufen.
// Sie halten die Actor-Map in `AppState::actor_handles` synchron mit der
// Account-Tabelle: pro Konto genau ein Actor, beim Account-Lifecycle wird
// gespawned/notify't/heruntergefahren.

use crate::domain::account::{ImapEndpoint, SmtpEndpoint};
use crate::domain::auth::AuthCredential;
use crate::infrastructure::queries::AccountSummary;
use std::collections::HashMap;
use tokio::sync::Mutex;

/// Übersetzt eine UI-Public-`AccountSummary` zurück in ein vollständiges
/// `Account`-Domain-Objekt — der Actor braucht den `credential_entry`
/// (Keyring-Pfad), der aus Sicherheitsgründen nicht in der Summary steht.
/// Das `keyring_entry`-Naming ist deterministisch (`imap::{uuid}`), also
/// konstruieren wir es lokal statt eine zweite DB-Query zu fahren.
///
/// `pub(crate)` damit `application::backup::apply` den Helper nach einem
/// erfolgreichen Import wiederverwenden kann.
pub(crate) fn account_from_summary(s: AccountSummary) -> Account {
    let keyring_entry = format!("imap::{}", s.id.0);
    Account {
        id: s.id,
        display_name: s.display_name,
        address: s.address,
        from_name: s.from_name,
        color: s.color,
        signature: s.signature,
        signature_html: s.signature_html,
        imap: ImapEndpoint {
            host: s.imap_host,
            port: s.imap_port,
            tls: s.imap_tls,
        },
        smtp: SmtpEndpoint {
            host: s.smtp_host,
            port: s.smtp_port,
            tls: s.smtp_tls,
        },
        credential: AuthCredential::Password { keyring_entry },
        archive_folder: s.archive_folder,
        sent_folder: s.sent_folder,
        drafts_folder: s.drafts_folder,
        trash_folder: s.trash_folder,
        spam_folder: s.spam_folder,
        archive_on_reply: s.archive_on_reply,
        prefetch_days: s.prefetch_days,
        sync_mode: s.sync_mode,
        server_stores_sent: s.server_stores_sent,
    }
}

/// App-Start-Hook: spawnt einen Actor pro existierendem Konto. Wird
/// nach `init_db` einmalig aus `main.rs` heraus aufgerufen.
pub async fn spawn_all(
    app: AppHandle,
    db: DbHandle,
    handles: &Mutex<HashMap<AccountId, ActorHandle>>,
) -> Result<(), String> {
    let summaries = {
        let conn = db.reads.get().map_err(|e| e.to_string())?;
        queries::list_accounts(&conn).map_err(|e| e.to_string())?
    };
    let mut map = handles.lock().await;
    for s in summaries {
        let id = s.id;
        // Wenn aus irgendeinem Grund schon ein Handle für die ID liegt
        // (z.B. doppelter Startup-Call durch Hot-Reload im Dev), erst
        // den alten herunterfahren bevor wir den neuen spawnen.
        if let Some(existing) = map.remove(&id) {
            let _ = existing.tx.send(ActorCmd::Shutdown).await;
        }
        let account = account_from_summary(s);
        let handle = spawn(app.clone(), db.clone(), account);
        map.insert(id, handle);
    }
    tracing::info!(actor_count = map.len(), "actors: startup-spawn complete");
    Ok(())
}

/// Add-Account-Hook: spawnt einen Actor für das frisch angelegte Konto.
pub async fn spawn_one(
    app: AppHandle,
    db: DbHandle,
    handles: &Mutex<HashMap<AccountId, ActorHandle>>,
    account: Account,
) {
    let mut map = handles.lock().await;
    if let Some(existing) = map.remove(&account.id) {
        // Sollte bei add_account nie passieren (frische UUID), aber für
        // den Backup-Import-Fall wo Accounts mit fester UUID reinkommen,
        // greifen wir defensive zur sauberen Übergabe.
        let _ = existing.tx.send(ActorCmd::Shutdown).await;
    }
    let id = account.id;
    let handle = spawn(app, db, account);
    map.insert(id, handle);
    tracing::info!(account_id = %id.0, "actor: spawned for new account");
}

/// Update-Account-Hook: schickt `Updated(Account)` an den existierenden
/// Actor. Der Actor entscheidet selbst, ob das Re-Connect heißt
/// (sync_mode oder IMAP-Daten haben sich geändert) oder nur Display-
/// Update der internen Felder.
pub async fn notify_updated(
    handles: &Mutex<HashMap<AccountId, ActorHandle>>,
    account: Account,
) {
    let map = handles.lock().await;
    if let Some(handle) = map.get(&account.id) {
        if handle.tx.send(ActorCmd::Updated(account)).await.is_err() {
            tracing::warn!("actor: update-channel closed (dead actor) — wird beim nächsten App-Start neu erzeugt");
        }
    } else {
        tracing::warn!("actor: update für unbekannten Account — kein Actor in der Map");
    }
}

/// Delete-Account-Hook: schickt Shutdown und entfernt das Handle.
pub async fn shutdown_one(
    handles: &Mutex<HashMap<AccountId, ActorHandle>>,
    account_id: &AccountId,
) {
    let mut map = handles.lock().await;
    if let Some(handle) = map.remove(account_id) {
        let _ = handle.tx.send(ActorCmd::Shutdown).await;
        tracing::info!(account_id = %account_id.0, "actor: shutdown");
    }
}

/// App-Quit-Hook: graceful shutdown aller Actors. Schickt `Shutdown` an
/// jeden Actor und wartet danach auf die zugehörigen JoinHandles, damit
/// die offenen IDLE-Sessions ihr LOGOUT noch sauber rausschicken können.
/// Der Caller setzt selbst ein Timeout drumrum (siehe `main.rs`), damit
/// ein hängender Server den App-Quit nicht blockiert.
pub async fn shutdown_all(handles: &Mutex<HashMap<AccountId, ActorHandle>>) {
    // Map drainen, Lock danach freigeben — wir wollen während des Wartens
    // nicht den Mutex halten (sonst könnte z.B. ein paralleler
    // shutdown_one() blockieren).
    let drained: Vec<(AccountId, ActorHandle)> = {
        let mut map = handles.lock().await;
        map.drain().collect()
    };
    let count = drained.len();
    if count == 0 {
        return;
    }
    // Erst alle Shutdown-Cmds raus, dann auf die Tasks warten — so
    // schließen die Sessions parallel statt seriell.
    let mut joins: Vec<JoinHandle<()>> = Vec::with_capacity(count);
    for (_, handle) in drained {
        let _ = handle.tx.send(ActorCmd::Shutdown).await;
        joins.push(handle.join);
    }
    tracing::info!(actor_count = count, "actors: shutdown-all sent, awaiting LOGOUT");
    for join in joins {
        let _ = join.await;
    }
    tracing::info!(actor_count = count, "actors: shutdown-all complete");
}
