use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::sync::mpsc;

mod audio;
mod keys;
#[cfg(target_os = "macos")]
mod mic_permission;
mod stt;
mod tracker;

#[cfg(target_os = "macos")]
mod overlay_macos;

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
fn cmd_shift() -> Modifiers {
    Modifiers::SHIFT | Modifiers::SUPER
}

/// Live session state, owned by Tauri and shared with the async event loop.
struct AppState {
    tracker: Arc<Mutex<Tracker>>,
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
    deepgram: bool,
    deepseek: bool,
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
    let guard = tracker.lock().unwrap();
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

/// Starts a session. Kept separate from the command so the app can also autostart
/// headlessly during verification (`STEALTH_AUTOSTART=1`).
fn begin_session(app: &tauri::AppHandle, device: Option<String>) -> Result<SessionInfo, String> {
    let state = app.state::<AppState>();

    if state.audio_tx.lock().unwrap().is_some() {
        return Err("a session is already running".to_string());
    }

    let deepgram =
        keys::get("DEEPGRAM_API_KEY").ok_or("DEEPGRAM_API_KEY not found in env or Keychain")?;
    let deepseek = keys::get("DEEPSEEK_API_KEY");
    println!(
        "KEYS deepgram={} deepseek={}",
        keys::mask(&deepgram),
        deepseek
            .as_deref()
            .map(keys::mask)
            .unwrap_or_else(|| "missing".to_string())
    );

    let stop = Arc::new(AtomicBool::new(false));
    *state.stop.lock().unwrap() = Some(stop.clone());

    let (audio_tx, audio_rx) = mpsc::unbounded_channel();
    // Simulated Call Mode: replay a 16 kHz mono WAV through the real Deepgram socket.
    let stats = match std::env::var("STEALTH_SIMULATE_WAV") {
        Ok(path) if !path.trim().is_empty() => {
            audio::replay_wav(&path, audio_tx.clone(), stop.clone(), state.level.clone())?
        }
        _ => {
            // Refuse to open a stream that macOS will silently zero out.
            #[cfg(target_os = "macos")]
            mic_permission::ensure()?;
            spawn_capture(device, audio_tx.clone(), state.level.clone(), stop.clone())?
        }
    };
    *state.audio_tx.lock().unwrap() = Some(audio_tx);

    let (stt_tx, mut stt_rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(async move {
        stt::run(deepgram, audio_rx, stt_tx).await;
    });

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
    let http = state.http.clone();
    let llm_key = deepseek.clone();
    let app_handle: tauri::AppHandle = (*app).clone();

    tauri::async_runtime::spawn(async move {
        let mut llm_failures: u32 = 0;
        let mut last_llm: Option<std::time::Instant> = None;
        while let Some(event) = stt_rx.recv().await {
            let transcript = match event {
                stt::SttEvent::Connected => {
                    println!("STT_READY");
                    continue;
                }
                stt::SttEvent::Error(err) => {
                    eprintln!("STT_EVENT_ERROR {err}");
                    continue;
                }
                stt::SttEvent::Transcript(t) => t,
            };

            let _ = app_handle.emit("transcript", &transcript);

            // Stage 0: interims count too. Waiting for Deepgram to finalize the sentence is
            // exactly what made the highlight arrive when the line was mostly already spoken.
            let plan = {
                let mut tr = tracker_arc.lock().unwrap();
                if transcript.is_final {
                    println!("STT_FINAL \"{}\"", transcript.text);
                    tr.push_text(&transcript.text);
                    tr.clear_partial();
                } else {
                    tr.set_partial(&transcript.text);
                }
                if tr.should_run_cycle() {
                    tr.mark_cycle_ran();
                    Some((
                        tr.local_match(),
                        tr.spoken_buffer(),
                        tr.active_id(),
                        tr.candidate_window(),
                    ))
                } else {
                    None
                }
            };
            let Some((local, buffer, active, candidates)) = plan else {
                continue;
            };

            let mut committed: Option<(i32, &'static str)> = None;

            if tracker::needs_llm(local) {
                // Stage 2: stage 1 was unsure — a paraphrase, off-script speech, or ambiguity.
                // Only the model may propose here. Falling back to the unconfident local guess
                // would let two weak guesses confirm each other and advance the block on
                // off-script speech, which is exactly what the model is meant to prevent.
                let mut proposal: Option<i32> = None;
                let mut decided_by_llm = false;
                // Interims make cycles frequent; without this throttle an ambiguous buffer
                // would fire a model request every few hundred milliseconds.
                let llm_ready =
                    last_llm.map_or(true, |at| at.elapsed() >= Duration::from_millis(700));
                if let Some(key) = llm_key.as_ref() {
                    if llm_failures < 3 && llm_ready {
                        last_llm = Some(std::time::Instant::now());
                        println!(
                            "TRACKER_ROUTE stage2-llm active={active} buffer_words={}",
                            buffer.split_whitespace().count()
                        );
                        match tracker::classify_with_llm(&http, key, &candidates, active, &buffer)
                            .await
                        {
                            Ok(id) => {
                                llm_failures = 0;
                                proposal = Some(id);
                                decided_by_llm = true;
                                println!("TRACKER_LLM_OK active={id}");
                            }
                            Err(err) => {
                                llm_failures += 1;
                                eprintln!("TRACKER_LLM_ERROR n={llm_failures} {err}");
                            }
                        }
                    } else {
                        println!("TRACKER_ROUTE stage1-local (llm throttled or circuit open)");
                    }
                } else {
                    println!("TRACKER_ROUTE stage1-local (no deepseek key)");
                }
                if let Some(id) = proposal {
                    if let Some(new_id) = tracker_arc.lock().unwrap().consider(id, false) {
                        committed = Some((
                            new_id,
                            if decided_by_llm {
                                "stage2-llm"
                            } else {
                                "stage1-local"
                            },
                        ));
                    }
                } else {
                    // Ambiguous or off-script and no model verdict: hold position.
                    println!(
                        "TRACKER_RETAIN active={active} reason=no-model-decision buffer_words={}",
                        buffer.split_whitespace().count()
                    );
                }
            } else if let Some((id, _, _)) = local {
                // Stage 1 was unambiguous: commit without paying for a model call.
                if let Some(new_id) = tracker_arc.lock().unwrap().consider(id, true) {
                    committed = Some((new_id, "stage1-local"));
                }
            }

            if let Some((id, reason)) = committed {
                emit_active(&app_handle, &tracker_arc, id, reason);
            }
        }
        println!("SESSION_LOOP_ENDED");
    });

    emit_status(app, true, format!("listening on {}", stats.device));

    Ok(SessionInfo {
        device: stats,
        deepgram: true,
        deepseek: deepseek.is_some(),
    })
}

#[tauri::command]
async fn start_session(
    app: tauri::AppHandle,
    device: Option<String>,
) -> Result<SessionInfo, String> {
    begin_session(&app, device)
}

/// Ends a session. Shared by the command and the headless autostop hook so the loop
/// exercises the same path a user's click does.
fn end_session(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    // Take this session's flag and raise it. Because the flag belongs to the session, a
    // following session cannot clear it before this producer has seen it.
    if let Some(stop) = state.stop.lock().unwrap().take() {
        stop.store(true, Ordering::Relaxed);
    }
    // Dropping the last remaining sender closes the audio channel, which makes the STT
    // task send CloseStream and return.
    state.audio_tx.lock().unwrap().take();
    emit_status(app, false, "stopped");
}

#[tauri::command]
fn stop_session(app: tauri::AppHandle) -> Result<(), String> {
    end_session(&app);
    Ok(())
}

#[tauri::command]
fn list_devices() -> Result<Vec<String>, String> {
    audio::list_input_devices()
}

#[tauri::command]
fn key_status() -> Vec<keys::KeyStatus> {
    vec![
        keys::status("DEEPGRAM_API_KEY"),
        keys::status("DEEPSEEK_API_KEY"),
    ]
}

#[tauri::command]
fn tracker_snapshot(state: tauri::State<'_, AppState>) -> tracker::TrackerSnapshot {
    state.tracker.lock().unwrap().snapshot()
}

#[tauri::command]
fn nudge(app: tauri::AppHandle, state: tauri::State<'_, AppState>, delta: i32) -> i32 {
    let id = state.tracker.lock().unwrap().manual(delta);
    emit_active(&app, &state.tracker, id, "manual");
    id
}

#[tauri::command]
fn audio_level(state: tauri::State<'_, AppState>) -> f32 {
    f32::from_bits(state.level.load(Ordering::Relaxed))
}

#[tauri::command]
fn script_lines(state: tauri::State<'_, AppState>) -> Vec<tracker::ScriptLine> {
    state.tracker.lock().unwrap().lines().to_vec()
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
    let id = state.tracker.lock().unwrap().manual(delta);
    println!("HOTKEY nudge delta={delta} active={id}");
    emit_active(app, &state.tracker, id, "manual-hotkey");
}

/// Registers the global shortcuts. These are process-wide, so they must stay a small,
/// deliberate set — every one of them steals the combination from every other app.
fn register_hotkeys(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let shortcuts = [
        Shortcut::new(Some(cmd_shift()), Code::KeyH),
        Shortcut::new(Some(cmd_shift()), Code::KeyE),
        Shortcut::new(Some(cmd_shift()), Code::ArrowUp),
        Shortcut::new(Some(cmd_shift()), Code::ArrowDown),
    ];
    app.global_shortcut()
        .register_multiple(shortcuts)
        .map_err(|err| Box::<dyn std::error::Error>::from(err.to_string()))?;
    println!("HOTKEYS registered cmd+shift+H hide, +E edit mode, +Up/+Down nudge");
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_file_logging();
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed || shortcut.mods != cmd_shift() {
                        return;
                    }
                    match shortcut.key {
                        Code::KeyH => toggle_visibility(app),
                        Code::KeyE => toggle_edit_mode(app),
                        Code::ArrowUp => nudge_from_hotkey(app, -1),
                        Code::ArrowDown => nudge_from_hotkey(app, 1),
                        _ => {}
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
            audio_level,
            script_lines,
            hud_state,
            input_probe,
            drag_probe,
            open_mic_settings
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
            let script = tracker::load_script().unwrap_or_else(|e| panic!("{e}"));
            println!(
                "SCRIPT_LOADED '{}' lines={}",
                script.title,
                script.lines.len()
            );
            let tracker = Tracker::new(script.lines).expect("tracker builds");

            app.manage(AppState {
                tracker: Arc::new(Mutex::new(tracker)),
                level: Arc::new(AtomicU32::new(0)),
                stop: Mutex::new(None),
                audio_tx: Mutex::new(None),
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .expect("http client"),
            });

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
                            let _ = win.set_position(tauri::PhysicalPosition::new(geo.x, geo.y));
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
                    "Unlock / Lock HUD (edit mode)",
                    true,
                    None::<&str>,
                )?;
                let show = MenuItem::with_id(app, "show", "Show / Hide HUD", true, None::<&str>)?;
                let nudge_up = MenuItem::with_id(app, "up", "Previous line", true, None::<&str>)?;
                let nudge_down = MenuItem::with_id(app, "down", "Next line", true, None::<&str>)?;
                let quit =
                    MenuItem::with_id(app, "quit", "Quit StealthPrompter", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&edit, &show, &nudge_up, &nudge_down, &quit])?;

                let tray = TrayIconBuilder::with_id("main")
                    .icon(app.default_window_icon().expect("app icon").clone())
                    .tooltip("StealthPrompter")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "edit" => toggle_edit_mode(app),
                        "show" => toggle_visibility(app),
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

            // Headless verification hook: start without a human clicking Start.
            if std::env::var("STEALTH_AUTOSTART").is_ok() {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1200));
                    match begin_session(&handle, None) {
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
