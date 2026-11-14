//! Bare 1–9 / 0 as objection triggers (see `bare_keys.m`).
//!
//! The event tap is installed once and decides per keystroke: a digit is handled only when the
//! user armed the mode **and** a listening session is active. Everything else — including digits
//! typed while idle, and any modified key — passes through untouched.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ARMED: AtomicBool = AtomicBool::new(false);
static LISTENING: AtomicBool = AtomicBool::new(false);
static APP: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

pub fn attach(app: tauri::AppHandle) {
    if let Ok(mut guard) = APP.lock() {
        *guard = Some(app);
    }
}

pub fn is_armed() -> bool {
    ARMED.load(Ordering::SeqCst)
}

/// Only swallow digits during a live session; otherwise the keyboard behaves normally.
pub fn set_listening(value: bool) {
    LISTENING.store(value, Ordering::SeqCst);
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{APP, ARMED, LISTENING};
    use std::ffi::c_int;
    use std::sync::atomic::Ordering;
    use tauri::Emitter;

    extern "C" {
        fn tp_bare_keys_start(on_digit: extern "C" fn(c_int) -> c_int) -> c_int;
        fn tp_bare_keys_stop();
    }

    extern "C" fn on_digit(digit: c_int) -> c_int {
        if !ARMED.load(Ordering::SeqCst) {
            return 0; // pass the keystroke through untouched
        }
        if let Ok(guard) = APP.lock() {
            if let Some(app) = guard.as_ref() {
                println!("BARE_KEY objection={digit}");
                let _ = app.emit("objection", digit);
            }
        }
        // Only swallow the keystroke during an active listening session so it is not typed into the dialer.
        // When not in an active call, let it pass through while still popping up the battlecard.
        if LISTENING.load(Ordering::SeqCst) {
            1
        } else {
            0
        }
    }

    pub fn start() -> Result<(), String> {
        match unsafe { tp_bare_keys_start(on_digit) } {
            0 => {
                ARMED.store(true, Ordering::SeqCst);
                println!("BARE_KEYS_ON");
                Ok(())
            }
            2 => Err(
                "Accessibility permission is required: System Settings → Privacy & Security → \
                 Accessibility, enable StealthPrompter, then toggle again."
                    .to_string(),
            ),
            code => Err(format!("could not install the key tap (code {code})")),
        }
    }

    pub fn stop() {
        ARMED.store(false, Ordering::SeqCst);
        unsafe { tp_bare_keys_stop() };
        println!("BARE_KEYS_OFF");
    }
}

#[cfg(target_os = "macos")]
pub use imp::{start, stop};

#[cfg(not(target_os = "macos"))]
pub fn start() -> Result<(), String> {
    Err("bare digit keys are only supported on macOS".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn stop() {}
