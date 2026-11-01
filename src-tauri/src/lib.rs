use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::sync::mpsc;

mod audio;
mod keys;
mod stt;
mod tracker;

#[cfg(target_os = "macos")]
mod overlay_macos;

use tracker::Tracker;

/// Click-through is ON by default: the HUD must never eat a click meant for Meet or the
/// browser. `Cmd+Shift+E` flips it off into edit mode (drag, click Start), because a
/// click-through window with no escape hatch cannot be interacted with at all.
static CLICK_THROUGH: AtomicBool = AtomicBool::new(true);

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
            " ignores_mouse_events={:?}",
            overlay_macos::ignores_mouse_events(win)
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
    // Click-through by default so the HUD never steals a click from the call UI.
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
    }

    println!("{}", report(win));
    let _ = std::io::stdout().flush();
}

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
    let _ = app.emit("edit_mode", !enabled);
    Ok(report(&window))
}

#[tauri::command]
fn hud_state() -> HudState {
    HudState {
        edit_mode: !CLICK_THROUGH.load(Ordering::Relaxed),
    }
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
            audio::replay_wav(&path, audio_tx.clone(), stop.clone())?
        }
        _ => spawn_capture(device, audio_tx.clone(), state.level.clone(), stop.clone())?,
    };
    *state.audio_tx.lock().unwrap() = Some(audio_tx);

    let (stt_tx, mut stt_rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(async move {
        stt::run(deepgram, audio_rx, stt_tx).await;
    });

    let tracker_arc = state.tracker.clone();
    let http = state.http.clone();
    let llm_key = deepseek.clone();
    let app_handle: tauri::AppHandle = (*app).clone();

    tauri::async_runtime::spawn(async move {
        let mut llm_failures: u32 = 0;
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
            if !transcript.is_final {
                continue;
            }
            println!("STT_FINAL \"{}\"", transcript.text);

            // Stage 0: only classify once enough new words have arrived.
            let plan = {
                let mut tr = tracker_arc.lock().unwrap();
                tr.push_text(&transcript.text);
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
                if let Some(key) = llm_key.as_ref() {
                    if llm_failures < 3 {
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
                        println!("TRACKER_ROUTE stage1-local (llm circuit open)");
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
            hud_state
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

            if let Some(win) = app.get_webview_window("hud") {
                apply_overlay_policy(&win);
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

                TrayIconBuilder::with_id("main")
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
