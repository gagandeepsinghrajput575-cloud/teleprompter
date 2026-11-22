use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::sync::mpsc;

#[cfg(target_os = "macos")]
mod apple_stt;
mod audio;
mod bare_keys;
mod keys;
pub mod llm_gate;
mod lead;
#[cfg(target_os = "macos")]
mod mic_permission;
mod objection_detect;
mod stt;
mod system_audio;
mod tracker;

#[cfg(target_os = "macos")]
mod overlay_macos;

mod context_server;

#[cfg(test)]
mod pipeline_stress_tests;

use tracker::Tracker;

/// Click-through is OFF at launch: a HUD that starts transparent to input looks broken,
/// because nothing in it can be clicked and the grab bar is not even rendered. Locking is
/// always an explicit act (`Cmd+Shift+E`, or the menu bar item) taken before a call.
static CLICK_THROUGH: AtomicBool = AtomicBool::new(false);

/// Set once, so the log shows the single click that proves pointer events reach the webview.
static POINTER_SEEN: AtomicBool = AtomicBool::new(false);

/// Held so the lock state can be shown in the menu bar. Once the HUD is click-through there
/// is no on-screen affordance left to tell you it is locked.
static TRAY: Mutex<Option<tauri::tray::TrayIcon>> = Mutex::new(None);

/// True while the HUD is not on the Space the user is looking at — i.e. they are in another
/// app's native fullscreen Space, where macOS will not show us.
static OFF_ACTIVE_SPACE: AtomicBool = AtomicBool::new(false);

/// Mirrors the current input mode into the menu bar title and tooltip.
fn refresh_tray() {
    let locked = CLICK_THROUGH.load(Ordering::Relaxed);
    let off_space = OFF_ACTIVE_SPACE.load(Ordering::Relaxed);
    if let Ok(guard) = TRAY.lock() {
        if let Some(tray) = guard.as_ref() {
            let _ = tray.set_title(Some(match (locked, off_space) {
                (true, false) => "🔒",
                (false, false) => "🔓",
                // macOS does not allow the HUD into another app's fullscreen Space, so say so
                // rather than leaving the window silently missing.
                (true, true) => "🔒⚠️",
                (false, true) => "🔓⚠️",
            }));
            let _ = tray.set_tooltip(Some(if off_space {
                "StealthPrompter - hidden: another app is fullscreen (use a maximised window)"
            } else if locked {
                "StealthPrompter - locked, clicks pass through"
            } else {
                "StealthPrompter - unlocked, clickable"
            }));
            println!("TRAY_STATE locked={locked} off_active_space={off_space}");
        }
    }
}

/// Cmd+Shift on every platform this app targets (macOS only today).
/// Modifiers for every global shortcut: Cmd+Shift+Option.
///
/// The extra Option is deliberate. `Cmd+Shift+E` was used first and it is VS Code's file-explorer
/// shortcut; because these are registered *globally* the app silently stole that combination and
/// switched the HUD into click-through, which looks exactly like a broken app: clicks fall through
/// to the window behind, and the user cannot drag or press Start. Three modifiers make an
/// accidental collision far less likely.
fn cmd_shift_opt() -> Modifiers {
    Modifiers::SHIFT | Modifiers::SUPER | Modifiers::ALT
}

/// Extension trait to gracefully recover from poisoned mutexes without panicking.
pub trait MutexExt<T> {
    fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for std::sync::Mutex<T> {
    fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T> {
        match self.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                eprintln!("[WARN] Recovered from poisoned mutex");
                poisoned.into_inner()
            }
        }
    }
}

/// Archived / deleted cold call objection indices. Pinned to match TypeScript's COLD_DELETED_INDICES.
pub const COLD_DELETED_INDICES: &[i32] = &[3, 6, 8];

/// Live session state, owned by Tauri and shared with the async event loop.
struct AppState {
    tracker: Arc<Mutex<Tracker>>,
    /// Monotonic counter incremented on every manual jump, hotkey nudge, re-anchor, or script
    /// switch. Prevents an in-flight Stage 2 LLM response from mutating a re-anchored tracker.
    tracker_epoch: Arc<AtomicU64>,
    call_type: Arc<Mutex<String>>,
    objection_lock: Arc<Mutex<Option<std::time::Instant>>>,
    /// RMS of the most recent audio block, stored as f32 bits.
    level: Arc<AtomicU32>,
    /// Stop flag for the *current* session only. Sharing one flag meant a stop followed by
    /// a quick restart reset it before the old producer observed it.
    stop: Mutex<Option<Arc<AtomicBool>>>,
    audio_tx: Mutex<Option<mpsc::UnboundedSender<Vec<i16>>>>,
    http: reqwest::Client,
}

/// Latest window placement, written to disk by a debounce thread. A process-wide `static`
/// avoids threading a borrow of the app state through window-event closures.
static GEOMETRY: Mutex<Option<Geometry>> = Mutex::new(None);

#[derive(Clone, serde::Serialize)]
struct ActiveBlock {
    id: i32,
    phase: String,
    label: String,
    body: String,
    reason: String,
}

#[derive(Clone, serde::Serialize)]
struct StatusEvent {
    running: bool,
    message: String,
}

#[derive(serde::Serialize)]
struct SessionInfo {
    device: audio::CaptureStats,
    engine: String,
    deepgram: bool,
    jev: bool,
}

#[derive(serde::Serialize)]
struct HudState {
    edit_mode: bool,
}

/// Persisted window placement. Physical pixels, so it survives a relaunch unchanged.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Geometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn geometry_file(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("window.json"))
}

fn read_geometry(app: &tauri::AppHandle) -> Option<Geometry> {
    let raw = std::fs::read_to_string(geometry_file(app)?).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_geometry(app: &tauri::AppHandle, geometry: Geometry) {
    if let Some(path) = geometry_file(app) {
        if let Ok(raw) = serde_json::to_string_pretty(&geometry) {
            if let Err(err) = std::fs::write(&path, raw) {
                eprintln!("GEOMETRY_WRITE_ERROR {err}");
            }
        }
    }
}

/// Records the latest placement. Uses the process-wide slot so no borrow of `AppState`
/// has to outlive a mutex guard.
fn record_geometry(geometry: Geometry) {
    match GEOMETRY.lock() {
        Ok(mut guard) => *guard = Some(geometry),
        Err(err) => eprintln!("GEOMETRY_LOCK_ERROR {err}"),
    }
}

fn take_geometry() -> Option<Geometry> {
    GEOMETRY.lock().ok().and_then(|mut guard| guard.take())
}

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
        let _ = write!(
            line,
            " ignores_mouse_events={:?} key_window={:?} can_become_key={:?}",
            overlay_macos::ignores_mouse_events(win),
            overlay_macos::is_key_window(win),
            overlay_macos::can_become_key_window(win),
        );
        let _ = write!(
            line,
            " level={:?} hides_on_deactivate={:?} occlusion={:?}",
            overlay_macos::window_level(win),
            overlay_macos::hides_on_deactivate(win),
            overlay_macos::occlusion_state(win),
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
    // Started unlocked so the HUD is usable immediately; `Cmd+Shift+E` locks it to
    // click-through before a call.
    if let Err(e) = win.set_ignore_cursor_events(CLICK_THROUGH.load(Ordering::Relaxed)) {
        eprintln!("OVERLAY_ERROR set_ignore_cursor_events: {e}");
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

        // Order matters: `alwaysOnTop` above only reaches floating level 3, so the level is
        // raised afterwards. Without this, switching to Chrome can bury the HUD.
        match overlay_macos::raise_above_apps(win) {
            Some((before, after, hides)) => {
                println!("OVERLAY_LEVEL before={before} after={after} hides_on_deactivate={hides}")
            }
            None => eprintln!("OVERLAY_ERROR could not raise window level"),
        }
    }

    println!("{INPUT_POLICY}");
    println!("{}", report(win));
    let _ = std::io::stdout().flush();
}

/// Logged next to the measured window state so the input contract is visible. `acceptFirstMouse`
/// is set because the HUD is normally not the key window and AppKit documents the first click
/// on such a window as activation-only unless the view accepts it first. That behaviour could
/// not be reproduced here (see `overlay_macos::inject_click`), so this is a defensive setting,
/// not a measured fix; the measured fixes are `start_unlocked` and the whole-card drag region.
const INPUT_POLICY: &str =
    "INPUT_POLICY accept_first_mouse=true start_unlocked=true (lock with Cmd+Shift+E)";

#[tauri::command]
fn hud_report(window: tauri::WebviewWindow) -> String {
    report(&window)
}

#[tauri::command]
fn set_click_through(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    enabled: bool,
) -> Result<String, String> {
    window
        .set_ignore_cursor_events(enabled)
        .map_err(|e| e.to_string())?;
    CLICK_THROUGH.store(enabled, Ordering::Relaxed);
    refresh_tray();
    let _ = app.emit("edit_mode", !enabled);
    Ok(report(&window))
}

/// Records what the frontend's drag handler did. Dragging used to fail silently, so the
/// outcome is logged rather than trusted.
#[tauri::command]
fn drag_probe(mode: String, detail: String) {
    println!("DRAG_RESULT mode={mode} detail={detail}");
    let _ = std::io::stdout().flush();
}

/// `open` target for the Microphone pane in System Settings.
const MIC_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

/// `open` target for the Speech Recognition pane in System Settings.
const SPEECH_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition";

/// `open` target for Screen & System Audio Recording, which the prospect tap needs.
const SCREEN_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// `open` target for Accessibility, which the bare 1–0 key tap needs.
const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// Sends the user straight to the permission they need. A denial is permanent until they change
/// it there, so telling them without a way to act on it is not enough.
#[tauri::command]
fn open_mic_settings() {
    match std::process::Command::new("open")
        .arg(MIC_SETTINGS_URL)
        .status()
    {
        Ok(_) => println!("MIC_PERMISSION opened settings"),
        Err(err) => eprintln!("MIC_PERMISSION_ERROR cannot open settings: {err}"),
    }
}

#[tauri::command]
fn open_speech_settings() {
    match std::process::Command::new("open")
        .arg(SPEECH_SETTINGS_URL)
        .status()
    {
        Ok(_) => println!("SPEECH_PERMISSION opened settings"),
        Err(err) => eprintln!("SPEECH_PERMISSION_ERROR cannot open settings: {err}"),
    }
}

#[tauri::command]
fn open_screen_settings() {
    match std::process::Command::new("open")
        .arg(SCREEN_SETTINGS_URL)
        .status()
    {
        Ok(_) => println!("SCREEN_PERMISSION opened settings"),
        Err(err) => eprintln!("SCREEN_PERMISSION_ERROR cannot open settings: {err}"),
    }
}

#[tauri::command]
fn open_accessibility_settings() {
    match std::process::Command::new("open")
        .arg(ACCESSIBILITY_SETTINGS_URL)
        .status()
    {
        Ok(_) => println!("ACCESSIBILITY_PERMISSION opened settings"),
        Err(err) => eprintln!("ACCESSIBILITY_PERMISSION_ERROR cannot open settings: {err}"),
    }
}

#[tauri::command]
fn hud_state() -> HudState {
    HudState {
        edit_mode: !CLICK_THROUGH.load(Ordering::Relaxed),
    }
}

/// Proves a real pointer event reached the webview. The `ignoresMouseEvents` flag only says
/// the window accepts clicks; it says nothing about whether they are delivered, which is a
/// separate gate (`acceptsFirstMouse`). This is the only end-to-end evidence available
/// without synthesising input, which needs Accessibility permission we do not have.
#[tauri::command]
fn input_probe(kind: String) {
    let first = !POINTER_SEEN.swap(true, Ordering::Relaxed);
    println!("INPUT_PROBE kind={kind} first={first}");
    let _ = std::io::stdout().flush();
}

fn emit_active(app: &tauri::AppHandle, tracker: &Arc<Mutex<Tracker>>, id: i32, reason: &str) {
    let guard = tracker.lock_or_recover();
    if let Some(line) = guard.lines().iter().find(|l| l.id == id) {
        println!(
            "ACTIVE_LINE id={} label='{}' reason={reason}",
            line.id, line.label
        );
        let _ = app.emit(
            "active_block",
            ActiveBlock {
                id: line.id,
                phase: line.phase.clone(),
                label: line.label.clone(),
                body: line.body.clone(),
                reason: reason.to_string(),
            },
        );
    }
}

fn emit_status(app: &tauri::AppHandle, running: bool, message: impl Into<String>) {
    let message = message.into();
    println!("SESSION_STATUS running={running} {message}");
    let _ = app.emit("status", StatusEvent { running, message });
}

/// cpal's `Stream` is not `Send`, so it cannot live in Tauri's shared state. A dedicated
/// thread owns it and parks until the stop flag is raised.
fn spawn_capture(
    device: Option<String>,
    tx: mpsc::UnboundedSender<Vec<i16>>,
    level: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
) -> Result<audio::CaptureStats, String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("audio-capture".into())
        .spawn(move || match audio::start(device.as_deref(), tx, level) {
            Ok(capture) => {
                let stats = capture.stats.clone();
                let _ = ready_tx.send(Ok(stats));
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                drop(capture);
                println!("AUDIO_STOPPED");
            }
            Err(err) => {
                let _ = ready_tx.send(Err(err));
            }
        })
        .map_err(|e| format!("cannot spawn capture thread: {e}"))?;

    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| format!("audio start did not report back: {e}"))?
}

/// Duration during which a finalized or interim transcript matching `last_completed` is treated
/// as a delayed echo from the line we just left.
const STALE_ECHO_WINDOW: Duration = Duration::from_millis(2500);

/// Signed step distance between two line IDs in the currently loaded script order.
/// Uses index arithmetic rather than raw `to_id - from_id` so non-contiguous scripts
/// (e.g., Cold Call ID 1 -> ID 3) still measure as step +1.
pub(crate) fn line_step(lines: &[tracker::ScriptLine], from_id: i32, to_id: i32) -> Option<isize> {
    tracker::line_step_in_slice(lines, from_id, to_id)
}

/// Snapshot produced by `ingest_transcript` when a classification cycle should run.
pub(crate) struct CyclePlan {
    pub local: Option<(i32, bool, f64)>,
    pub buffer: String,
    pub active: i32,
    pub candidates: Vec<tracker::ScriptLine>,
}

pub(crate) enum IngestOutcome {
    Committed(i32, &'static str),
    RunCycle(CyclePlan),
    Idle,
}

/// Strips the delayed-echo prefix of `last_completed` (within `STALE_ECHO_WINDOW`) while
/// preserving any unconsumed run-on tail belonging to the active line.
/// Returns `None` if the entire payload was a pure echo with no remaining content words.
fn slice_stale_echo_prefix(tr: &Tracker, text: &str) -> Option<String> {
    if let Some((completed_id, completed_at)) = tr.last_completed() {
        if completed_at.elapsed() < STALE_ECHO_WINDOW && tr.matches_line_tokens(completed_id, text)
        {
            let tail = tr.strip_completed_prefix(completed_id, text);
            if tail.is_empty() {
                return None;
            }
            return Some(tail);
        }
    }
    Some(text.to_string())
}

/// Processes one STT transcript (interim or final) against `Tracker` under lock.
///
/// Key invariants enforced here:
/// - **Strip-Before-Evaluate (#2):** Combined run-on transcripts (`[Line N + start of Line N+1]`)
///   strip Line N's prefix instead of discarding the entire payload, preserving Line N+1's opening
///   words in both `window` and `active_matched` (for finals) or `partial` (for interims).
/// - **Live-Advance Spillover Preservation (#2):** When an interim completes Line N while already
///   carrying opening words of Line N+1, those trailing words immediately seed Line N+1's `partial`.
pub(crate) fn ingest_transcript(tr: &mut Tracker, text: &str, is_final: bool) -> IngestOutcome {
    let mut committed: Option<(i32, &'static str)> = None;

    if is_final {
        println!("STT_FINAL \"{text}\"");
        match slice_stale_echo_prefix(tr, text) {
            None => {
                println!("STT_ECHO_DROPPED (pure delayed echo of completed line)");
            }
            Some(effective) => {
                if effective.len() != text.trim().len() {
                    println!("STT_RUNON_TAIL_PRESERVED tail=\"{effective}\"");
                }
                if let Some(next_id) = tr.on_utterance_final(&effective) {
                    committed = Some((next_id, "line-completed"));
                } else {
                    tr.push_text(&effective);
                }
            }
        }
        tr.clear_partial();
    } else {
        match slice_stale_echo_prefix(tr, text) {
            None => {
                // Stale interim still repeating the line we just completed; clear so it
                // cannot trigger a phantom Stage 1/2 cycle.
                tr.clear_partial();
            }
            Some(effective) => {
                tr.set_partial(&effective);
                if tr.active_line_spoken_out() {
                    let old_active = tr.active_id();
                    if let Some(next_id) = tr.complete_active() {
                        // Preserve any immediate run-on words of `next_id` already present
                        // at the end of the triggering interim.
                        let tail = tr.strip_completed_prefix(old_active, &effective);
                        if !tail.is_empty() {
                            tr.set_partial(&tail);
                        }
                        committed = Some((next_id, "line-spoken"));
                    }
                }
            }
        }
    }

    if let Some((id, reason)) = committed {
        return IngestOutcome::Committed(id, reason);
    }

    if tr.should_run_cycle() {
        tr.mark_cycle_ran();
        IngestOutcome::RunCycle(CyclePlan {
            local: tr.local_match(),
            buffer: tr.spoken_buffer(),
            active: tr.active_id(),
            candidates: tr.all_lines(),
        })
    } else {
        IngestOutcome::Idle
    }
}

/// Applies a confident Stage 1 `local_match()` proposal with strict directional guards (#3):
/// - `step == 0`: confirms active line and resets pending hysteresis votes.
/// - `step in 1..=2`: commits immediately (strict forward sequential reach clamp).
/// - `step > 2`: rejected (`TRACKER_HOLD`) so distant keyword collisions never skip ahead.
/// - `step < 0`: routed through 3-confirmation hysteresis (`consider(id, false)`) instead of
///   instant 1-hit commit, requiring both Chunk 2's high lexical floor AND 3 consecutive cycles.
pub(crate) fn apply_stage1_confident(
    tr: &mut Tracker,
    proposed_id: i32,
) -> Option<(i32, &'static str)> {
    let active = tr.active_id();
    let Some(step) = line_step(tr.lines(), active, proposed_id) else {
        return None;
    };

    match step {
        0 => {
            let _ = tr.consider(proposed_id, true);
            None
        }
        1 | 2 => tr
            .consider(proposed_id, true)
            .map(|new_id| (new_id, "stage1-local")),
        s if s > 2 => {
            println!(
                "TRACKER_HOLD active={active} (clamping distant Stage 1 forward proposal {proposed_id} step=+{s})"
            );
            None
        }
        s => {
            // Backward match passed Chunk 2's high coverage floor; still require 3 consecutive
            // confirming cycles before committing to prevent short-line backward jitter.
            let committed = tr.consider(proposed_id, false);
            if committed.is_none() {
                println!(
                    "TRACKER_BACKWARD_PENDING active={active} proposed={proposed_id} step={s}"
                );
            }
            committed.map(|new_id| (new_id, "stage1-local-backward"))
        }
    }
}

/// Applies an unconfident Stage 1 local fallback (only when LLM is absent or circuit-open):
/// strictly limited to holding active (`step == 0`) or advancing one line (`step == 1`) with
/// 2-confirmation hysteresis (`consider(local_id, false)`).
pub(crate) fn apply_stage1_fallback(
    tr: &mut Tracker,
    local_id: i32,
    circuit_open: bool,
) -> Option<(i32, &'static str)> {
    let active = tr.active_id();
    let step = line_step(tr.lines(), active, local_id)?;
    if step == 0 || step == 1 {
        let reason = if circuit_open {
            "stage1-hysteresis-circuit-open"
        } else {
            "stage1-hysteresis"
        };
        tr.consider(local_id, false).map(|new_id| (new_id, reason))
    } else {
        println!(
            "TRACKER_HOLD active={active} (ignoring unconfident Stage 1 fallback {local_id} step={step})"
        );
        None
    }
}

/// Applies a Stage 2 LLM proposal after re-verifying that the tracker did not move while the
/// network call was in flight. Strictly clamped to `step == 1 || step == 2`.
pub(crate) fn apply_llm_proposal(
    tr: &mut Tracker,
    expected_active: i32,
    proposed_id: i32,
    decided_by_llm: bool,
) -> Option<(i32, &'static str)> {
    if tr.active_id() != expected_active {
        println!(
            "TRACKER_LLM_STALE expected_active={expected_active} current_active={} proposed={proposed_id}",
            tr.active_id()
        );
        return None;
    }
    let Some(step) = line_step(tr.lines(), expected_active, proposed_id) else {
        return None;
    };
    if step == 0 {
        println!("TRACKER_FREEZE active={expected_active} confirmed by LLM");
        let _ = tr.consider(proposed_id, false);
        return None;
    }
    if step == 1 || step == 2 {
        let reason = if decided_by_llm {
            "stage2-llm"
        } else {
            "stage1-local"
        };
        tr.consider(proposed_id, false)
            .map(|new_id| (new_id, reason))
    } else {
        println!(
            "TRACKER_HOLD active={expected_active} (ignoring out-of-order LLM proposal {proposed_id} step={step})"
        );
        None
    }
}

/// Starts a session. Kept separate from the command so the app can also autostart
/// headlessly during verification (`STEALTH_AUTOSTART=1`).
fn begin_session(
    app: &tauri::AppHandle,
    device: Option<String>,
    engine: Option<String>,
) -> Result<SessionInfo, String> {
    let state = app.state::<AppState>();

    let engine_mode = engine.as_deref().unwrap_or("apple").to_lowercase();
    let is_apple = engine_mode == "apple";

    let deepgram_key = keys::get("DEEPGRAM_API_KEY");
    let llm_provider = keys::get("GROQ_API_KEY")
        .map(tracker::LlmProvider::Groq)
        .or_else(|| keys::get("CEREBRAS_API_KEY").map(tracker::LlmProvider::Cerebras))
        .or_else(|| {
            keys::get("JEV_API_KEY")
                .or_else(|| keys::get("OPENROUTER_API_KEY"))
                .map(tracker::LlmProvider::Jev)
        });

    if !is_apple && deepgram_key.is_none() {
        return Err("DEEPGRAM_API_KEY not found in env or Keychain".to_string());
    }

    let (audio_tx, audio_rx) = mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));

    // ATOMIC check-and-reserve under a single critical section:
    // Guarantees no two capture threads or recognizers can ever spawn concurrently.
    {
        let mut tx_guard = state.audio_tx.lock_or_recover();
        if tx_guard.is_some() {
            return Err("a session is already running".to_string());
        }
        *tx_guard = Some(audio_tx.clone());
        *state.stop.lock_or_recover() = Some(stop.clone());
    }

    println!(
        "KEYS engine={} deepgram={} llm={}",
        engine_mode,
        deepgram_key
            .as_deref()
            .map(keys::mask)
            .unwrap_or_else(|| "none".to_string()),
        llm_provider
            .as_ref()
            .map(|p| match p {
                tracker::LlmProvider::Groq(k) => format!("groq({})", keys::mask(k)),
                tracker::LlmProvider::Cerebras(k) => format!("cerebras({})", keys::mask(k)),
                tracker::LlmProvider::Jev(k) => format!("jev({})", keys::mask(k)),
            })
            .unwrap_or_else(|| "none".to_string())
    );

    // Simulated Call Mode: replay a 16 kHz mono WAV through the pipeline.
    let stats = match (|| -> Result<audio::CaptureStats, String> {
        match std::env::var("STEALTH_SIMULATE_WAV") {
            Ok(path) if !path.trim().is_empty() => {
                audio::replay_wav(&path, audio_tx.clone(), stop.clone(), state.level.clone())
            }
            _ => {
                // Refuse to open a stream that macOS will silently zero out.
                #[cfg(target_os = "macos")]
                mic_permission::ensure()?;
                spawn_capture(device, audio_tx.clone(), state.level.clone(), stop.clone())
            }
        }
    })() {
        Ok(s) => s,
        Err(err) => {
            state.audio_tx.lock_or_recover().take();
            state.stop.lock_or_recover().take();
            return Err(err);
        }
    };

    let (stt_tx, mut stt_rx) = mpsc::unbounded_channel();
    let mut actual_is_apple = is_apple;
    if is_apple {
        #[cfg(target_os = "macos")]
        {
            let apple_perm = apple_stt::ensure_permission();
            if let Err(perm_err) = apple_perm {
                if let Some(dg) = deepgram_key.clone() {
                    println!(
                        "APPLE_SPEECH unavailable ({perm_err}); falling back to Deepgram Nova-3"
                    );
                    actual_is_apple = false;
                    tauri::async_runtime::spawn(async move {
                        stt::run(dg, audio_rx, stt_tx).await;
                    });
                } else {
                    return Err(perm_err);
                }
            } else {
                let stop_flag = stop.clone();
                let vocab = state.tracker.lock_or_recover().vocabulary();
                tauri::async_runtime::spawn(async move {
                    apple_stt::run(audio_rx, stt_tx, stop_flag, vocab).await;
                });
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let dg = deepgram_key.unwrap_or_default();
            tauri::async_runtime::spawn(async move {
                stt::run(dg, audio_rx, stt_tx).await;
            });
        }
    } else {
        let dg = deepgram_key.unwrap_or_default();
        tauri::async_runtime::spawn(async move {
            stt::run(dg, audio_rx, stt_tx).await;
        });
    }

    // Silence watchdog. Any cause — permission, a muted device, the wrong input, a hardware
    // mute switch — looks identical from here: a stream that produces zeros. Rather than sit
    // there looking alive, say so.
    {
        let watch_level = state.level.clone();
        let watch_stop = stop.clone();
        let watch_app = (*app).clone();
        std::thread::Builder::new()
            .name("audio-watchdog".into())
            .spawn(move || {
                let mut silent_ticks = 0u32;
                let mut ticks = 0u32;
                let mut reported = false;
                while !watch_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    ticks += 1;
                    let rms = f32::from_bits(watch_level.load(Ordering::Relaxed));
                    if rms.is_nan() {
                        // The source closed: end of fixture, or the stream shut down. Nothing to
                        // watch, and "silence" here would be a false alarm.
                        println!("AUDIO_SOURCE_CLOSED");
                        let _ = std::io::stdout().flush();
                        return;
                    }
                    if ticks % 4 == 0 {
                        println!("AUDIO_STATS rms={rms:.6}");
                        let _ = std::io::stdout().flush();
                    }
                    if rms < 1e-5 {
                        silent_ticks += 1;
                    } else {
                        silent_ticks = 0;
                        reported = false;
                    }
                    // 6 ticks at 500ms = 3s of nothing at all.
                    if silent_ticks == 6 && !reported {
                        reported = true;
                        println!("AUDIO_SILENT rms={rms:.6} no microphone signal after 3s");
                        let _ = std::io::stdout().flush();
                        let _ = watch_app.emit(
                            "status",
                            StatusEvent {
                                running: true,
                                message: "no microphone signal - check permission or input device"
                                    .to_string(),
                            },
                        );
                    }
                }
            })
            .map_err(|e| format!("cannot spawn audio watchdog: {e}"))?;
    }

    let tracker_arc = state.tracker.clone();
    let tracker_epoch_arc = state.tracker_epoch.clone();
    let call_type_arc = state.call_type.clone();
    let objection_lock_arc = state.objection_lock.clone();
    let http = state.http.clone();
    let llm_provider_slot = llm_provider.clone();
    let app_handle: tauri::AppHandle = (*app).clone();

    tauri::async_runtime::spawn(async move {
        let mut llm_failures: u32 = 0;
        let mut last_llm: Option<std::time::Instant> = None;
        let mut last_rep_obj: Option<(i32, std::time::Instant)> = None;
        // Routing decisions are logged on every interim word, which is a lot of file I/O during a
        // call. Opt in with STEALTH_VERBOSE=1 when diagnosing.
        let verbose = std::env::var("STEALTH_VERBOSE").is_ok();
        while let Some(event) = stt_rx.recv().await {
            let transcript = match event {
                stt::SttEvent::Connected => {
                    println!("STT_READY");
                    emit_status(&app_handle, true, "listening");
                    continue;
                }
                stt::SttEvent::Error(err) => {
                    eprintln!("STT_EVENT_ERROR {err}");
                    emit_status(&app_handle, false, format!("STT: {err}"));
                    continue;
                }
                stt::SttEvent::Transcript(t) => t,
            };

            let _ = app_handle.emit("transcript", &transcript);

            // Tier 0 Fast-Path: Rep speaks 2-3 words of an objection rebuttal.
            // 0ms latency, highest priority over prospect speech.
            if let Some(obj_idx) = tracker::detect_rep_objection_prefix(&transcript.text) {
                let now = std::time::Instant::now();
                let should_emit = last_rep_obj.map_or(true, |(prev_idx, prev_at)| {
                    prev_idx != obj_idx || now.duration_since(prev_at) >= std::time::Duration::from_secs(4)
                });
                if should_emit {
                    last_rep_obj = Some((obj_idx, now));
                    *objection_lock_arc.lock_or_recover() = Some(now + Duration::from_secs(14));
                    println!("REP_OBJECTION_PREFIX matched index={obj_idx} text=\"{}\"", transcript.text);
                    let _ = app_handle.emit("objection", obj_idx);
                }
            }

            // Objection rebuttal lock: while rep delivers an objection, freeze center tracker
            // to eliminate chaotic jumps caused by rebuttal words matching random script lines.
            let is_objection_locked = {
                let lock = objection_lock_arc.lock_or_recover();
                lock.map_or(false, |until| std::time::Instant::now() < until)
            };

            if is_objection_locked {
                tracker_arc.lock_or_recover().clear_partial();
                continue;
            }

            // Cold call mode: speech tracking auto-advancement is disabled per user request
            // to eliminate accidental jumps on overlapping script words. The rep manually clicks
            // and navigates flowchart cards. Objection detection remains 100% active above.
            let is_coldcall = {
                let ct = call_type_arc.lock_or_recover();
                ct.as_str() == "coldcall"
            };

            if is_coldcall {
                tracker_arc.lock_or_recover().clear_partial();
                continue;
            }

            let (outcome, epoch_before) = {
                let mut tr = tracker_arc.lock_or_recover();
                let epoch = tracker_epoch_arc.load(Ordering::SeqCst);
                (ingest_transcript(&mut tr, &transcript.text, transcript.is_final), epoch)
            };

            let plan = match outcome {
                IngestOutcome::Committed(id, reason) => {
                    emit_active(&app_handle, &tracker_arc, id, reason);
                    continue;
                }
                IngestOutcome::Idle => continue,
                IngestOutcome::RunCycle(plan) => plan,
            };

            let CyclePlan {
                local,
                buffer,
                active,
                candidates,
            } = plan;
            let mut committed: Option<(i32, &'static str)> = None;

            if tracker::needs_llm(local) {
                let mut proposal: Option<i32> = None;
                let mut decided_by_llm = false;
                let llm_ready =
                    last_llm.map_or(true, |at| at.elapsed() >= Duration::from_millis(1000));
                if let Some(ref provider) = llm_provider_slot {
                    if llm_failures >= 3
                        && last_llm.map_or(false, |at| at.elapsed() >= Duration::from_secs(20))
                    {
                        llm_failures = 0;
                        println!("TRACKER_LLM_RESET circuit breaker reset after quiet cooldown");
                    }
                    if llm_failures < 3 && llm_ready {
                        last_llm = Some(std::time::Instant::now());
                        let provider_name = match provider {
                            tracker::LlmProvider::Groq(_) => "groq",
                            tracker::LlmProvider::Cerebras(_) => "cerebras",
                            tracker::LlmProvider::Jev(_) => "jev",
                        };
                        println!(
                            "TRACKER_ROUTE stage2-{provider_name} active={active} buffer_words={}",
                            buffer.split_whitespace().count()
                        );
                        match tracker::classify_with_llm(
                            &http,
                            provider,
                            &candidates,
                            active,
                            &buffer,
                        )
                        .await
                        {
                            Ok(decision) => {
                                llm_failures = 0;
                                proposal = decision.choice;
                                decided_by_llm = true;
                                if let Some(obj) = decision.objection {
                                    println!(
                                        "TRACKER_LLM_OBJECTION ({provider_name}) objection={obj}"
                                    );
                                    let _ = app_handle.emit("objection", obj);
                                }
                                if let Some(id) = decision.choice {
                                    println!("TRACKER_LLM_OK ({provider_name}) active={id}");
                                } else {
                                    println!("TRACKER_LLM_UNSURE ({provider_name}) active={active}");
                                }
                            }
                            Err(err) => {
                                llm_failures += 1;
                                eprintln!(
                                    "TRACKER_LLM_ERROR ({provider_name}) n={llm_failures} {err}"
                                );
                                if err.contains("429") {
                                    last_llm =
                                        Some(std::time::Instant::now() + Duration::from_secs(5));
                                }
                            }
                        }
                    } else if verbose {
                        println!("TRACKER_ROUTE stage1-local (llm throttled or circuit open)");
                    }
                } else if verbose {
                    println!("TRACKER_ROUTE stage1-local (no llm key)");
                }

                // Re-verify live guards after the async LLM await: discard if an objection lock,
                // script switch to coldcall, or manual navigation happened mid-flight.
                let still_valid = {
                    let obj_locked = objection_lock_arc
                        .lock_or_recover()
                        .map_or(false, |until| std::time::Instant::now() < until);
                    let switched_to_coldcall =
                        call_type_arc.lock_or_recover().as_str() == "coldcall";
                    let same_epoch = tracker_epoch_arc.load(Ordering::SeqCst) == epoch_before;
                    !obj_locked && !switched_to_coldcall && same_epoch
                };

                if !still_valid {
                    println!("TRACKER_LLM_STALE discarding post-await decision (epoch/mode changed)");
                } else if let Some(id) = proposal {
                    let mut tr = tracker_arc.lock_or_recover();
                    committed = apply_llm_proposal(&mut tr, active, id, decided_by_llm);
                } else if llm_provider_slot.is_none() || llm_failures >= 3 {
                    if let Some((local_id, _, _)) = local {
                        let mut tr = tracker_arc.lock_or_recover();
                        if tr.active_id() == active {
                            committed =
                                apply_stage1_fallback(&mut tr, local_id, llm_failures >= 3);
                        }
                    }
                } else {
                    println!("TRACKER_FREEZE active={active} (LLM holding position)");
                }
            } else if let Some((id, _, _)) = local {
                // Stage 1 was unambiguous: enforce strict forward clamp (<= +2) and
                // 3-confirmation hysteresis on any backward jump.
                let mut tr = tracker_arc.lock_or_recover();
                if tracker_epoch_arc.load(Ordering::SeqCst) == epoch_before
                    && tr.active_id() == active
                {
                    committed = apply_stage1_confident(&mut tr, id);
                }
            }

            if let Some((id, reason)) = committed {
                emit_active(&app_handle, &tracker_arc, id, reason);
            }
        }
        println!("SESSION_LOOP_ENDED");
    });

    emit_status(
        app,
        true,
        format!(
            "listening on {} ({})",
            stats.device,
            if actual_is_apple {
                "Apple Neural Engine"
            } else {
                "Deepgram Nova-3"
            }
        ),
    );

    // From here on bare digits belong to the battlecards (the tap still requires the user to
    // have armed the mode; otherwise every keystroke passes through).
    bare_keys::set_listening(true);

    Ok(SessionInfo {
        device: stats,
        engine: if actual_is_apple {
            "apple".to_string()
        } else {
            "deepgram".to_string()
        },
        deepgram: !actual_is_apple,
        jev: llm_provider.is_some(),
    })
}

#[tauri::command]
async fn start_session(
    app: tauri::AppHandle,
    device: Option<String>,
    engine: Option<String>,
) -> Result<SessionInfo, String> {
    begin_session(&app, device, engine)
}

/// Ends a session. Shared by the command and the headless autostop hook so the loop
/// exercises the same path a user's click does.
fn end_session(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    // Take this session's flag and raise it. Because the flag belongs to the session, a
    // following session cannot clear it before this producer has seen it.
    if let Some(stop) = state.stop.lock_or_recover().take() {
        stop.store(true, Ordering::Relaxed);
    }
    // Dropping the last remaining sender closes the audio channel, which makes the STT
    // task send CloseStream and return.
    state.audio_tx.lock_or_recover().take();
    bare_keys::set_listening(false);
    emit_status(app, false, "stopped");
}

#[tauri::command]
fn stop_session(app: tauri::AppHandle) -> Result<(), String> {
    end_session(&app);
    Ok(())
}

/// True while the *prospect* (system-audio) capture is running.
static PROSPECT_AUDIO_RUNNING: AtomicBool = AtomicBool::new(false);

pub fn spawn_prospect_pipeline(
    app: &tauri::AppHandle,
    rx: mpsc::UnboundedReceiver<Vec<i16>>,
    stop: Arc<AtomicBool>,
) {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<stt::SttEvent>();
    let vocab = app
        .try_state::<AppState>()
        .map(|s| s.tracker.lock_or_recover().vocabulary())
        .unwrap_or_default();

    let deepgram_key = keys::get("DEEPGRAM_API_KEY");

    #[cfg(target_os = "macos")]
    {
        if let Ok(()) = apple_stt::ensure_permission() {
            let stop_flag = stop.clone();
            tauri::async_runtime::spawn(apple_stt::run(rx, event_tx, stop_flag, vocab));
        } else if let Some(dg) = deepgram_key {
            tauri::async_runtime::spawn(stt::run(dg, rx, event_tx));
        } else {
            eprintln!("PROSPECT_PIPELINE: no speech recognizer available");
            return;
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(dg) = deepgram_key {
            tauri::async_runtime::spawn(stt::run(dg, rx, event_tx));
        } else {
            eprintln!("PROSPECT_PIPELINE: Deepgram key required on non-macOS");
            return;
        }
    }

    let emit_to = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut recent = objection_detect::Recent::new();
        let mut tail = String::new();
        while let Some(event) = event_rx.recv().await {
            match event {
                stt::SttEvent::Transcript(t) => {
                    let _ = emit_to.emit("prospect_transcript", t.text.clone());
                    let candidate = if tail.is_empty() {
                        t.text.clone()
                    } else {
                        format!("{} {}", tail, t.text)
                    };
                    let active_call = emit_to
                        .try_state::<AppState>()
                        .map(|s| s.call_type.lock_or_recover().clone())
                        .unwrap_or_else(|| "coldcall".to_string());
                    if let Some(index) = objection_detect::classify_for_call(&candidate, &active_call) {
                        // Suppress cold call objections that are archived or already handled directly on the flowchart sheet (3, 6, 8, 9, 0)
                        if active_call == "coldcall" && COLD_DELETED_INDICES.contains(&index) {
                            // Handled natively on the flowchart sheet, skip popup and tracker lock
                        } else if recent.should_emit(index) {
                            println!("PROSPECT_OBJECTION ({active_call}) index={index} text=\"{candidate}\"");
                            if let Some(state) = emit_to.try_state::<AppState>() {
                                *state.objection_lock.lock_or_recover() = Some(std::time::Instant::now() + Duration::from_secs(14));
                            }
                            let _ = emit_to.emit("objection", index);
                        }
                    }
                    if t.is_final {
                        tail.push(' ');
                        tail.push_str(&t.text);
                    }
                    let words: Vec<&str> = tail.split_whitespace().collect();
                    if words.len() > 30 {
                        tail = words[words.len() - 30..].join(" ");
                    }
                }
                stt::SttEvent::Error(err) => eprintln!("PROSPECT_STT_ERROR {err}"),
                stt::SttEvent::Connected => println!("PROSPECT_STT_READY"),
            }
        }
        println!("PROSPECT_PIPELINE_ENDED");
    });
}

/// Start the prospect stream: system audio → a second, isolated Apple STT session → keyword
/// classifier → the existing `objection` event, so the battlecard appears on its own.
///
/// Additive by construction: the microphone session, the tracker and Pulse Dialer are untouched.
fn start_prospect_audio(app: tauri::AppHandle) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        if PROSPECT_AUDIO_RUNNING.swap(true, Ordering::SeqCst) {
            return Ok("prospect audio already running".to_string());
        }
        system_audio::attach(app.clone());
        // A permission prompt can block for up to a minute, so none of this runs on the caller's
        // thread: the HUD keeps rendering and the microphone session is unaffected either way.
        tauri::async_runtime::spawn_blocking(move || {
            let fail = |app: &tauri::AppHandle, message: String| {
                eprintln!("PROSPECT_AUDIO_FAIL {message}");
                let _ = app.emit(
                    "prospect_status",
                    system_audio::ProspectStatus {
                        running: false,
                        message,
                    },
                );
                PROSPECT_AUDIO_RUNNING.store(false, Ordering::SeqCst);
            };

            // Same on-device recogniser, so it needs the Speech permission the mic session uses.
            if let Err(err) = apple_stt::ensure_permission() {
                fail(&app, err);
                return;
            }
            if !PROSPECT_AUDIO_RUNNING.load(Ordering::SeqCst) {
                return; // toggled off while the prompt was up
            }

            let rx = match system_audio::start() {
                Ok(rx) => rx,
                Err(err) => {
                    fail(&app, err);
                    return;
                }
            };

            let stop = Arc::new(AtomicBool::new(false));
            spawn_prospect_pipeline(&app, rx, stop);
        });

        Ok("starting prospect audio (macOS may ask for Speech and Screen Recording)".to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("prospect audio is only supported on macOS".to_string())
    }
}

#[tauri::command]
fn system_audio_start(app: tauri::AppHandle) -> Result<String, String> {
    start_prospect_audio(app)
}

#[tauri::command]
fn system_audio_stop() -> Result<(), String> {
    PROSPECT_AUDIO_RUNNING.store(false, Ordering::SeqCst);
    system_audio::stop();
    Ok(())
}

/// Arm bare 1–9 / 0 as objection triggers. Digits are only swallowed while a session is
/// listening, so idle typing and numbers in other apps are unaffected.
#[tauri::command]
fn bare_keys_start(app: tauri::AppHandle) -> Result<(), String> {
    bare_keys::attach(app);
    bare_keys::start()
}

#[tauri::command]
fn bare_keys_stop() -> Result<(), String> {
    bare_keys::stop();
    Ok(())
}

#[tauri::command]
fn bare_keys_armed() -> bool {
    bare_keys::is_armed()
}

#[tauri::command]
fn list_devices() -> Result<Vec<String>, String> {
    audio::list_input_devices()
}

#[tauri::command]
fn key_status() -> Vec<keys::KeyStatus> {
    vec![
        keys::status("DEEPGRAM_API_KEY"),
        keys::status("JEV_API_KEY"),
        keys::status("OPENROUTER_API_KEY"),
    ]
}

#[tauri::command]
fn tracker_snapshot(state: tauri::State<'_, AppState>) -> tracker::TrackerSnapshot {
    state.tracker.lock_or_recover().snapshot()
}

#[tauri::command]
fn nudge(app: tauri::AppHandle, state: tauri::State<'_, AppState>, delta: i32) -> i32 {
    state.tracker_epoch.fetch_add(1, Ordering::SeqCst);
    let id = state.tracker.lock_or_recover().manual(delta);
    emit_active(&app, &state.tracker, id, "manual");
    id
}

#[tauri::command]
fn jump_to_line(app: tauri::AppHandle, state: tauri::State<'_, AppState>, id: i32) -> i32 {
    state.tracker_epoch.fetch_add(1, Ordering::SeqCst);
    let new_id = state.tracker.lock_or_recover().jump_to(id);
    emit_active(&app, &state.tracker, new_id, "manual-click");
    new_id
}

#[tauri::command]
fn set_objection_active(state: tauri::State<'_, AppState>, active: bool) {
    let mut lock = state.objection_lock.lock_or_recover();
    if active {
        *lock = Some(std::time::Instant::now() + Duration::from_secs(14));
        println!("TRACKER_OBJECTION_LOCK activated via UI");
    } else {
        *lock = None;
        println!("TRACKER_OBJECTION_LOCK released via UI");
    }
}

#[tauri::command]
fn audio_level(state: tauri::State<'_, AppState>) -> f32 {
    f32::from_bits(state.level.load(Ordering::Relaxed))
}

#[tauri::command]
fn script_lines(state: tauri::State<'_, AppState>) -> Vec<tracker::ScriptLine> {
    state.tracker.lock_or_recover().lines().to_vec()
}

pub fn do_select_script(
    app: &tauri::AppHandle,
    tracker_mutex: &Arc<Mutex<Tracker>>,
    call_type: &str,
) -> Result<Vec<tracker::ScriptLine>, String> {
    if let Some(state) = app.try_state::<AppState>() {
        state.tracker_epoch.fetch_add(1, Ordering::SeqCst);
        *state.call_type.lock_or_recover() = call_type.to_string();
    }
    let script = tracker::load_script_by_type(call_type)?;
    let lines = script.lines.clone();
    let id = tracker_mutex.lock_or_recover().switch_script(lines.clone());
    emit_active(app, tracker_mutex, id, "script-select");
    let _ = app.emit("script_selected", call_type);
    println!(
        "SCRIPT_SWITCHED to '{}' ({call_type}) lines={}",
        script.title,
        lines.len()
    );
    Ok(lines)
}

#[tauri::command]
fn select_script(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    call_type: String,
) -> Result<Vec<tracker::ScriptLine>, String> {
    do_select_script(&app, &state.tracker, &call_type)
}

fn toggle_visibility(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("hud") else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    let outcome = if visible {
        window.hide()
    } else {
        window.show()
    };
    match outcome {
        Ok(()) => println!("HOTKEY visibility={}", !visible),
        Err(err) => eprintln!("HOTKEY_ERROR visibility: {err}"),
    }
}

fn toggle_edit_mode(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("hud") else {
        return;
    };
    let click_through = !CLICK_THROUGH.load(Ordering::Relaxed);
    match window.set_ignore_cursor_events(click_through) {
        Ok(()) => {
            CLICK_THROUGH.store(click_through, Ordering::Relaxed);
            println!(
                "HOTKEY click_through={click_through} edit_mode={}",
                !click_through
            );
            // Tell the HUD to show its grab handle and unlocked outline.
            let _ = app.emit("edit_mode", !click_through);
            refresh_tray();
        }
        Err(err) => eprintln!("HOTKEY_ERROR click_through: {err}"),
    }
}

fn nudge_from_hotkey(app: &tauri::AppHandle, delta: i32) {
    let state = app.state::<AppState>();
    state.tracker_epoch.fetch_add(1, Ordering::SeqCst);
    let id = state.tracker.lock_or_recover().manual(delta);
    println!("HOTKEY nudge delta={delta} active={id}");
    emit_active(app, &state.tracker, id, "manual-hotkey");
}

fn phase_from_hotkey(app: &tauri::AppHandle, delta: i32) {
    let state = app.state::<AppState>();
    state.tracker_epoch.fetch_add(1, Ordering::SeqCst);
    let id = state.tracker.lock_or_recover().jump_phase(delta);
    println!("HOTKEY phase delta={delta} active={id}");
    emit_active(app, &state.tracker, id, "manual-phase");
}

fn reanchor_from_hotkey(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    state.tracker_epoch.fetch_add(1, Ordering::SeqCst);
    let id = state.tracker.lock_or_recover().reanchor();
    println!("HOTKEY reanchor active={id}");
    emit_active(app, &state.tracker, id, "manual-reanchor");
}

/// Maps a digit key (top row or numpad) to the objection battlecard index 0-9.
fn digit_index(key: Code) -> Option<u8> {
    Some(match key {
        Code::Digit1 | Code::Numpad1 => 1,
        Code::Digit2 | Code::Numpad2 => 2,
        Code::Digit3 | Code::Numpad3 => 3,
        Code::Digit4 | Code::Numpad4 => 4,
        Code::Digit5 | Code::Numpad5 => 5,
        Code::Digit6 | Code::Numpad6 => 6,
        Code::Digit7 | Code::Numpad7 => 7,
        Code::Digit8 | Code::Numpad8 => 8,
        Code::Digit9 | Code::Numpad9 => 9,
        Code::Digit0 | Code::Numpad0 => 0,
        _ => return None,
    })
}

/// Registers the global shortcuts. These are process-wide, so they must stay a small,
/// deliberate set — every one of them steals the combination from every other app.
fn register_hotkeys(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let mut shortcuts = vec![
        Shortcut::new(Some(cmd_shift_opt()), Code::KeyH),
        Shortcut::new(Some(cmd_shift_opt()), Code::KeyL),
        Shortcut::new(Some(cmd_shift_opt()), Code::ArrowLeft),
        Shortcut::new(Some(cmd_shift_opt()), Code::ArrowRight),
        Shortcut::new(Some(cmd_shift_opt()), Code::ArrowUp),
        Shortcut::new(Some(cmd_shift_opt()), Code::ArrowDown),
        Shortcut::new(Some(cmd_shift_opt()), Code::KeyR),
        Shortcut::new(Some(cmd_shift_opt()), Code::KeyD),
    ];
    // Control + digit = objection battlecards. Control+arrows are avoided because macOS binds
    // them to Mission Control / Spaces.
    for key in [
        Code::Digit1,
        Code::Digit2,
        Code::Digit3,
        Code::Digit4,
        Code::Digit5,
        Code::Digit6,
        Code::Digit7,
        Code::Digit8,
        Code::Digit9,
        Code::Digit0,
    ] {
        shortcuts.push(Shortcut::new(Some(Modifiers::CONTROL), key));
    }
    app.global_shortcut()
        .register_multiple(shortcuts)
        .map_err(|err| Box::<dyn std::error::Error>::from(err.to_string()))?;
    println!(
        "HOTKEYS registered cmd+shift+opt H/L/←/→/↑/↓/R nudge+phase+reanchor, ctrl+0-9 objections"
    );
    Ok(())
}

/// Keeps the HUD present: level, `hidesOnDeactivate`, and the collection behaviour that lets
/// the window join another app's fullscreen Space.
///
/// None of this survives being set once. The level was measurably back to 5 within 500 ms of
/// startup, and the collection behaviour is reverted along the same path — which is what would
/// leave the HUD missing while Chrome is fullscreen. Mutating `NSWindow` off the main thread
/// deadlocks (it hung a thread when tried), so every correction is dispatched.
fn enforce_overlay_presence(handle: &tauri::AppHandle) {
    let inner = handle.clone();
    let _ = handle.run_on_main_thread(move || {
        if let Some(win) = inner.get_webview_window("hud") {
            if let Some(changes) = overlay_macos::enforce_presence(&win) {
                // Only logged when something actually had to be corrected.
                println!("OVERLAY_PRESENCE_ENFORCED {changes}");
                let _ = std::io::stdout().flush();
            }
            // Surface the one case we cannot fix: another app's native fullscreen Space.
            let off_space = overlay_macos::is_on_active_space(&win) == Some(false);
            if off_space != OFF_ACTIVE_SPACE.swap(off_space, Ordering::Relaxed) {
                refresh_tray();
            }
        }
    });
}

/// Chrome's front-window bounds and the desktop bounds, so a measurement can be labelled as
/// fullscreen or not rather than assumed. In native fullscreen Chrome's window matches the
/// screen exactly.
#[cfg(target_os = "macos")]
fn chrome_bounds() -> String {
    let script = r#"
tell application "System Events"
  tell process "Google Chrome"
    set w to front window
    return (position of w as text) & " " & (size of w as text)
  end tell
end tell"#;
    let out = std::process::Command::new("osascript")
        .args(["-e", script])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => format!("err:{}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(err) => format!("spawn error: {err}"),
    }
}

/// Desktop bounds, for comparison with `chrome_bounds`.
#[cfg(target_os = "macos")]
fn screen_bounds() -> String {
    let out = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"Finder\" to get bounds of window of desktop",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => format!("err:{}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(err) => format!("spawn error: {err}"),
    }
}

/// One line describing whether the HUD is still on screen. Reads must happen on the main
/// thread, so the values are collected there and handed back over a channel.
#[cfg(target_os = "macos")]
fn focus_snapshot(handle: &tauri::AppHandle) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    let inner = handle.clone();
    let dispatched = handle.run_on_main_thread(move || {
        let line = match inner.get_webview_window("hud") {
            Some(win) => format!(
                "level={:?} visible={:?} occlusion={:?} on_active_space={:?} \
hides_on_deactivate={:?} key={:?}",
                overlay_macos::window_level(&win),
                win.is_visible().ok(),
                overlay_macos::occlusion_state(&win).map(|bits| (bits, bits & 2 != 0)),
                overlay_macos::is_on_active_space(&win),
                overlay_macos::hides_on_deactivate(&win),
                overlay_macos::is_key_window(&win),
            ),
            None => "no hud window".to_string(),
        };
        let _ = tx.send(line);
    });
    if dispatched.is_err() {
        return "dispatch failed".into();
    }
    rx.recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| "timeout".into())
}

/// Mirrors `stdout`/`stderr` into `~/Library/Logs/StealthPrompter.log`.
///
/// A Finder-launched `.app` has no terminal, so every diagnostic this app prints went nowhere —
/// which is a large part of why a silent microphone took this long to pin down. Runs only when
/// fd 1 is neither a terminal nor a regular file, so dev runs and `> logfile` captures are left
/// untouched.
fn init_file_logging() {
    #[cfg(unix)]
    {
        // SAFETY: plain libc calls on fds this process owns.
        unsafe {
            if libc::isatty(1) == 1 {
                return;
            }
            let mut stat: libc::stat = std::mem::zeroed();
            if libc::fstat(1, &mut stat) == 0 && (stat.st_mode & libc::S_IFMT) == libc::S_IFREG {
                return;
            }
            let Ok(home) = std::env::var("HOME") else {
                return;
            };
            let path = format!("{home}/Library/Logs/StealthPrompter.log");
            let Some(dir) = std::path::Path::new(&path).parent() else {
                return;
            };
            if std::fs::create_dir_all(dir).is_err() {
                return;
            }
            let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            else {
                return;
            };
            use std::os::unix::io::AsRawFd;
            if libc::dup2(file.as_raw_fd(), 1) < 0 || libc::dup2(file.as_raw_fd(), 2) < 0 {
                return;
            }
            println!("LOG_FILE {path}");
        }
    }
}

/// Returns the most recent dialer-pushed context, so a late-mounting UI can still fetch it.
#[tauri::command]
fn call_context() -> Option<context_server::CallContext> {
    context_server::current()
}

/// Clears dialer-pushed prospect context back to empty defaults.
#[tauri::command]
fn clear_context(app: tauri::AppHandle) {
    context_server::clear_context(&app);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_file_logging();
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    if shortcut.mods == cmd_shift_opt() {
                        match shortcut.key {
                            Code::KeyH => toggle_visibility(app),
                            Code::KeyL => toggle_edit_mode(app),
                            Code::ArrowLeft => nudge_from_hotkey(app, -1),
                            Code::ArrowRight => nudge_from_hotkey(app, 1),
                            Code::ArrowUp => phase_from_hotkey(app, -1),
                            Code::ArrowDown => phase_from_hotkey(app, 1),
                            Code::KeyR => reanchor_from_hotkey(app),
                            Code::KeyD => {
                                let _ = app.emit("objection_dismiss", ());
                                println!("HOTKEY objection dismiss");
                            }
                            _ => {}
                        }
                        return;
                    }
                    // Control + digit = objection battlecard (overlay only, never moves the script).
                    if shortcut.mods == Modifiers::CONTROL {
                        if let Some(index) = digit_index(shortcut.key) {
                            let _ = app.emit("objection", index);
                            println!("HOTKEY objection index={index}");
                        }
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            hud_report,
            set_click_through,
            start_session,
            stop_session,
            list_devices,
            key_status,
            tracker_snapshot,
            nudge,
            jump_to_line,
            audio_level,
            script_lines,
            select_script,
            hud_state,
            input_probe,
            drag_probe,
            open_mic_settings,
            open_speech_settings,
            open_screen_settings,
            open_accessibility_settings,
            bare_keys_start,
            bare_keys_stop,
            bare_keys_armed,
            system_audio_start,
            system_audio_stop,
            set_objection_active,
            call_context,
            clear_context
        ])
        .on_window_event(|window, event| {
            // Dragging and resizing fire continuously; just record the latest placement and
            // let the debounce thread write it once the window settles.
            if matches!(
                event,
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_)
            ) {
                let app = window.app_handle();
                if let Some(win) = app.get_webview_window("hud") {
                    if let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) {
                        record_geometry(Geometry {
                            x: pos.x,
                            y: pos.y,
                            width: size.width,
                            height: size.height,
                        });
                    }
                }
            }
        })
        .setup(|app| {
            // Test hook: start the prospect (system-audio) tap at launch so the ScreenCaptureKit
            // path and its permission prompt can be exercised without clicking the HUD.
            if std::env::var("STEALTH_PROSPECT_AUDIO").is_ok() {
                match start_prospect_audio(app.handle().clone()) {
                    Ok(msg) => println!("PROSPECT_AUDIO_AUTOSTART {msg}"),
                    Err(err) => eprintln!("PROSPECT_AUDIO_AUTOSTART_FAIL {err}"),
                }
            }
            let script = tracker::load_script().unwrap_or_else(|e| panic!("{e}"));
            println!(
                "SCRIPT_LOADED '{}' lines={}",
                script.title,
                script.lines.len()
            );
            let tracker = Tracker::new(script.lines).expect("tracker builds");

            app.manage(AppState {
                tracker: Arc::new(Mutex::new(tracker)),
                tracker_epoch: Arc::new(AtomicU64::new(0)),
                call_type: Arc::new(Mutex::new("call1".to_string())),
                objection_lock: Arc::new(Mutex::new(None)),
                level: Arc::new(AtomicU32::new(0)),
                stop: Mutex::new(None),
                audio_tx: Mutex::new(None),
                http: reqwest::Client::builder()
                    .user_agent("stealthprompter/1.0")
                    .timeout(Duration::from_secs(5))
                    .build()
                    .expect("http client"),
            });

            if let Ok(st) = std::env::var("STEALTH_SCRIPT") {
                let state = app.state::<AppState>();
                let _ = do_select_script(&app.handle(), &state.tracker, &st);
            }

            // Restore the last placement. `center: true` used to live in tauri.conf.json and
            // silently overrode this, and macOS also repositions the window when it is first
            // shown — so the position is re-applied just after the window appears.
            if let (Some(geo), Some(win)) =
                (read_geometry(app.handle()), app.get_webview_window("hud"))
            {
                let _ = win.set_size(tauri::PhysicalSize::new(geo.width, geo.height));
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(500));
                    if let Some(win) = handle.get_webview_window("hud") {
                        if let Some(geo) = read_geometry(&handle) {
                            let is_on_any_monitor = win.available_monitors().ok().map(|monitors| {
                                monitors.iter().any(|m| {
                                    let mx = m.position().x;
                                    let my = m.position().y;
                                    let mw = m.size().width as i32;
                                    let mh = m.size().height as i32;
                                    let overlap_w = (geo.x + geo.width as i32).min(mx + mw) - geo.x.max(mx);
                                    let overlap_h = (geo.y + geo.height as i32).min(my + mh) - geo.y.max(my);
                                    overlap_w >= 100 && overlap_h >= 100
                                })
                            }).unwrap_or(false);

                            if is_on_any_monitor {
                                let _ = win.set_position(tauri::PhysicalPosition::new(geo.x, geo.y));
                            } else {
                                eprintln!(
                                    "WINDOW_OFFSCREEN wanted=({},{}) -> centering on visible monitor",
                                    geo.x, geo.y
                                );
                                let _ = win.center();
                            }
                            // set_position is dispatched to the main thread, so the value is
                            // not readable immediately; let it settle before measuring.
                            std::thread::sleep(Duration::from_millis(400));
                            println!(
                                "WINDOW_GEOMETRY_RESTORED wanted=({},{}) actual={:?}",
                                geo.x,
                                geo.y,
                                win.outer_position().map(|p| (p.x, p.y)).ok()
                            );
                        }
                    }
                });
            }

            // Test hook: prove that toggling edit mode actually flips the OS-level
            // click-through flag, independent of whether a global hotkey fires.
            if let Some(ms) = std::env::var("STEALTH_EDIT_TOGGLE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(ms));
                    if let Some(win) = handle.get_webview_window("hud") {
                        println!(
                            "EDIT_TOGGLE_TEST before={:?}",
                            overlay_macos::ignores_mouse_events(&win)
                        );
                    }
                    toggle_edit_mode(&handle);
                    std::thread::sleep(Duration::from_millis(600));
                    if let Some(win) = handle.get_webview_window("hud") {
                        println!(
                            "EDIT_TOGGLE_TEST after={:?}",
                            overlay_macos::ignores_mouse_events(&win)
                        );
                    }
                });
            }

            // Test hook: prove a synthesised click reaches the webview (logs INPUT_PROBE).
            // Delay is configurable so the HUD can be pushed out of focus first: the click
            // only proves anything while the window is *not* key.
            if std::env::var("STEALTH_CLICK_TEST").is_ok() {
                let delay = std::env::var("STEALTH_CLICK_TEST_MS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(3500);
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(delay));
                    let inner = handle.clone();
                    let _ = handle.run_on_main_thread(move || {
                        if let Some(win) = inner.get_webview_window("hud") {
                            println!("CLICK_TEST {}", overlay_macos::inject_click(&win));
                            let _ = std::io::stdout().flush();
                        }
                    });
                });
            }

            // Test hook: assert that a drag gesture actually moves the window. The gesture is
            // posted from the main thread but drained by the run loop afterwards, so the
            // position is re-read from a background thread once the queue has emptied.
            if std::env::var("STEALTH_DRAG_TEST").is_ok() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(6000));
                    let Some(win) = handle.get_webview_window("hud") else {
                        return;
                    };
                    let before = win.outer_position().map(|p| (p.x, p.y)).ok();
                    let inner = handle.clone();
                    let _ = handle.run_on_main_thread(move || {
                        if let Some(w) = inner.get_webview_window("hud") {
                            println!("DRAG_TEST inject {}", overlay_macos::inject_drag(&w));
                            let _ = std::io::stdout().flush();
                        }
                    });
                    std::thread::sleep(Duration::from_millis(2000));
                    let after = win.outer_position().map(|p| (p.x, p.y)).ok();
                    println!(
                        "DRAG_TEST before={before:?} after={after:?} moved={}",
                        before != after
                    );
                    let _ = std::io::stdout().flush();
                });
            }

            // Test hook: measure the HUD against Chrome. Reads the window state, activates
            // Chrome, then reads it again — the only way to see whether the HUD survives
            // another app taking focus without a human looking at the screen.
            if std::env::var("STEALTH_FOCUS_TEST").is_ok() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    // Sample the level over the first seconds so a reset can be timed.
                    for tick in 0..8 {
                        std::thread::sleep(Duration::from_millis(500));
                        if let Some(win) = handle.get_webview_window("hud") {
                            if tick == 2 {
                                enforce_overlay_presence(&handle);
                                println!("LEVEL_REASSERT dispatched");
                            }
                            println!(
                                "LEVEL_TRACE t_ms={} level={:?}",
                                (tick + 1) * 500,
                                overlay_macos::window_level(&win)
                            );
                        }
                    }
                    println!("FOCUS_TEST before {}", focus_snapshot(&handle));
                    let _ = std::process::Command::new("osascript")
                        .args(["-e", "tell application \"Google Chrome\" to activate"])
                        .status();
                    std::thread::sleep(Duration::from_millis(3000));
                    println!(
                        "FOCUS_TEST after_chrome chrome=[{}] {}",
                        chrome_bounds(),
                        focus_snapshot(&handle)
                    );

                    // Fullscreen is a different Space, not just a bigger window, so it needs
                    // its own measurement. Driving Chrome into it needs a synthesised
                    // keystroke, which fails without Accessibility permission — the status is
                    // reported rather than assumed.
                    if std::env::var("STEALTH_FULLSCREEN_TEST").is_ok() {
                        let toggle = || {
                            let _ = std::process::Command::new("osascript")
                                .args([
                                    "-e",
                                    "tell application \"System Events\" to keystroke \"f\" using {control down, command down}",
                                ])
                                .status();
                        };
                        // Two toggles, so one of the two measurements is always taken while
                        // Chrome is fullscreen, whatever state it started in. Success means the
                        // HUD is on the active Space in BOTH states.
                        toggle();
                        std::thread::sleep(Duration::from_millis(3000));
                        println!(
                            "FULLSCREEN_A chrome=[{}] screen=[{}] {}",
                            chrome_bounds(),
                            screen_bounds(),
                            focus_snapshot(&handle)
                        );
                        toggle();
                        std::thread::sleep(Duration::from_millis(3000));
                        println!(
                            "FULLSCREEN_B chrome=[{}] screen=[{}] {}",
                            chrome_bounds(),
                            screen_bounds(),
                            focus_snapshot(&handle)
                        );
                    }
                    let _ = std::io::stdout().flush();
                });
            }

            // Test hook: move the window once so the persist path can be proven headlessly.
            if std::env::var("STEALTH_TEST_MOVE").is_ok() {
                if let Some(win) = app.get_webview_window("hud") {
                    let _ = win.set_position(tauri::PhysicalPosition::new(120, 160));
                    println!("TEST_MOVE applied x=120 y=160");
                }
            }

            // Debounced writer: dragging fires Moved continuously, so persist after quiet.
            let flush_handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(700));
                let pending = take_geometry();
                if let Some(geo) = pending {
                    write_geometry(&flush_handle, geo);
                    println!("WINDOW_GEOMETRY_SAVED x={} y={}", geo.x, geo.y);
                }
            });

            // Reported without prompting, so the state is visible in the log even before a
            // session starts.
            #[cfg(target_os = "macos")]
            println!("MIC_PERMISSION startup_status={}", mic_permission::status().as_str());

            // Note: an accessory (agent) activation policy was measured against a genuinely
            // fullscreen Chrome and did NOT put the HUD on that Space either, so the app stays
            // a regular app.
            if let Some(win) = app.get_webview_window("hud") {
                apply_overlay_policy(&win);
            }

            // Watchdog: the level is reverted during startup, so it is re-asserted on a timer.
            // This is also what keeps the HUD above a window that later takes focus.
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(1500));
                    enforce_overlay_presence(&handle);
                });
            }

            // A click-through HUD must never depend on a single global hotkey to be unlocked:
            // if the shortcut is swallowed, there is no way back in. The menu bar item is
            // always clickable regardless of the window's click-through state.
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::TrayIconBuilder;

                let edit = MenuItem::with_id(
                    app,
                    "edit",
                    "Unlock / Lock HUD  (\u{2318}\u{21e7}\u{2325}L)",
                    true,
                    None::<&str>,
                )?;
                let show = MenuItem::with_id(app, "show", "Show / Hide HUD", true, None::<&str>)?;
                let center = MenuItem::with_id(app, "center", "Center Window on Screen", true, None::<&str>)?;
                let script_call1 = MenuItem::with_id(app, "script_call1", "Script: Call 1 (10-Min Audit)", true, None::<&str>)?;
                let script_call2 = MenuItem::with_id(app, "script_call2", "Script: Call 2 (Presentation & Close)", true, None::<&str>)?;
                let script_coldcall = MenuItem::with_id(app, "script_coldcall", "Script: Cold Call (Scalix Elite)", true, None::<&str>)?;
                let nudge_up = MenuItem::with_id(app, "up", "Previous line", true, None::<&str>)?;
                let nudge_down = MenuItem::with_id(app, "down", "Next line", true, None::<&str>)?;
                let quit =
                    MenuItem::with_id(app, "quit", "Quit StealthPrompter", true, None::<&str>)?;
                let menu = Menu::with_items(
                    app,
                    &[
                        &edit,
                        &show,
                        &center,
                        &script_call1,
                        &script_call2,
                        &script_coldcall,
                        &nudge_up,
                        &nudge_down,
                        &quit,
                    ],
                )?;

                let tray = TrayIconBuilder::with_id("main")
                    .icon(app.default_window_icon().expect("app icon").clone())
                    .tooltip("StealthPrompter")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "edit" => toggle_edit_mode(app),
                        "show" => toggle_visibility(app),
                        "center" => {
                            if let Some(win) = app.get_webview_window("hud") {
                                let _ = win.center();
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                        "script_call1" => {
                            let state = app.state::<AppState>();
                            let _ = do_select_script(app, &state.tracker, "call1");
                        }
                        "script_call2" => {
                            let state = app.state::<AppState>();
                            let _ = do_select_script(app, &state.tracker, "call2");
                        }
                        "script_coldcall" => {
                            let state = app.state::<AppState>();
                            let _ = do_select_script(app, &state.tracker, "coldcall");
                        }
                        "up" => nudge_from_hotkey(app, -1),
                        "down" => nudge_from_hotkey(app, 1),
                        "quit" => app.exit(0),
                        other => eprintln!("TRAY_UNKNOWN {other}"),
                    })
                    .build(app)?;
                if let Ok(mut guard) = TRAY.lock() {
                    *guard = Some(tray);
                }
                refresh_tray();
                println!("TRAY_READY menu bar item installed");
            }

            if let Err(err) = register_hotkeys(app.handle()) {
                eprintln!("HOTKEY_ERROR registration failed: {err}");
            }

            bare_keys::attach(app.handle().clone());
            if let Err(err) = bare_keys::start() {
                println!("BARE_KEYS_INIT: {err}");
            }

            // Loopback webhook so the local dialer (Pulse Dialer) can push the prospect's
            // name/company/niche when a call starts. Non-fatal if the port is unavailable.
            context_server::spawn(app.handle().clone());

            // Loopback dialer intake on port 47831 (POST /lead)
            {
                let handle_lead = app.handle().clone();
                let port = std::env::var("STEALTH_LEAD_PORT")
                    .ok()
                    .and_then(|v| v.parse::<u16>().ok())
                    .unwrap_or(lead::DEFAULT_PORT);
                let token = std::env::var("STEALTH_LEAD_TOKEN")
                    .ok()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                match lead::spawn(port, token, move |incoming| {
                    let updated = context_server::update_lead(
                        incoming.name.filter(|n| !n.is_empty()),
                        incoming.company.filter(|c| !c.is_empty()),
                        incoming.niche.filter(|ni| !ni.is_empty()),
                        incoming.employees.filter(|e| !e.is_empty()),
                    );
                    let _ = handle_lead.emit("call_context", &updated);
                }) {
                    Ok(bound) => println!("LEAD_READY http://127.0.0.1:{bound}/lead"),
                    Err(err) => eprintln!("LEAD_LISTENER_ERROR {err} (dialer intake disabled)"),
                }
            }

            // Headless verification hook: start without a human clicking Start.
            if std::env::var("STEALTH_AUTOSTART").is_ok() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1200));
                    match begin_session(&handle, None, None) {
                        Ok(info) => println!("AUTOSTART_OK device={}", info.device.device),
                        Err(err) => eprintln!("AUTOSTART_FAILED {err}"),
                    }
                });
            }
            // Headless teardown hook, so "does Stop actually stop?" is testable.
            if let Some(ms) = std::env::var("STEALTH_AUTOSTOP_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(ms));
                    println!("AUTOSTOP_FIRED after_ms={ms}");
                    end_session(&handle);
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod arbitration_tests {
    use super::*;
    use crate::tracker::{load_script_by_type, Tracker};

    fn call1_tracker() -> Tracker {
        let script = load_script_by_type("call1").expect("call1 fixture loads");
        Tracker::new(script.lines).expect("tracker builds")
    }

    #[test]
    fn fast_talker_run_on_final_preserves_line_two_opening_instead_of_dropping() {
        let mut tr = call1_tracker();
        assert_eq!(tr.active_id(), 1);

        // 1. Interim finishes Line 1 live -> advances to Line 2 with last_completed = (1, now)
        let out1 = ingest_transcript(
            &mut tr,
            "good to meet you appreciate you carving out ten minutes",
            false,
        );
        assert!(matches!(out1, IngestOutcome::Committed(2, "line-spoken")));
        assert_eq!(tr.active_id(), 2);

        // 2. Fast-talker combined final arrives containing Line 1 + opening half of Line 2.
        // Previously `is_stale_echo` dropped the ENTIRE transcript, starving Line 2.
        let combined_final = "good to meet you appreciate you carving out ten minutes just to set the stage so we use our time well";
        let _ = ingest_transcript(&mut tr, combined_final, true);
        assert_eq!(tr.active_id(), 2);
        assert!(
            tr.spoken_buffer().contains("stage") && !tr.spoken_buffer().contains("carving"),
            "must preserve Line 2 tail while stripping Line 1 echo: '{}'",
            tr.spoken_buffer()
        );

        // 3. Second half of Line 2 completes cleanly -> Line 3
        let out3 = ingest_transcript(&mut tr, "I'm not here to pitch you anything today", true);
        assert!(matches!(out3, IngestOutcome::Committed(3, "line-completed")));
        assert_eq!(tr.active_id(), 3);
    }

    #[test]
    fn pure_delayed_echo_after_live_advance_is_dropped_cleanly() {
        let mut tr = call1_tracker();
        let out1 = ingest_transcript(
            &mut tr,
            "good to meet you appreciate you carving out ten minutes",
            false,
        );
        assert!(matches!(out1, IngestOutcome::Committed(2, "line-spoken")));

        // Pure delayed final of Line 1 must not pollute Line 2's buffer
        let out2 = ingest_transcript(
            &mut tr,
            "good to meet you appreciate you carving out ten minutes",
            true,
        );
        assert!(matches!(out2, IngestOutcome::Idle));
        assert_eq!(tr.spoken_buffer(), "");
        assert_eq!(tr.active_id(), 2);
    }

    #[test]
    fn run_on_interim_after_live_advance_seeds_next_line_partial() {
        let mut tr = call1_tracker();
        let out = ingest_transcript(
            &mut tr,
            "good to meet you appreciate you carving out ten minutes just to set the stage",
            false,
        );
        assert!(matches!(out, IngestOutcome::Committed(2, "line-spoken")));
        assert_eq!(tr.active_id(), 2);
        assert!(
            tr.spoken_buffer().contains("stage") && !tr.spoken_buffer().contains("appreciate"),
            "run-on tail should immediately seed Line 2 partial: '{}'",
            tr.spoken_buffer()
        );
    }

    #[test]
    fn stage1_confident_distant_forward_jump_is_clamped_to_max_plus_two() {
        let mut tr = call1_tracker();
        assert_eq!(tr.active_id(), 1);

        // Distant forward jump (1 -> 15) must be clamped/held
        assert_eq!(apply_stage1_confident(&mut tr, 15), None);
        assert_eq!(tr.active_id(), 1);

        // Sequential +2 jump (1 -> 3) is allowed
        assert_eq!(
            apply_stage1_confident(&mut tr, 3),
            Some((3, "stage1-local"))
        );
        assert_eq!(tr.active_id(), 3);

        // Sequential +1 jump (3 -> 4) is allowed
        assert_eq!(
            apply_stage1_confident(&mut tr, 4),
            Some((4, "stage1-local"))
        );
        assert_eq!(tr.active_id(), 4);
    }

    #[test]
    fn stage1_confident_backward_jump_requires_three_confirmations() {
        let mut tr = call1_tracker();
        tr.jump_to(20);
        assert_eq!(tr.active_id(), 20);

        // Even a confident Stage 1 backward match cannot yank the highlight on 1 or 2 hits
        assert_eq!(apply_stage1_confident(&mut tr, 8), None);
        assert_eq!(tr.active_id(), 20);
        assert_eq!(apply_stage1_confident(&mut tr, 8), None);
        assert_eq!(tr.active_id(), 20);

        // 3rd consecutive confirmation commits intentionally
        assert_eq!(
            apply_stage1_confident(&mut tr, 8),
            Some((8, "stage1-local-backward"))
        );
        assert_eq!(tr.active_id(), 8);
    }

    #[test]
    fn llm_proposal_is_rejected_if_active_moved_while_in_flight() {
        let mut tr = call1_tracker();
        tr.jump_to(5);

        // Simulate LLM request started at active=3 returning proposal=4 after user jumped to 5
        assert_eq!(apply_llm_proposal(&mut tr, 3, 4, true), None);
        assert_eq!(tr.active_id(), 5);

        // Out-of-order LLM jump (5 -> 12) is rejected
        assert_eq!(apply_llm_proposal(&mut tr, 5, 12, true), None);
        assert_eq!(tr.active_id(), 5);

        // Valid sequential LLM proposal (5 -> 6) requires 2 confirmations (hysteresis)
        assert_eq!(apply_llm_proposal(&mut tr, 5, 6, true), None);
        assert_eq!(
            apply_llm_proposal(&mut tr, 5, 6, true),
            Some((6, "stage2-llm"))
        );
        assert_eq!(tr.active_id(), 6);
    }

    #[test]
    fn line_step_handles_non_contiguous_coldcall_ids() {
        let cold = load_script_by_type("coldcall").expect("coldcall fixture loads");
        // Cold call jumps from ID 1 to ID 3 (no ID 2); index step is still +1
        assert_eq!(line_step(&cold.lines, 1, 3), Some(1));
        assert_eq!(line_step(&cold.lines, 1, 4), Some(2));
        assert_eq!(line_step(&cold.lines, 1, 5), Some(3));
    }
}
