//! Rust side of the ScreenCaptureKit system-audio tap (see `system_audio.m`).
//!
//! This is the *prospect's* audio: whatever this Mac is playing (the Pulse Dialer call). It feeds
//! a second Apple STT session, so the microphone stream, the script tracker and the dialer are all
//! left exactly as they were.

use std::sync::Mutex;
#[cfg(not(target_os = "macos"))]
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, serde::Serialize)]
pub struct ProspectStatus {
    pub running: bool,
    pub message: String,
}

static AUDIO_TX: Mutex<Option<UnboundedSender<Vec<i16>>>> = Mutex::new(None);
static APP: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// Remember the app handle so the Obj-C status callback can emit `prospect_status`.
pub fn attach(app: tauri::AppHandle) {
    if let Ok(mut guard) = APP.lock() {
        *guard = Some(app);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{ProspectStatus, APP, AUDIO_TX};
    use std::ffi::{c_char, CStr};
    use tauri::Emitter;
    use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

    extern "C" {
        fn tp_system_audio_start(
            on_audio: extern "C" fn(*const i16, libc::c_int),
            on_status: extern "C" fn(libc::c_int, *const c_char),
        );
        fn tp_system_audio_stop();
    }

    extern "C" fn on_audio(samples: *const i16, count: libc::c_int) {
        if samples.is_null() || count <= 0 {
            return;
        }
        static FIRST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        if FIRST.fetch_add(count as usize, std::sync::atomic::Ordering::Relaxed) == 0 {
            println!("PROSPECT_AUDIO_FIRST count={count}");
        }
        if let Ok(guard) = AUDIO_TX.lock() {
            if let Some(tx) = guard.as_ref() {
                let slice = unsafe { std::slice::from_raw_parts(samples, count as usize) };
                let _ = tx.send(slice.to_vec());
            }
        }
    }

    extern "C" fn on_status(code: libc::c_int, message: *const c_char) {
        let text = if message.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned()
        };
        println!("PROSPECT_AUDIO_STATUS code={code} \"{text}\"");
        if let Ok(guard) = APP.lock() {
            if let Some(app) = guard.as_ref() {
                let _ = app.emit(
                    "prospect_status",
                    ProspectStatus {
                        running: code == 0,
                        message: text,
                    },
                );
            }
        }
    }

    pub fn start() -> Result<UnboundedReceiver<Vec<i16>>, String> {
        let (tx, rx): (UnboundedSender<Vec<i16>>, UnboundedReceiver<Vec<i16>>) =
            mpsc::unbounded_channel();
        *AUDIO_TX.lock().map_err(|_| "audio channel poisoned")? = Some(tx);
        unsafe { tp_system_audio_start(on_audio, on_status) };
        Ok(rx)
    }

    pub fn stop() {
        if let Ok(mut guard) = AUDIO_TX.lock() {
            // Dropping the sender closes the channel, which ends the second STT loop.
            *guard = None;
        }
        unsafe { tp_system_audio_stop() };
    }
}

#[cfg(target_os = "macos")]
pub use imp::{start, stop};

#[cfg(not(target_os = "macos"))]
pub fn start() -> Result<UnboundedReceiver<Vec<i16>>, String> {
    Err("system audio capture is only supported on macOS".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn stop() {}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// Manual probe: starts real system-audio capture, prints the status line, then stops.
    /// `cargo test system_audio_probe -- --ignored --nocapture` — proves the ScreenCaptureKit
    /// path and the TCC behaviour without needing the HUD.
    #[test]
    #[ignore]
    fn system_audio_probe() {
        let mut rx = super::start().expect("start");
        std::thread::sleep(std::time::Duration::from_millis(4000));
        let mut chunks = 0usize;
        let mut samples = 0usize;
        while let Ok(chunk) = rx.try_recv() {
            chunks += 1;
            samples += chunk.len();
        }
        println!("PROSPECT_AUDIO_PROBE chunks={chunks} samples={samples}");
        super::stop();
    }
}
