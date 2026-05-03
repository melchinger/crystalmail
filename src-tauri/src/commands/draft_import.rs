// Tauri-Commands rund um den externen Draft-Import.
//
// Drei Aufrufpunkte:
//
//   * `prepare_draft_from_template`: Frontend (oder Tests) reicht
//     direkt einen Template-Pfad + Params + Anhänge ein und bekommt
//     einen fertigen `PreparedImportDraft` zurück. Genutzt vom Argv-
//     Pfad (siehe `main.rs::dispatch_import_argv`) und potenziell
//     von späterem In-App-UI ("Aus Template …").
//
//   * `consume_pending_import_drafts`: Drain-Helper. Importe, die
//     zwischen App-Start und erstem Frontend-Listen-Subscribe ankommen,
//     werden in `AppState::pending_import_drafts` gepuffert. Sobald
//     der Composer-Listener live ist, ruft das Frontend einmal diesen
//     Command auf und kriegt alle Roh-Drafts ausgeschüttet — danach
//     ist der Puffer leer und die Live-Events übernehmen.
//
//   * `parse_import_argv`: für Tests — nimmt einen Argv-Vektor und
//     liefert den geparsten Request zurück, ohne Side-Effects.

use tauri::{AppHandle, Manager};

use crate::application::draft_import::{self, PreparedImportDraft};
use crate::state::AppState;

/// Frontend-Direkt-Aufruf. Akzeptiert die drei Roh-Felder einzeln,
/// damit der Caller nicht mit der camel-cased ImportRequest-Form
/// hantieren muss.
#[tauri::command]
pub async fn prepare_draft_from_template(
    template_path: String,
    params: Option<std::collections::HashMap<String, String>>,
    attachments: Option<Vec<String>>,
) -> Result<PreparedImportDraft, String> {
    let req = draft_import::ImportRequest {
        template_path: draft_import::expand_user_path(std::path::Path::new(&template_path)),
        params: params.unwrap_or_default(),
        attachments: attachments
            .unwrap_or_default()
            .into_iter()
            .map(|s| draft_import::expand_user_path(std::path::Path::new(&s)))
            .collect(),
    };
    draft_import::build_prepared_draft(&req)
}

/// Drain-Endpunkt für Startup-Imports. Frontend ruft das einmal
/// nach dem Compose-Listener-Mount auf; danach kommen Imports nur
/// noch via Live-Event `compose-from-template` rein.
#[tauri::command]
pub async fn consume_pending_import_drafts(app: AppHandle) -> Vec<PreparedImportDraft> {
    let state = app.state::<AppState>();
    let mut guard = match state.pending_import_drafts.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    std::mem::take(&mut *guard)
}
