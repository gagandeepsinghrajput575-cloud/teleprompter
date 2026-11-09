//! Apple On-Device Speech Recognition (Speech.framework + Neural Engine).
//!
//! Provides ultra-low latency (~20-50ms), 100% on-device speech-to-text without cloud
//! dependency or API costs. Ideal when paired with CrystalSound microphone AI noise
//! cancellation.

use crate::audio::TARGET_RATE;
use crate::stt::{SttEvent, Transcript};
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

extern "C" {
    fn apple_speech_permission_status() -> libc::c_int;
    fn apple_speech_request_permission(timeout_seconds: f64) -> libc::c_int;
    fn apple_speech_start(
        on_transcript: extern "C" fn(*const c_char, bool, *mut c_void),
        on_status: extern "C" fn(libc::c_int, *const c_char, *mut c_void),
        user_data: *mut c_void,
    ) -> *mut c_void;
    fn apple_speech_feed_audio(session: *mut c_void, samples: *const i16, count: usize);
    fn apple_speech_rotate_request(session: *mut c_void);
    fn apple_speech_stop(session: *mut c_void);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleSpeechPermission {
    NotDetermined = 0,
    Denied = 1,
    Restricted = 2,
    Authorized = 3,
    Unavailable = -1,
}

impl AppleSpeechPermission {
    pub fn from_i32(val: i32) -> Self {
        match val {
            0 => Self::NotDetermined,
            1 => Self::Denied,
            2 => Self::Restricted,
            3 => Self::Authorized,
            _ => Self::Unavailable,
        }
    }
}

pub fn ensure_permission() -> Result<(), String> {
    let mut status = AppleSpeechPermission::from_i32(unsafe { apple_speech_permission_status() });
    println!("APPLE_SPEECH_PERMISSION status={:?}", status);
    if status == AppleSpeechPermission::NotDetermined {
        println!("APPLE_SPEECH_PERMISSION prompting user");
        status = AppleSpeechPermission::from_i32(unsafe { apple_speech_request_permission(60.0) });
        println!("APPLE_SPEECH_PERMISSION status_after_prompt={:?}", status);
    }
    match status {
        AppleSpeechPermission::Authorized => Ok(()),
        AppleSpeechPermission::Denied => Err(
            "Speech recognition permission was denied. Please allow Speech Recognition for StealthPrompter in macOS System Settings -> Privacy & Security -> Speech Recognition."
                .to_string(),
        ),
        AppleSpeechPermission::Restricted => {
            Err("Speech recognition is restricted by parental controls or device policy.".to_string())
        }
        _ => Err("Speech recognition is not available on this device.".to_string()),
    }
}

/// Minimum RMS that counts as speech. With CrystalSound noise cancellation, true silence sits
/// below ~0.003 and active speech above ~0.02.
const SILENCE_THRESHOLD: f32 = 0.005;
/// How long the signal must stay below the threshold before an utterance is finalized. Measured
/// in *audio time*, not chunk count: live CoreAudio callbacks are ~5–20 ms, so the old
/// "7 chunks = 700 ms" assumption meant 50–150 ms of quiet — the recognizer finalized on the
/// first inter-word gap and shredded sentences into single words. 400 ms keeps the pause-to-advance
/// snappy; the tracker still refuses to complete a line until it has heard (most of) the sentence.
const SILENCE_MS_TO_FINALIZE: u64 = 400;
/// Safety valve: rotate the recognizer request even during non-stop speech so its buffer stays
/// bounded and a wedged task can always recover.
const MAX_REQUEST_SECS: u64 = 30;

/// Time-based silence detector. `observe` returns true once `need_samples` consecutive samples
/// have been below the threshold (a real pause) and resets on any speech.
struct SilenceGate {
    need_samples: u64,
    silent_samples: u64,
}

impl SilenceGate {
    fn new(rate: u32, ms: u64) -> Self {
        Self {
            need_samples: (rate as u64 * ms) / 1000,
            silent_samples: 0,
        }
    }

    fn observe(&mut self, rms: f32, samples: usize, threshold: f32) -> bool {
        if rms < threshold {
            self.silent_samples += samples as u64;
        } else {
            self.silent_samples = 0;
        }
        if self.silent_samples >= self.need_samples {
            self.silent_samples = 0;
            true
        } else {
            false
        }
    }

    fn reset(&mut self) {
        self.silent_samples = 0;
    }
}

struct InnerState {
    latest_text: Option<String>,
    last_heard: Option<Instant>,
    has_unfinalized: bool,
    /// Apple reported `isFinal` for the current request: the task is finished and must be
    /// rotated, or every further audio buffer is silently dropped.
    final_seen: bool,
}

struct SessionContext {
    event_tx: UnboundedSender<SttEvent>,
    state: Arc<Mutex<InnerState>>,
}

extern "C" fn on_transcript_cb(text_ptr: *const c_char, is_final: bool, user_data: *mut c_void) {
    if text_ptr.is_null() || user_data.is_null() {
        return;
    }
    let ctx = unsafe { &*(user_data as *const SessionContext) };
    let text = unsafe {
        CStr::from_ptr(text_ptr)
            .to_string_lossy()
            .trim()
            .to_string()
    };
    if text.is_empty() {
        return;
    }

    {
        let mut st = ctx.state.lock().unwrap();
        st.latest_text = Some(text.clone());
        st.last_heard = Some(Instant::now());
        st.has_unfinalized = !is_final;
        if is_final {
            st.final_seen = true;
        }
    }

    let _ = ctx.event_tx.send(SttEvent::Transcript(Transcript {
        text,
        is_final,
        speech_final: is_final,
    }));
}

extern "C" fn on_status_cb(code: libc::c_int, msg_ptr: *const c_char, user_data: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    let ctx = unsafe { &*(user_data as *const SessionContext) };
    let msg = if !msg_ptr.is_null() {
        unsafe { CStr::from_ptr(msg_ptr).to_string_lossy().into_owned() }
    } else {
        String::new()
    };

    if code == 0 {
        println!("APPLE_SPEECH_STATUS ok: {msg}");
        let _ = ctx.event_tx.send(SttEvent::Connected);
    } else {
        eprintln!("APPLE_SPEECH_STATUS error {code}: {msg}");
        let _ = ctx.event_tx.send(SttEvent::Error(msg));
    }
}

pub async fn run(
    mut audio_rx: UnboundedReceiver<Vec<i16>>,
    event_tx: UnboundedSender<SttEvent>,
    stop_flag: Arc<AtomicBool>,
) {
    if let Err(err) = ensure_permission() {
        eprintln!("APPLE_SPEECH_PERM_FAIL {err}");
        let _ = event_tx.send(SttEvent::Error(err));
        return;
    }

    let state = Arc::new(Mutex::new(InnerState {
        latest_text: None,
        last_heard: None,
        has_unfinalized: false,
        final_seen: false,
    }));

    let ctx = Box::new(SessionContext {
        event_tx: event_tx.clone(),
        state: state.clone(),
    });
    let ctx_addr = Box::into_raw(ctx) as usize;

    let session =
        unsafe { apple_speech_start(on_transcript_cb, on_status_cb, ctx_addr as *mut c_void) };

    if session.is_null() {
        let err = "Failed to initialize Apple Speech recognition session".to_string();
        eprintln!("APPLE_SPEECH_INIT_FAIL {err}");
        let _ = event_tx.send(SttEvent::Error(err));
        unsafe {
            drop(Box::from_raw(ctx_addr as *mut SessionContext));
        }
        return;
    }

    let session_addr = session as usize;

    println!("APPLE_SPEECH_ACTIVE engine=on-device");
    let _ = event_tx.send(SttEvent::Connected);

    let mut silence = SilenceGate::new(TARGET_RATE, SILENCE_MS_TO_FINALIZE);
    let mut request_started = Instant::now();

    while !stop_flag.load(Ordering::Relaxed) {
        tokio::select! {
            maybe_chunk = audio_rx.recv() => {
                match maybe_chunk {
                    Some(chunk) => {
                        if chunk.is_empty() {
                            continue;
                        }

                        // Feed raw 16kHz PCM to Apple Speech
                        unsafe {
                            apple_speech_feed_audio(session_addr as *mut c_void, chunk.as_ptr(), chunk.len());
                        }

                        // RMS of this chunk (chunk length varies with the device callback size).
                        let sum: f32 = chunk.iter().map(|&s| {
                            let f = s as f32 / 32768.0;
                            f * f
                        }).sum();
                        let rms = (sum / chunk.len() as f32).sqrt();
                        let pause = silence.observe(rms, chunk.len(), SILENCE_THRESHOLD);

                        // Apple may end its own task (isFinal). If we do not rotate then, the
                        // finished task silently drops every subsequent audio buffer and the
                        // whole pipeline appears frozen.
                        let apple_final = {
                            let mut st = state.lock().unwrap();
                            let done = st.final_seen;
                            st.final_seen = false;
                            done
                        };
                        let mut rotate = apple_final;

                        // Real pause (>=700ms of true silence): finalize whatever was said.
                        if pause {
                            let text = {
                                let mut st = state.lock().unwrap();
                                if st.has_unfinalized {
                                    let t = st.latest_text.take();
                                    st.has_unfinalized = false;
                                    t
                                } else {
                                    None
                                }
                            };
                            if let Some(text) = text {
                                println!("APPLE_SPEECH_ENDPOINTING finalize=\"{text}\"");
                                let _ = event_tx.send(SttEvent::Transcript(Transcript {
                                    text,
                                    is_final: true,
                                    speech_final: true,
                                }));
                                rotate = true;
                            }
                        }

                        // Bounded buffer even during non-stop speech.
                        if request_started.elapsed() >= Duration::from_secs(MAX_REQUEST_SECS) {
                            let text = {
                                let mut st = state.lock().unwrap();
                                if st.has_unfinalized {
                                    let t = st.latest_text.take();
                                    st.has_unfinalized = false;
                                    t
                                } else {
                                    None
                                }
                            };
                            if let Some(text) = text {
                                println!("APPLE_SPEECH_ENDPOINTING finalize=\"{text}\" reason=max-duration");
                                let _ = event_tx.send(SttEvent::Transcript(Transcript {
                                    text,
                                    is_final: true,
                                    speech_final: true,
                                }));
                            }
                            rotate = true;
                        }

                        if rotate {
                            unsafe {
                                apple_speech_rotate_request(session_addr as *mut c_void);
                            }
                            silence.reset();
                            request_started = Instant::now();
                        }
                    }
                    None => {
                        // Audio producer closed
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                // Keep select responsive
            }
        }
    }

    println!("APPLE_SPEECH_STOPPING");
    unsafe {
        apple_speech_stop(session_addr as *mut c_void);
        drop(Box::from_raw(ctx_addr as *mut SessionContext));
    }
    println!("APPLE_SPEECH_STOPPED");
}

#[cfg(test)]
mod tests {
    use super::{SilenceGate, SILENCE_MS_TO_FINALIZE};
    use crate::audio::TARGET_RATE;

    const THRESHOLD: f32 = 0.005;

    fn gate() -> SilenceGate {
        SilenceGate::new(TARGET_RATE, SILENCE_MS_TO_FINALIZE)
    }

    #[test]
    fn endpointing_waits_for_the_configured_duration_not_a_chunk_count() {
        // 10 ms chunks: fewer than the configured duration must not finalize, however many
        // "chunks" that is. This is the regression guard for the old chunk-count bug.
        let mut g = gate();
        let chunks_for_window = (SILENCE_MS_TO_FINALIZE / 10) as usize;
        for _ in 0..chunks_for_window.saturating_sub(1) {
            assert!(
                !g.observe(0.0, 160, THRESHOLD),
                "finalized before {SILENCE_MS_TO_FINALIZE}ms of silence had elapsed"
            );
        }
        let mut fired = false;
        for _ in 0..4 {
            if g.observe(0.0, 160, THRESHOLD) {
                fired = true;
            }
        }
        assert!(
            fired,
            "must finalize once {SILENCE_MS_TO_FINALIZE}ms of true silence accumulates"
        );
    }

    #[test]
    fn a_single_short_gap_never_finalizes() {
        // A normal inter-word gap (~70 ms) is far below the window and must not end the utterance.
        let mut g = gate();
        for _ in 0..7 {
            assert!(
                !g.observe(0.0, 160, THRESHOLD),
                "a ~70ms gap must not end the utterance"
            );
        }
    }

    #[test]
    fn speech_resets_the_silence_window() {
        let mut g = gate();
        let chunks_for_window = (SILENCE_MS_TO_FINALIZE / 10) as usize;
        for _ in 0..chunks_for_window.saturating_sub(1) {
            let _ = g.observe(0.0, 160, THRESHOLD);
        }
        // A loud chunk resets the timer, so the next silent chunk must not finalize.
        assert!(!g.observe(0.5, 160, THRESHOLD));
        assert!(
            !g.observe(0.0, 160, THRESHOLD),
            "speech must reset the pause timer"
        );
    }
}
