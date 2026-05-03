// Tauri-Commands für Settings-Export / -Import. Drei Endpoints:
//
// * `export_settings` — Backend liest alles zusammen, gibt das Bundle als
//   `BackupBundle` zurück. Das Frontend serialisiert es zu JSON und ruft
//   den Tauri-Save-Dialog auf, um die Datei zu schreiben. (Wir schreiben
//   bewusst nicht im Backend, weil Save-Dialoge konsequent von der UI-Seite
//   her bedient werden — gleiche Plugin, gleicher Userflow.)
//
// * `peek_backup_file` — Liest eine Bundle-Datei vom Pfad und gibt nur die
//   `BackupPreview` zurück (Anzahlen, kein Datendump). Damit kann das UI
//   "12 Konten, 5 Spam-Regeln, 3 Workflows — importieren?" anzeigen.
//
// * `import_settings_file` — Liest die Datei, entschlüsselt Passwörter
//   (falls Passphrase mitgegeben), schreibt alles atomar in die DB,
//   rekonstruiert pi_config / workflow_config und retourniert einen
//   `ImportReport` mit Zählern und Warnungen.
//
// Wir nehmen Pfade als String entgegen — der Save-/Open-Dialog im Frontend
// liefert ohnehin Strings, und das hält die Command-Signatur portabel.

use tauri::{AppHandle, Manager};

use crate::application::backup::{
    self, BackupBundle, BackupPreview, ImportReport,
};
use crate::state::AppState;

/// Sammelt das aktuelle Settings-Bundle und gibt es als JSON-Wert
/// zurück. Wird vom Frontend nicht direkt angerufen (siehe
/// `export_settings_to_path`); existiert für künftige In-Memory-Konsumenten
/// (z.B. Backup-zu-Clipboard, Test-Roundtrips).
#[tauri::command]
pub async fn export_settings(
    app: AppHandle,
    passphrase: Option<String>,
) -> Result<BackupBundle, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let phrase: Option<&str> = passphrase
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    backup::build(&app, db, phrase).await
}

/// Bundle bauen und atomar an `path` schreiben. Der typische Userflow:
/// Frontend ruft Tauri-Save-Dialog → bekommt einen absoluten Pfad → ruft
/// dieses Command. Die UI zeigt nur "Backup erstellt" und den Pfad.
///
/// Atomares Schreiben via temp-file + rename, sodass eine abgebrochene
/// Operation niemals eine halbe Datei hinterlässt, in der der User später
/// vergeblich nach Accounts sucht.
///
/// Tracing auf jedem Schritt — wenn der User „Backup geht nicht" meldet,
/// können wir im Dev-Terminal direkt sehen, ob der Build oder das
/// Schreiben gestolpert ist (häufiger das Schreiben — verbotene Zeichen
/// im Filename, fehlende Berechtigung, abgemounteter Pfad, …).
#[tauri::command]
pub async fn export_settings_to_path(
    app: AppHandle,
    path: String,
    passphrase: Option<String>,
) -> Result<(), String> {
    tracing::info!(target: "backup", path = %path, with_phrase = passphrase.is_some(), "export start");
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or_else(|| {
        tracing::warn!(target: "backup", "export: db not ready");
        "database not ready".to_string()
    })?;
    let phrase: Option<&str> = passphrase
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let bundle = backup::build(&app, db, phrase).await.map_err(|e| {
        tracing::error!(target: "backup", error = %e, "export: bundle build failed");
        e
    })?;
    let json = serde_json::to_vec_pretty(&bundle).map_err(|e| {
        let msg = format!("serialize: {e}");
        tracing::error!(target: "backup", error = %msg, "export: JSON serialize failed");
        msg
    })?;

    let dest = std::path::PathBuf::from(&path);
    // Tmp-Datei explizit als `<filename>.part` aufm gleichen Verzeichnis —
    // `with_extension("part")` ersetzt den letzten Suffix. Bei
    // `backup-2026-05-01.json` wird das `backup-2026-05-01.part` (statt
    // `…json.part`) — auch fine, solange Quell- und Ziel-FS dasselbe sind
    // (Voraussetzung für den atomaren `rename`).
    let tmp = dest.with_extension("part");
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                let msg = format!("mkdir: {e}");
                tracing::error!(target: "backup", parent = %parent.display(), error = %msg, "export: mkdir failed");
                msg
            })?;
        }
    }
    tokio::fs::write(&tmp, &json).await.map_err(|e| {
        let msg = format!("write tmp ({}): {e}", tmp.display());
        tracing::error!(target: "backup", error = %msg, "export: write tmp failed");
        msg
    })?;
    if let Err(e) = tokio::fs::rename(&tmp, &dest).await {
        // Rename-Fehler aufräumen — sonst hinterlässt der nächste Versuch
        // einen Leichen-Tmp, weil `with_extension("part")` deterministisch
        // denselben Pfad liefert.
        let _ = tokio::fs::remove_file(&tmp).await;
        let msg = format!("rename ({} → {}): {e}", tmp.display(), dest.display());
        tracing::error!(target: "backup", error = %msg, "export: rename failed");
        return Err(msg);
    }

    tracing::info!(target: "backup", path = %dest.display(), bytes = json.len(), "export ok");
    Ok(())
}

/// Datei-Pfad → Preview. Liest und parst, entschlüsselt aber NICHT — die
/// Anzahlen sind aus den Metadaten direkt ablesbar. Anschließend ein
/// SELECT auf die existierende Konten-Tabelle, um Adress-Konflikte
/// (gleiche Email, andere UUID) im Voraus zu erkennen — die UI zeigt
/// das dem User noch vor dem Klick auf "Importieren".
///
/// Wenn die Datei nicht als Bundle parsbar ist, kommt ein verständlicher
/// Fehler zurück.
#[tauri::command]
pub async fn peek_backup_file(
    app: AppHandle,
    path: String,
) -> Result<BackupPreview, String> {
    let bundle = read_bundle(&path).await?;
    let mut preview = backup::preview(&bundle);

    // DB-Lookup für Konflikt-Erkennung. Wenn die DB noch nicht initialisiert
    // ist (sehr früh nach App-Start), liefern wir die Preview ohne Konflikt-
    // Liste — die Adress-Skip-Logik im apply() greift dann sowieso noch.
    let state = app.state::<AppState>();
    if let Some(db) = state.db.get() {
        preview.conflicting_addresses = backup::compute_conflicts(db, &bundle).await?;
    }
    Ok(preview)
}

/// Datei einlesen, atomar in DB importieren, Sidecar-Dateien schreiben,
/// Keyring-Einträge anlegen (falls Passwörter im Bundle und Passphrase
/// passt), ImportReport zurückgeben.
#[tauri::command]
pub async fn import_settings_file(
    app: AppHandle,
    path: String,
    passphrase: Option<String>,
) -> Result<ImportReport, String> {
    let state = app.state::<AppState>();
    let db = state.db.get().ok_or("database not ready")?;
    let bundle = read_bundle(&path).await?;
    let phrase: Option<&str> = passphrase
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    backup::apply(&app, db, bundle, phrase).await
}

async fn read_bundle(path: &str) -> Result<BackupBundle, String> {
    // 5 MB Limit: ein realistisches Bundle (50 Accounts mit 200 Spam-Regeln)
    // bleibt deutlich darunter. Wir schützen uns gegen versehentliches
    // Auswählen von z.B. einer Mailbox-Datei.
    const MAX_SIZE_BYTES: u64 = 5 * 1024 * 1024;
    let path_buf = std::path::PathBuf::from(path);
    let meta = tokio::fs::metadata(&path_buf)
        .await
        .map_err(|e| format!("Datei nicht lesbar: {e}"))?;
    if meta.len() > MAX_SIZE_BYTES {
        return Err(format!(
            "Datei ist {} MB groß (Limit {} MB) — vermutlich kein Backup.",
            meta.len() / (1024 * 1024),
            MAX_SIZE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = tokio::fs::read(&path_buf)
        .await
        .map_err(|e| format!("Datei lesen: {e}"))?;
    serde_json::from_slice::<BackupBundle>(&bytes)
        .map_err(|e| format!("Backup-Datei nicht gültig: {e}"))
}
