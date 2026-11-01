use std::io::Write;
use tauri::Manager;

/// Emits the real window state from inside the running app.
/// This is machine-generated evidence for the overlay contract assertions
/// (decorated / always-on-top / visible / focus), which cannot be read from
/// outside the process without macOS Accessibility or Screen Recording grants.
fn report_window(win: &tauri::WebviewWindow) {
    let size = win.inner_size().map(|s| (s.width, s.height)).ok();
    let pos = win.outer_position().map(|p| (p.x, p.y)).ok();
    let scale = win.scale_factor().ok();
    let decorated = win.is_decorated().unwrap_or(false);
    let always_on_top = win.is_always_on_top().unwrap_or(false);
    let visible = win.is_visible().unwrap_or(false);
    let focused = win.is_focused().unwrap_or(false);
    let resizable = win.is_resizable().unwrap_or(false);

    println!(
        "WINDOW_REPORT size={size:?} pos={pos:?} scale={scale:?} decorated={decorated} \
         always_on_top={always_on_top} visible={visible} focused={focused} resizable={resizable}"
    );
    let _ = std::io::stdout().flush();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            if let Some(win) = app.get_webview_window("hud") {
                report_window(&win);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
