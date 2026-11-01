//! macOS-specific window behaviour.
//!
//! Two properties cannot be expressed through Tauri's portable API alone:
//!  * reading back whether the OS actually accepted the capture-exclusion flag, and
//!  * joining a *fullscreen app's* Space (`alwaysOnTop` does not do that on macOS).
//!
//! `objc2` gives direct access to the underlying `NSWindow`.

use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};

/// `NSWindowSharingNone` — the window is excluded from screen capture and screen sharing.
pub const SHARING_NONE: isize = 0;
pub const BEHAVIOR_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
pub const BEHAVIOR_FULL_SCREEN_AUXILIARY: usize = 1 << 8;

fn ns_window(win: &tauri::WebviewWindow) -> Option<*mut NSWindow> {
    let ptr = win.ns_window().ok()? as *mut NSWindow;
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

/// Raw `NSWindow.sharingType`. `0` means excluded from capture.
pub fn sharing_type(win: &tauri::WebviewWindow) -> Option<isize> {
    let ptr = ns_window(win)?;
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window.
    let ns = unsafe { &*ptr };
    Some(ns.sharingType().0 as isize)
}

/// True only when the OS reports the window as hidden from capture.
pub fn is_sharing_none(win: &tauri::WebviewWindow) -> Option<bool> {
    sharing_type(win).map(|t| t == SHARING_NONE)
}

/// Raw `NSWindow.collectionBehavior` bitmask.
pub fn collection_behavior(win: &tauri::WebviewWindow) -> Option<usize> {
    let ptr = ns_window(win)?;
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window.
    let ns = unsafe { &*ptr };
    Some(ns.collectionBehavior().0)
}

pub fn has_fullscreen_auxiliary(win: &tauri::WebviewWindow) -> Option<bool> {
    collection_behavior(win).map(|b| b & BEHAVIOR_FULL_SCREEN_AUXILIARY != 0)
}

pub fn has_can_join_all_spaces(win: &tauri::WebviewWindow) -> Option<bool> {
    collection_behavior(win).map(|b| b & BEHAVIOR_CAN_JOIN_ALL_SPACES != 0)
}

/// Adds `FullScreenAuxiliary` and `CanJoinAllSpaces`. Without these, macOS keeps the
/// window on the desktop Space and a fullscreen Chrome/Meet window hides it.
pub fn add_fullscreen_auxiliary(win: &tauri::WebviewWindow) -> Option<usize> {
    let ptr = ns_window(win)?;
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window.
    let ns = unsafe { &*ptr };
    let mut behavior = ns.collectionBehavior();
    behavior |= NSWindowCollectionBehavior::FullScreenAuxiliary;
    behavior |= NSWindowCollectionBehavior::CanJoinAllSpaces;
    ns.setCollectionBehavior(behavior);
    Some(behavior.0)
}
