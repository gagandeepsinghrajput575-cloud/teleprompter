//! macOS-specific window behaviour.
//!
//! Two properties cannot be expressed through Tauri's portable API alone:
//!  * reading back whether the OS actually accepted the capture-exclusion flag, and
//!  * joining a *fullscreen app's* Space (`alwaysOnTop` does not do that on macOS).
//!
//! `objc2` gives direct access to the underlying `NSWindow`.

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindow, NSWindowCollectionBehavior,
};
use objc2_foundation::NSPoint;

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

/// `NSWindow.ignoresMouseEvents` — the OS-level truth about click-through, rather than
/// trusting that our setter call took effect.
pub fn ignores_mouse_events(win: &tauri::WebviewWindow) -> Option<bool> {
    let ptr = ns_window(win)?;
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window.
    let ns = unsafe { &*ptr };
    Some(ns.ignoresMouseEvents())
}

/// `NSWindow.canBecomeKeyWindow` — a window that cannot become key never becomes active, so
/// the first click is consumed by activation and a drag can never begin.
pub fn can_become_key_window(win: &tauri::WebviewWindow) -> Option<bool> {
    let ptr = ns_window(win)?;
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window.
    let ns = unsafe { &*ptr };
    Some(ns.canBecomeKeyWindow())
}

/// `NSWindow.isKeyWindow` — true only while this window owns keyboard focus.
pub fn is_key_window(win: &tauri::WebviewWindow) -> Option<bool> {
    let ptr = ns_window(win)?;
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window.
    let ns = unsafe { &*ptr };
    Some(ns.isKeyWindow())
}

/// Synthesises a `mouseDown` at the centre of the window and queues it with `NSApplication`.
///
/// What this proves: the window accepts the event, hit-tests to a view, and the webview
/// receives it as a `pointerdown`. What it does **not** prove: that AppKit would deliver the
/// click from the HID layer. Measured directly — with `acceptFirstMouse` both true and false,
/// while the window was not key, this event still arrived — so the injector cannot turn red for
/// the activation gate and must not be cited as evidence that `acceptFirstMouse` is what fixes
/// a swallowed first click.
///
/// `click_count` is 2 on purpose: Tauri's drag script returns early for a double-click and
/// only calls `start_dragging` for a single click, and `start_dragging` runs a nested event
/// loop that would block the main thread forever waiting for a mouse-up that never arrives.
pub fn inject_click(win: &tauri::WebviewWindow) -> String {
    let Some(mtm) = MainThreadMarker::new() else {
        return "not main thread".into();
    };
    let Some(ptr) = ns_window(win) else {
        return "no NSWindow".into();
    };
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window, and
    // the main-thread requirement is enforced by `MainThreadMarker::new()` above.
    let ns = unsafe { &*ptr };
    let frame = ns.frame();
    let point = NSPoint::new(frame.size.width / 2.0, frame.size.height / 2.0);
    let hit = ns
        .contentView()
        .map(|view| view.hitTest(point).is_some())
        .unwrap_or(false);

    // SAFETY: not required — objc2 exposes this constructor as safe; every argument is a plain
    // value and the graphics context is nil.
    let event =
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDown,
            point,
            NSEventModifierFlags::empty(),
            0.0,
            ns.windowNumber(),
            None,
            1,
            2,
            1.0,
        );
    let Some(event) = event else {
        return "event build failed".into();
    };
    // Posted rather than sent so the run loop dispatches it like a real event, rather than
    // going straight to the window. (Even so, this path does not reproduce the activation
    // swallow — see the doc comment above.)
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
    format!(
        "sent x={:.0} y={:.0} window={} is_key={} hit_test={hit}",
        point.x,
        point.y,
        ns.windowNumber(),
        ns.isKeyWindow()
    )
}

/// Posts a complete drag gesture — one `mouseDown`, several `mouseDragged`, one `mouseUp` — at
/// the centre of the window, moving right and down.
///
/// A whole gesture is required, not just a click: the frontend drag handler positions the
/// window from `pointermove` deltas, so without the moves there is nothing to observe. This is
/// only safe because the window no longer carries `data-tauri-drag-region`; while it did, a
/// single synthetic `mouseDown` called `start_dragging` and blocked the main thread in a nested
/// AppKit loop waiting for a mouse-up that never arrived.
pub fn inject_drag(win: &tauri::WebviewWindow) -> String {
    let Some(mtm) = MainThreadMarker::new() else {
        return "not main thread".into();
    };
    let Some(ptr) = ns_window(win) else {
        return "no NSWindow".into();
    };
    // SAFETY: `ns_window()` returned a non-null NSWindow owned by Tauri for this window, and
    // `MainThreadMarker::new()` above proves main-thread execution.
    let ns = unsafe { &*ptr };
    let frame = ns.frame();
    let x0 = frame.size.width / 2.0;
    let y0 = frame.size.height / 2.0;
    let app = NSApplication::sharedApplication(mtm);
    let num = ns.windowNumber();

    let post = |kind: NSEventType, dx: f64, dy: f64, count: isize| {
        let event =
            NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind,
                NSPoint::new(x0 + dx, y0 + dy),
                NSEventModifierFlags::empty(),
                0.0,
                num,
                None,
                0,
                count,
                1.0,
            );
        if let Some(event) = event {
            app.postEvent_atStart(&event, false);
        }
    };

    post(NSEventType::LeftMouseDown, 0.0, 0.0, 1);
    for step in 1..=6 {
        post(
            NSEventType::LeftMouseDragged,
            10.0 * step as f64,
            -6.0 * step as f64,
            1,
        );
    }
    post(NSEventType::LeftMouseUp, 60.0, -36.0, 1);
    format!("gesture x0={x0:.0} y0={y0:.0}")
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
