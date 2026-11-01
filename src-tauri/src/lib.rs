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
    stop: Arc<AtomicBool>,
    audio_tx: Mutex<Option<mpsc::UnboundedSender<Vec<i16>>>>,
    http: reqwest::Client,
}

#[derive(Clone, serde::Serialize)]
struct ActiveBlock {
    id: i32,
    phase: String,
    label: String,
    summary: String,
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
fn set_click_through(window: tauri::WebviewWindow, enabled: bool) -> Result<String, String> {
    window
        .set_ignore_cursor_events(enabled)
        .map_err(|e| e.to_string())?;
    CLICK_THROUGH.store(enabled, Ordering::Relaxed);
    Ok(report(&window))
}

fn emit_active(app: &tauri::AppHandle, tracker: &Arc<Mutex<Tracker>>, id: i32, reason: &str) {
    let guard = tracker.lock().unwrap();
    if let Some(block) = guard.blocks().iter().find(|b| b.id == id) {
        println!(
            "ACTIVE_BLOCK id={} label='{}' reason={reason}",
            block.id, block.label
        );
        let _ = app.emit(
            "active_block",
            ActiveBlock {
                id: block.id,
                phase: block.phase.clone(),
                label: block.label.clone(),
                summary: block.summary.clone(),
                body: block.body.clone(),
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

    state.stop.store(false, Ordering::Relaxed);
    let (audio_tx, audio_rx) = mpsc::unbounded_channel();
    // Simulated Call Mode: replay a 16 kHz mono WAV through the real Deepgram socket.
    let stats = match std::env::var("STEALTH_SIMULATE_WAV") {
        Ok(path) if !path.trim().is_empty() => audio::replay_wav(&path, audio_tx.clone())?,
        _ => spawn_capture(
            device,
            audio_tx.clone(),
            state.level.clone(),
            state.stop.clone(),
        )?,
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

            if let Some((id, true, _)) = local {
                // Stage 1 was unambiguous: commit without paying for a model call.
                if let Some(new_id) = tracker_arc.lock().unwrap().consider(id, true) {
                    committed = Some((new_id, "stage1-local"));
                }
            } else {
                // Stage 2: only reached when stage 1 is unsure.
                let mut proposal = local.map(|(id, _, _)| id);
                if let Some(key) = llm_key.as_ref() {
                    if llm_failures < 3 {
                        match tracker::classify_with_llm(&http, key, &candidates, active, &buffer)
                            .await
                        {
                            Ok(id) => {
                                llm_failures = 0;
                                proposal = Some(id);
                            }
                            Err(err) => {
                                llm_failures += 1;
                                eprintln!("TRACKER_LLM_ERROR n={llm_failures} {err}");
                            }
                        }
                    }
                }
                if let Some(id) = proposal {
                    if let Some(new_id) = tracker_arc.lock().unwrap().consider(id, false) {
                        committed = Some((new_id, "stage2-llm"));
                    }
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

#[tauri::command]
fn stop_session(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.stop.store(true, Ordering::Relaxed);
    // Dropping the sender ends the audio channel, which closes the Deepgram stream.
    state.audio_tx.lock().unwrap().take();
    emit_status(&app, false, "stopped");
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
fn script_blocks(state: tauri::State<'_, AppState>) -> Vec<tracker::ScriptBlock> {
    state.tracker.lock().unwrap().blocks().to_vec()
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
            println!("HOTKEY click_through={click_through}");
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
            script_blocks
        ])
        .setup(|app| {
            let script = tracker::load_script().unwrap_or_else(|e| panic!("{e}"));
            println!(
                "SCRIPT_LOADED '{}' blocks={}",
                script.title,
                script.blocks.len()
            );
            let tracker = Tracker::new(script.blocks).expect("tracker builds");

            app.manage(AppState {
                tracker: Arc::new(Mutex::new(tracker)),
                level: Arc::new(AtomicU32::new(0)),
                stop: Arc::new(AtomicBool::new(false)),
                audio_tx: Mutex::new(None),
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .expect("http client"),
            });

            if let Some(win) = app.get_webview_window("hud") {
                apply_overlay_policy(&win);
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
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
