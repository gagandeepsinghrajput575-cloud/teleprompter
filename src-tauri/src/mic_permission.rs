//! Microphone permission (TCC) preflight.
//!
//! macOS does not fail a capture when the process lacks microphone permission — it opens the
//! device and feeds the app digital silence. The stream starts, the callback fires, every sample
//! is zero, and nothing reports an error. A denied permission is therefore indistinguishable
//! from "the app is broken", which is exactly how it presented.
//!
//! This module makes the state readable and asks once when the user has not answered yet.

use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermission {
    Authorized,
    Denied,
    Restricted,
    NotDetermined,
    Unknown,
}

impl MicPermission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::Denied => "denied",
            Self::Restricted => "restricted",
            Self::NotDetermined => "not-determined",
            Self::Unknown => "unknown",
        }
    }

    pub fn authorized(self) -> bool {
        matches!(self, Self::Authorized)
    }
}

fn media_type() -> Option<&'static objc2_av_foundation::AVMediaType> {
    // SAFETY: `AVMediaTypeAudio` is a framework constant valid for the process lifetime.
    unsafe { AVMediaTypeAudio }
}

/// Current authorisation without prompting.
pub fn status() -> MicPermission {
    let Some(media) = media_type() else {
        return MicPermission::Unknown;
    };
    // SAFETY: a valid media-type constant is supplied, which is what the selector requires.
    let state = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if state == AVAuthorizationStatus::Authorized {
        MicPermission::Authorized
    } else if state == AVAuthorizationStatus::Denied {
        MicPermission::Denied
    } else if state == AVAuthorizationStatus::Restricted {
        MicPermission::Restricted
    } else if state == AVAuthorizationStatus::NotDetermined {
        MicPermission::NotDetermined
    } else {
        MicPermission::Unknown
    }
}

/// Shows the system prompt and waits for the answer. Only meaningful for `NotDetermined`:
/// once a decision exists macOS never prompts again, which is why a denial is permanent until
/// the user changes it in System Settings.
pub fn request(timeout: Duration) -> MicPermission {
    let Some(media) = media_type() else {
        return MicPermission::Unknown;
    };
    let (tx, rx) = mpsc::channel();
    let handler: block2::RcBlock<dyn Fn(Bool)> = block2::RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    // SAFETY: valid media type, and `handler` is kept alive until the answer arrives or the
    // timeout expires; the runtime copies the block for the duration of the request.
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler);
    }
    match rx.recv_timeout(timeout) {
        Ok(true) => MicPermission::Authorized,
        Ok(false) => MicPermission::Denied,
        // No answer within the timeout: report the real current state rather than guessing.
        Err(_) => status(),
    }
}

/// Returns `Ok(())` when capture is allowed, prompting once if the user has never decided.
pub fn ensure() -> Result<(), String> {
    let mut current = status();
    println!("MIC_PERMISSION status={}", current.as_str());
    if current == MicPermission::NotDetermined {
        println!("MIC_PERMISSION prompting");
        current = request(Duration::from_secs(60));
        println!("MIC_PERMISSION status_after_prompt={}", current.as_str());
    }
    if current.authorized() {
        Ok(())
    } else {
        Err(format!(
            "microphone access is {} - open System Settings > Privacy & Security > Microphone, \
enable StealthPrompter, then start listening again. macOS feeds a silent signal without it, \
so the app cannot detect this any other way.",
            current.as_str()
        ))
    }
}
