// Windows taskbar overlay-icon badge. The pCloud / Teams / Outlook
// small round icon on top of the taskbar entry — drawn by Windows via
// ITaskbarList3::SetOverlayIcon, exposed through Tauri's
// `WebviewWindow::set_overlay_icon`.
//
// Raw RGBA bytes + width/height come in from the frontend (16×16, red
// circle with the unread count drawn in white). Canvas' `getImageData`
// already returns RGBA, so there's no PNG encode/decode round-trip —
// we hand the byte buffer straight to Tauri's `Image::new_owned`.
// Pulling in `fontdue` + a TTF in Rust for one glyph would be a
// disproportionate dependency cost for ~10 distinct bitmaps.
//
// `None` clears the overlay, which Windows collapses back to the base
// icon. Non-Windows platforms (macOS, Linux) have no equivalent API
// in Tauri 2 — `set_overlay_icon` ist nicht definiert; auf macOS
// gäbe es zwar Dock-Badges, das ist aber ein anderer UX-Vertrag
// (Zähler im roten Kreis am Dock-Icon, nicht klein über der
// Taskbar-Vorschau). Wir lassen den Aufruf auf Nicht-Windows
// schlicht ins Leere laufen, damit der Frontend-Caller nicht pro
// Plattform branchen muss und der Build überall durchgeht.

use tauri::AppHandle;

/// Windows-Variante: tatsächlicher Overlay-Icon-Set via Tauri.
#[cfg(target_os = "windows")]
#[tauri::command]
pub async fn set_unread_badge(
    app: AppHandle,
    rgba: Option<Vec<u8>>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    use tauri::Manager;

    let Some(window) = app.get_webview_window("main") else {
        // Main window not up yet during early startup — treat as a
        // successful no-op so the frontend's first-render invoke
        // doesn't spew an error toast.
        return Ok(());
    };

    let icon = match rgba {
        None => None,
        Some(bytes) => {
            let expected = (width as usize) * (height as usize) * 4;
            if bytes.len() != expected {
                return Err(format!(
                    "rgba buffer size mismatch: got {}, expected {} for {}x{}",
                    bytes.len(),
                    expected,
                    width,
                    height
                ));
            }
            Some(tauri::image::Image::new_owned(bytes, width, height))
        }
    };

    window
        .set_overlay_icon(icon)
        .map_err(|e| format!("set_overlay_icon: {e}"))
}

/// Non-Windows: silent no-op. Argumente kommen weiterhin im selben
/// Shape vom Frontend rein (das spart die Plattform-Branch-Logik
/// auf JS-Seite); wir verwerfen sie hier explizit.
#[cfg(not(target_os = "windows"))]
#[tauri::command]
pub async fn set_unread_badge(
    _app: AppHandle,
    _rgba: Option<Vec<u8>>,
    _width: u32,
    _height: u32,
) -> Result<(), String> {
    Ok(())
}
