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
// icon. Non-Windows platforms noop the set_overlay_icon call — the
// API is Windows-specific in Tauri 2.

use tauri::{AppHandle, Manager};

#[tauri::command]
pub async fn set_unread_badge(
    app: AppHandle,
    rgba: Option<Vec<u8>>,
    width: u32,
    height: u32,
) -> Result<(), String> {
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
