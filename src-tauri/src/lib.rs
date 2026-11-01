use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;

#[cfg(target_os = "macos")]
mod overlay_macos;

/// Click-through is a capability, not a default, until T7 adds a global escape hatch:
/// a click-through window with no global shortcut cannot be interacted with at all.
static CLICK_THROUGH: AtomicBool = AtomicBool::new(false);

fn report(win: &tauri::WebviewWindow) -> String {
    let mut line = format!(
        "WINDOW_REPORT size={:?} pos={:?} scale={:?} decorated={} always_on_top={} \
visible={} focused={} click_through={}",
        win.inner_size().map(|s| (s.width, s.height)).ok(),
        win.outer_position().map(|p| (p.x, p.y)).ok(),
        win.scale_factor().ok(),
        win.is_decorated().unwrap_or(false),
        win.is_always_on_top().unwrap_or(false),
        win.is_visible().unwrap_or(false),
        win.is_focused().unwrap_or(false),
        CLICK_THROUGH.load(Ordering::Relaxed),
    );

    #[cfg(target_os = "macos")]
    {
        use std::fmt::Write as _;
        let _ = write!(
            line,
            " sharing_type={:?} sharing_none={:?} collection_behavior={:?} \
can_join_all_spaces={:?} fullscreen_auxiliary={:?}",
            overlay_macos::sharing_type(win),
            overlay_macos::is_sharing_none(win),
            overlay_macos::collection_behavior(win),
            overlay_macos::has_can_join_all_spaces(win),
            overlay_macos::has_fullscreen_auxiliary(win),
        );
    }

    line
}

/// Applies every overlay property the product depends on, logging the readback so the
/// effect on the real `NSWindow` is observable rather than assumed.
fn apply_overlay_policy(win: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    let sharing_before = overlay_macos::sharing_type(win);

    // The single most important property in the product: a shared screen must never
    // reveal the script to the prospect.
    if let Err(e) = win.set_content_protected(true) {
        eprintln!("OVERLAY_ERROR set_content_protected: {e}");
    }

    #[cfg(target_os = "macos")]
    {
        let sharing_after = overlay_macos::sharing_type(win);
        println!(
            "CONTENT_PROTECTION sharing_before={sharing_before:?} sharing_after={sharing_after:?} \
sharing_none={:?} (NSWindowSharingNone={})",
            overlay_macos::is_sharing_none(win),
            overlay_macos::SHARING_NONE,
        );
    }

    // Float across Spaces, including a fullscreen application's Space.
    if let Err(e) = win.set_visible_on_all_workspaces(true) {
        eprintln!("OVERLAY_ERROR set_visible_on_all_workspaces: {e}");
    }
    if let Err(e) = win.set_always_on_top(true) {
        eprintln!("OVERLAY_ERROR set_always_on_top: {e}");
    }

    #[cfg(target_os = "macos")]
    {
        match overlay_macos::add_fullscreen_auxiliary(win) {
            Some(bits) => println!(
                "SPACES_POLICY collection_behavior=0x{bits:x} can_join_all_spaces={:?} \
fullscreen_auxiliary={:?}",
                overlay_macos::has_can_join_all_spaces(win),
                overlay_macos::has_fullscreen_auxiliary(win),
            ),
            None => eprintln!("OVERLAY_ERROR could not access NSWindow for Space policy"),
        }
    }

    println!("{}", report(win));
    let _ = std::io::stdout().flush();
}

#[tauri::command]
fn hud_report(window: tauri::WebviewWindow) -> String {
    report(&window)
}

#[tauri::command]
fn set_click_through(window: tauri::WebviewWindow, enabled: bool) -> Result<String, String> {
    window
        .set_ignore_cursor_events(enabled)
        .map_err(|e| e.to_string())?;
    CLICK_THROUGH.store(enabled, Ordering::Relaxed);
    Ok(report(&window))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![hud_report, set_click_through])
        .setup(|app| {
            if let Some(win) = app.get_webview_window("hud") {
                apply_overlay_policy(&win);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
