#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod application;
mod commands;
mod domain;
mod infrastructure;
mod llm;
mod state;

use tauri::{Emitter, Manager};
use tracing_subscriber::EnvFilter;

/// Tauri-Event, mit dem das Frontend Compose-Window-Aufträge empfängt.
/// Payload ist ein serialisiertes `PreparedImportDraft`.
const EVT_COMPOSE_FROM_TEMPLATE: &str = "compose-from-template";

/// Fehler-Variante: Argv-Parse / Template-Read / Anhang-Stat scheitert.
/// Frontend zeigt das als Banner, sonst wäre der Aufruf für den User
/// stumm — er sähe nur das Fokus-Setup vom single-instance-Hook und
/// würde sich fragen warum kein Composer kommt.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportErrorPayload {
    message: String,
    /// Template-Pfad falls schon bekannt, sonst leer. Hilft dem User
    /// zuzuordnen, von welchem Aufruf der Fehler kam (mehrere Triggers
    /// in kurzer Folge sind selten, aber möglich).
    source_template: String,
}
const EVT_COMPOSE_FROM_TEMPLATE_ERROR: &str = "compose-from-template-error";

/// Verarbeitet einen Argv-Vektor: parst, lädt das Template, baut den
/// PreparedImportDraft. Bei Live-Trigger (App läuft schon) emittiert
/// es das Event sofort. Bei Startup-Trigger pusht es in den
/// `pending_import_drafts`-Puffer, weil der Frontend-Listener noch
/// nicht steht — Frontend zieht den Puffer beim Mount via
/// `consume_pending_import_drafts`.
fn dispatch_import_argv(app: &tauri::AppHandle, argv: &[String], live: bool) {
    let req = match application::draft_import::parse_argv(argv) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "draft-import argv parse failed");
            // Argv-Fehler heißt: User wollte importieren, hat aber
            // ein Flag falsch geschrieben (`--param` ohne `=`-Wert
            // o.ä.). Fokus geht eh aufs Fenster, also Banner anzeigen.
            if live {
                let _ = app.emit(
                    EVT_COMPOSE_FROM_TEMPLATE_ERROR,
                    ImportErrorPayload {
                        message: format!("Argv-Parse: {e}"),
                        source_template: String::new(),
                    },
                );
            }
            return;
        }
    };

    let template_for_error = req.template_path.to_string_lossy().to_string();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match application::draft_import::build_prepared_draft(&req) {
            Ok(prepared) => {
                if live {
                    if let Err(e) = app.emit(EVT_COMPOSE_FROM_TEMPLATE, &prepared) {
                        tracing::warn!(error = %e, "compose-from-template emit failed");
                    } else {
                        tracing::info!(
                            template = %prepared.source_template,
                            attachments = prepared.attachments.len(),
                            "draft-import: emitted to live frontend"
                        );
                    }
                    if let Some(win) = app.get_webview_window("main") {
                        let _ = win.set_focus();
                    }
                } else {
                    let state = app.state::<state::AppState>();
                    if let Ok(mut guard) = state.pending_import_drafts.lock() {
                        guard.push(prepared.clone());
                    }
                    tracing::info!(
                        template = %prepared.source_template,
                        attachments = prepared.attachments.len(),
                        "draft-import: queued for first frontend mount"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "draft-import build failed");
                // Live-Pfad: Frontend zeigt Banner. Cold-Start-Pfad:
                // beim Mount läuft `consume_pending_import_drafts`,
                // der Banner-Listener läuft parallel — wir emittieren
                // hier auch im Cold-Start, das Event kommt dann
                // einfach nach dem Mount an wie jedes andere.
                if let Err(emit_err) = app.emit(
                    EVT_COMPOSE_FROM_TEMPLATE_ERROR,
                    ImportErrorPayload {
                        message: e,
                        source_template: template_for_error,
                    },
                ) {
                    tracing::warn!(error = %emit_err, "compose-from-template-error emit failed");
                }
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.set_focus();
                }
            }
        }
    });
}

fn main() {
    // Default filter: our crate at info, everyone else quiet. async-imap
    // and imap-proto in particular like to dump full ENVELOPE byte arrays
    // (Date/From/Subject/Message-Id/… as a `Debug`-formatted Vec<u8>) at
    // debug level — readable only as ASCII decimal, useless for anyone
    // but a protocol debugger, and loud enough to drown the signal.
    // User can still override via `RUST_LOG=…` in the environment.
    let default_filter = "info,async_imap=warn,imap_proto=warn";
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .try_init();

    // rustls 0.23+ refuses to pick a crypto backend at runtime by default.
    // We compile with the `ring` feature; installing it once here avoids the
    // panic the first time any TLS code path runs.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Re-focus the main window when a second instance is launched.
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
            // Argv vom zweiten Aufruf kann ein Draft-Import-Trigger
            // sein (Python-Script ruft die App mit `--draft-from-template`
            // / `--draft-job` auf). Live-Variante: Frontend-Listener
            // existiert bereits, also direkt emittieren.
            dispatch_import_argv(app, &argv, true);
        }))
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .manage(state::AppState::default())
        .setup(|app| {
            // Open the DB once the app is up. The handle is stored in
            // `AppState::db` so Tauri commands can grab it from anywhere.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = init_db(&handle).await {
                    tracing::error!("db init failed: {e}");
                    return;
                }
                // DB ist hoch → Per-Konto-Background-Sync-Actors starten.
                // Jeder Account mit sync_mode=idle/polling kriegt einen
                // dauerhaften Actor, der für IDLE-Verbindungen oder
                // periodisches Polling sorgt. Muss VOR dem Prefetch
                // laufen, weil Prefetch ohnehin erst lazy bei Bedarf
                // greift und kein Race verursacht.
                let state = handle.state::<state::AppState>();
                if let Some(db) = state.db.get().cloned() {
                    if let Err(e) = application::actor::spawn_all(
                        handle.clone(),
                        db,
                        &state.actor_handles,
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "actor: startup-spawn fehlgeschlagen");
                    }
                }
                // Body-Prefetch wie bisher.
                application::prefetch::spawn_for_all_accounts(handle).await;
            });

            // Hydrate pi config from disk if the user saved one previously.
            // Failure is non-fatal — the default PiConfig remains in place.
            // Spawning on the async runtime mirrors the DB init and keeps
            // the lifetime of `app.state()` out of this synchronous setup
            // closure.
            let cfg_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(persisted) = commands::pi::load_persisted(&cfg_handle) {
                    let state = cfg_handle.state::<state::AppState>();
                    let lock_result = state.pi_config.lock();
                    if let Ok(mut guard) = lock_result {
                        *guard = persisted;
                        tracing::info!("pi_config restored from disk");
                    }
                }
            });

            // Same pattern for the workflow config (script dir +
            // python interpreter). Missing file is the common case on
            // first run — the defaults leave `RunScript` disabled
            // until the user fills in the Settings panel.
            let wf_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(persisted) = commands::workflows::load_persisted(&wf_handle) {
                    let state = wf_handle.state::<state::AppState>();
                    let lock_result = state.workflow_config.lock();
                    if let Ok(mut guard) = lock_result {
                        *guard = persisted;
                        tracing::info!("workflow_config restored from disk");
                    }
                }
            });

            // Cold-Start-Argv: wurde die App vom OS direkt mit einem
            // `--draft-from-template`-Trigger gestartet (z.B. weil der
            // User aus einem Python-Script heraus `crystalmail.exe …`
            // aufgerufen hat während die App noch nicht lief), packen
            // wir den Auftrag in den Pending-Puffer. Das Frontend
            // zieht ihn beim Compose-Listener-Mount per
            // `consume_pending_import_drafts` raus.
            let argv: Vec<String> = std::env::args().collect();
            dispatch_import_argv(&app.handle(), &argv, false);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::db_ping,
            commands::accounts::test_imap,
            commands::accounts::test_imap_verbose,
            commands::accounts::discover_folders,
            commands::accounts::add_account,
            commands::accounts::update_account,
            commands::accounts::delete_account,
            commands::accounts::list_accounts,
            commands::folders::create_folder,
            commands::folders::delete_folder,
            commands::taskbar::set_unread_badge,
            commands::mail::list_unified_folder,
            commands::mail::list_account_folders,
            commands::mail::list_folder_envelopes,
            commands::mail::search_mail,
            commands::mail::search_in_folder,
            commands::mail::search_advanced,
            commands::mail::sync_account,
            commands::mail::sync_folder_recent,
            commands::mail::sync_folder_older,
            commands::mail::sync_unified_folder_older,
            commands::mail::set_folder_sync_enabled,
            commands::mail::open_message,
            commands::mail::send_mail,
            commands::mail::save_draft,
            commands::mail::set_message_flags,
            commands::mail::save_attachment,
            commands::mail::open_attachment,
            commands::mail::get_inline_attachment_data_url,
            commands::mail::archive_message,
            commands::mail::delete_message,
            commands::mail::move_message_to,
            commands::mail::mark_as_spam,
            commands::mail::mark_messages_read,
            commands::mail::unified_unread_counts,
            commands::mail::prefetch_account_bodies,
            commands::mail::cancel_pending_fetch,
            commands::pi::get_pi_config,
            commands::pi::set_pi_config,
            commands::pi::pi_reset,
            commands::pi::pi_ask,
            commands::pi::get_ai_enabled,
            commands::pi::set_ai_enabled,
            commands::pi_models::list_pi_models,
            commands::spam::list_spam_rules,
            commands::spam::add_spam_rule,
            commands::spam::set_spam_rule_enabled,
            commands::spam::delete_spam_rule,
            commands::spam::preview_spam_rule,
            commands::spam::apply_spam_rule,
            commands::spam::suggest_spam_rules,
            commands::spam::cancel_spam_analysis,
            commands::workflows::get_workflow_config,
            commands::workflows::set_workflow_config,
            commands::workflows::list_workflows,
            commands::workflows::add_workflow,
            commands::workflows::update_workflow,
            commands::workflows::delete_workflow,
            commands::workflows::apply_workflow,
            commands::workflows::list_workflow_scripts,
            commands::workflows::analyze_python_script,
            commands::workflows::list_workflow_rules,
            commands::workflows::list_workflow_rules_for_workflow,
            commands::workflows::add_workflow_rule,
            commands::workflows::update_workflow_rule,
            commands::workflows::delete_workflow_rule,
            commands::workflows::set_workflow_rule_enabled,
            commands::workflows::apply_workflow_rule,
            commands::workflows::list_workflow_training_candidates,
            commands::workflows::list_workflow_training_ids,
            commands::workflows::is_workflow_training_candidate,
            commands::workflows::add_workflow_training,
            commands::workflows::remove_workflow_training,
            commands::workflows::clear_workflow_training,
            commands::workflows::suggest_workflow_rule,
            commands::workflows::cancel_workflow_training,
            commands::backup::export_settings,
            commands::backup::export_settings_to_path,
            commands::backup::peek_backup_file,
            commands::backup::import_settings_file,
            commands::contacts::list_address_completions,
            commands::contacts::list_contacts,
            commands::contacts::get_contact,
            commands::contacts::lookup_contact_by_email,
            commands::contacts::list_messages_for_contact,
            commands::contacts::create_contact,
            commands::contacts::update_contact,
            commands::contacts::delete_contact,
            commands::contacts::add_contact_email,
            commands::contacts::remove_contact_email,
            commands::contacts::set_primary_contact_email,
            commands::contacts::extract_contact_from_message,
            commands::contacts::list_tags,
            commands::contacts::upsert_tag,
            commands::contacts::update_tag,
            commands::contacts::delete_tag,
            commands::contacts::set_contact_tags,
            commands::contacts::export_contacts_vcf,
            commands::contacts::export_contacts_csv,
            commands::contacts::import_contacts_vcf,
            commands::contacts::import_contacts_csv,
            commands::workflows::set_workflow_rule_dry_run,
            commands::workflows::apply_workflow_rule_to_existing,
            commands::workflows::run_rule_sweep_now,
            commands::workflows::list_rule_action_log,
            commands::draft_import::prepare_draft_from_template,
            commands::draft_import::consume_pending_import_drafts,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            // Graceful shutdown beim App-Quit: jeder Per-Konto-Actor
            // bekommt `Shutdown` und seine offene IDLE-Session schickt
            // ein sauberes IMAP-LOGOUT, statt dem Server einen abrupten
            // TCP-Reset zu hinterlassen. Hard-Timeout damit ein
            // hängender Server uns nicht blockiert — 2 s reichen für
            // das einzelne LOGOUT-Roundtrip locker.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app_handle.state::<state::AppState>();
                let _ = tauri::async_runtime::block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        application::actor::shutdown_all(&state.actor_handles),
                    )
                    .await
                });
            }
        });
}

async fn init_db(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<state::AppState>();

    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let db_path = dir.join("crystalmail.sqlite");

    // SQLCipher master key aus dem OS-Keyring. Existiert noch keiner,
    // wird einer aus dem OS-RNG generiert und persistiert. Schlägt der
    // Keyring-Zugriff fehl, brechen wir hart ab — wir wollen keinen
    // stillen Plaintext-Fallback (genau das war der ursprüngliche Bug).
    let cipher_key = infrastructure::db::open_cipher_key()
        .map_err(|e| format!("db cipher key: {e}"))?;

    let handle = tokio::task::spawn_blocking({
        let db_path = db_path.clone();
        let key = cipher_key.clone();
        move || infrastructure::db::open(&db_path, &key)
    })
    .await
    .map_err(|e| format!("db-open task panicked: {e}"))?
    .map_err(|e| format!("db open: {e}"))?;

    state
        .db
        .set(handle)
        .map_err(|_| "db already initialized".to_string())?;

    tracing::info!("db initialized at {}", db_path.display());
    Ok(())
}
