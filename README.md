# StealthPrompter

A transparent, click-through, screen-share-invisible HUD for macOS that follows your sales script in real time as you talk. It listens to your microphone, transcribes speech with zero latency, and moves the highlight to the exact line you are delivering — through paraphrase, tangents, pauses, and interruptions — with no mouse clicking required.

The window is excluded from screen capture (`NSWindowSharingNone`), so your script is completely invisible on Zoom, Google Meet, Microsoft Teams, Loom, and OBS.

Built with **Tauri v2.12** (Rust + Objective-C) + **React 19** / **Vite 8** / **TypeScript**.

---

## What changed (latest)

| Area | Change |
|---|---|
| **Bare number keys** | `1`–`9` / `0` show the matching objection card with **no Control key**. Toggle `1–0 Bare keys` in the HUD. Works only while listening; needs Accessibility. |
| **Prospect audio** | The `👂 Prospect audio` toggle transcribes the dialer call and opens the matching card by itself (keyword match, ~1–2 s). Needs Speech + Screen & System Audio Recording. |
| **Speed** | High-frequency updates are isolated and `font-size` is no longer animated. Idle: WebKit GPU ~0% (was ~100%), app 0.3% CPU / 28 MB. |
| **Dialer link** | Hardened only: browser requests get `403`, malformed JSON `400`, one thread per connection. Behaviour is unchanged, and the dialer repo was not touched. |

Permissions are granted once per install — and again after each rebuild, because the app is signed
ad-hoc. Deeper notes: [`HANDOFF.md`](HANDOFF.md).

---

## Status & Capabilities

| Area | Engine / Mode | State |
|---|---|---|
| **Local STT (Default)** | **Apple On-Device Speech Recognition** (Neural Engine) | **Production-ready** (~20–50 ms latency, 100% offline, $0 cost) |
| **Cloud STT** | **Deepgram Nova-3** (streaming WebSocket) | **Production-ready** (endpointing=100, keep-alive 4s, resilient backoff) |
| **Microphone Input** | **CrystalSound Microphone** (AI noise cancel) / Hardware Mics | **Working** (automatic 48 kHz stereo → 16 kHz mono linear PCM resampling) |
| **Tracking Pipeline** | **Two-Stage Semantic Tracker** + Utterance Auto-Advance | **Working** (interims trigger real-time lexical match; pauses trigger line completion) |
| **Echo & Hallucination Guard** | Timestamped suppression + Monotonic boundary clamps | **Working** (backward moves require 3 confirmations; echo window prevents bounce-back) |
| **Sales Scripts** | Call 1 (10-Min Audit) & Call 2 (20-Min Close) | **Working** (includes Alex Hormozi vocal cues: ⬇️ DOWN, 🐢 SLOW, ⏸️ PAUSE, ⚡ PACE UP) |
| **Screen Stealth** | `NSWindowSharingNone` (0) + Level 25 (`NSStatusWindowLevel`) | **Working** (excluded from screen shares & recordings across all desktop Spaces) |
| **Code Signing** | Ad-hoc signing (`"-"`) with entitlements | **Working** (zero Keychain password popups) |

---

## Architecture Overview

```
                                Microphone Input
                 (e.g. CrystalSound AI Noise-Cancelled Mic / Hardware)
                                       │
                                       ▼
                       Resampler (audio.rs) ──► 16 kHz mono i16
                                       │
                      ┌────────────────┴────────────────┐
                      ▼                                 ▼
           [Apple STT Engine]                 [Deepgram STT Engine]
         (apple_speech.m / apple_stt.rs)             (stt.rs)
      SFSpeechRecognizer (Neural Engine)       Nova-3 WebSocket
      requiresOnDeviceRecognition = YES        KeepAlive = 4s, endpointing = 100
      Latency: ~20-50ms | $0 / Local          Latency: ~200-300ms | Cloud
                      │                                 │
                      └────────────────┬────────────────┘
                                       ▼
                     Transcript Stream (SttEvent)
                     • Interim results (is_final = false)
                     • Finalized utterances (is_final = true)
                                       │
                                       ▼
                     Tracker Engine (tracker.rs)
     ┌─────────────────────────────────────────────────────────────┐
     │ 1. Interim text ──► Stage 0 (partial tail) ──► Stage 1 Match│
     │ 2. Finalized utterance ──► on_utterance_final()             │
     │    Matches active line? ──► Advances to next line in pause! │
     │ 3. Echo guard (is_completed_echo) drops trailing echoes     │
     │ 4. Backward moves require 3 distinct confirmations          │
     │ 5. Stage 2 LLM (Jev AI)  bounded to active+1 .. active+3   │
     └─────────────────────────────────────────────────────────────┘
                                       │
                                       ▼
                           Tauri App State & Events
                      (active_block, transcript, status)
                                       │
                                       ▼
                         React HUD Webview (App.tsx)
     ┌─────────────────────────────────────────────────────────────┐
     │ • Call 1 / Call 2 Script Playbook Selector                  │
     │ • STT Engine Toggle: [ 🍏 Apple STT | ☁️ Deepgram ]         │
     │ • CrystalSound Microphone Selector                          │
     │ • Real-time RMS Audio Level Meter                           │
     │ • Centered Smooth Auto-Scroll Viewport                      │
     │ • Alex Hormozi Delivery Cues ([⬇️ DOWN], [🐢 SLOW & LOW])   │
     └─────────────────────────────────────────────────────────────┘
```

---

## Key Features & How They Work

### 1. Apple On-Device Speech Recognition (Neural Engine)
- **Zero Cloud Latency & Zero Cost**: Bridges directly to Apple's native `Speech.framework` via an Objective-C ARC bridge ([`apple_speech.m`](file:///Users/gaganrajput11/teleprompter/src-tauri/src/apple_speech.m)) and Rust driver ([`apple_stt.rs`](file:///Users/gaganrajput11/teleprompter/src-tauri/src/apple_stt.rs)).
- **Hardware Acceleration**: Automatically activates the on-device acoustic model (`requiresOnDeviceRecognition = YES`), executing transcription directly on Apple Silicon Neural Engine cores at ~20–50 ms latency.
- **Continuous Session Rotation**: Apple's native audio buffer requests can degrade or accumulate memory over lengthy calls. StealthPrompter automatically detects sentence endpointing during speaker pauses (~700 ms silence), emits the finalized transcript, and seamlessly rotates to a fresh `SFSpeechAudioBufferRecognitionRequest` without dropping any incoming audio.
- **Microphone & Speech TCC Protection**: Includes `NSSpeechRecognitionUsageDescription` and `NSMicrophoneUsageDescription` in [`Info.plist`](file:///Users/gaganrajput11/teleprompter/src-tauri/Info.plist), paired with one-click direct system preference openers (`open_speech_settings` / `open_mic_settings`).

### 2. Deepgram Nova-3 Cloud STT
- **Fast Cloud Transcription**: High-accuracy cloud fallback connecting via streaming WebSocket to Deepgram's Nova-3 model.
- **Endpointing Tuning**: Configured with `endpointing=100` and `utterance_end_ms=1000`, resolving HTTP 400 rejection issues while maintaining rapid turnarounds.
- **Resilient Connection**: Sends heartbeat keep-alive messages every 4 seconds to prevent socket disconnects during long conversational pauses, with exponential backoff auto-reconnection.

### 3. Dual Engine Switcher
- Choose between **Apple STT (Local)** and **Deepgram (Cloud)** anytime:
  - From the pre-call selection modal (`call-modal`).
  - Directly from the HUD header bar via the `[ 🍏 Apple | ☁️ Deepgram ]` pill.
- Remembers your selection across launches in `localStorage` (`tp.engine`), defaulting to **Apple STT**.

### 4. CrystalSound AI Noise Cancellation Integration
- Pre-selects `CrystalSound Microphone` by default.
- By stripping room echo, keyboard clicks, and background noise before the audio hits the acoustic model, CrystalSound delivers clean phonemes to Apple's Neural Engine.
- Silence detection operates with ultra-clean thresholds (RMS < 0.003 during silence vs. > 0.02 during speech), making pause detection snappy and accurate.

### 5. Deterministic Utterance Completion Auto-Advance
- **The Problem Solved**: Previously, scripts either lagged behind speech or jumped abruptly mid-sentence when hitting a 50% match heuristic.
- **The Solution**:
  - While speaking, interim transcripts (`is_final = false`) feed into Stage 1 lexical matching to highlight active words.
  - When you finish speaking a line and pause, the recognizer emits a finalized utterance (`is_final = true`).
  - `tracker.on_utterance_final(text)` checks if the completed utterance satisfied the active line.
  - If matched, the HUD immediately steps `active_id -> active_id + 1` **during the pause**! The next sentence is in front of your eyes before you even begin delivering it.

### 6. Hallucination & Echo Suppression Guards
- **Echo Suppression Window (`is_completed_echo`)**: Trailing finalized packets from an already-completed line are timestamped and suppressed for 4 seconds, stopping the tracker from bouncing back to Line 1 after reaching Line 2.
- **Monotonic Confirmation Bias**: Moving backwards requires 3 consecutive confirmations, preventing incidental word matches from pulling the prompt backwards.
- **LLM Boundary Clamps**: Stage 2 LLM proposals are restricted strictly to immediate forward candidates `(active + 1 .. active + 3)`. Ambiguous or off-script banter holds position rather than jumping to random distant script blocks.

### 7. Alex Hormozi Sales Delivery Playbooks
StealthPrompter comes bundled with two complete, ready-to-use sales playbooks:
1. **Call 1: The 10-Minute Audit** (`call1_audit_script.json` — 31 lines)
   - Phase 1: Set the Frame (Consultative agreement)
   - Phase 2: The 4 Discovery Buckets (Past projects, lead flow, personal bottleneck, budget)
   - Phase 3: Clinical Diagnosis (Naming the exact cost of inaction)
   - Phase 4: Lock Call 2 (Booking the prototype walkthrough)
2. **Call 2: 20-Min Prototype & Close** (`call2_sales_script.json` — 25 lines)
   - Phase 1: Pain Re-Anchor (Reviewing Call 1 findings)
   - Phase 2: 3 Live Prototype Pillars (Interactive solution demo)
   - Phase 3: Soft Close & Decision Frame
   - Phase 4: Starbucks Cashier Pitch (Natural pricing delivery)

Each line features **Alex Hormozi Vocal Cues**:
- `[⬇️ DOWN]`: Downward vocal inflection on certainty statements.
- `[🐢 SLOW & LOW]`: Measured, deep cadence for high-gravity terms.
- `[⏸️ PAUSE 1s]` / `[⏸️ PAUSE 2s]`: Conversational pregnant pauses for buyer absorption.
- `[⚡ PACE UP]`: Energy acceleration through familiar concepts.
- `[🤔 CURIOUS]`: Upward tonal lift for inquisitive questions.

---

## Quick Start

### Prerequisites
- macOS 11+ (Apple Silicon M1/M2/M3/M4 recommended for on-device Neural Engine; Intel supported)
- Node.js 20+ and npm
- Rust stable (`rustup`) — <https://rustup.rs>

### 1. Installation

```bash
git clone https://github.com/gaganrajput11/teleprompter.git
cd teleprompter
npm install
```

### 2. Running in Development

```bash
npm run tauri dev
```

### 3. API Keys (Optional if using Apple STT)
When using **Apple STT**, no API keys are required.
If you wish to use Deepgram or Jev (OpenRouter) tie-breaking, store your keys in the macOS Keychain or environment:

```bash
security add-generic-password -a stealthprompter -s DEEPGRAM_API_KEY -w "<your-deepgram-key>" -U
security add-generic-password -a stealthprompter -s JEV_API_KEY -w "<your-openrouter-key>" -U
```

Stage 2 uses **Jev**, TypeSafe's "System One" decision model, reached through OpenRouter. Any
OpenRouter API key works; the app also reads `OPENROUTER_API_KEY` if `JEV_API_KEY` is unset.

### 4. Production Build & Deployment

StealthPrompter uses ad-hoc code signing (`"-"`), eliminating all macOS Keychain password prompts during compilation:

```bash
npm run tauri build
cp -R src-tauri/target/release/bundle/macos/StealthPrompter.app /Applications/
open /Applications/StealthPrompter.app
```

---

## Shortcuts & Controls

| Action | Shortcut / Input | Behavior |
|---|---|---|
| **Toggle Lock (Click-Through)** | `⌘⇧⌥L` or Menu Bar item | Locks HUD so mouse clicks pass straight through to background windows |
| **Show / Hide HUD** | `⌘⇧⌥H` or Menu Bar item | Toggles HUD visibility instantly |
| **Manual Line Nudge** | `ArrowUp` / `ArrowDown` or `⌘⇧⌥↑` / `⌘⇧⌥↓` | Manually moves the active script position forward or backward |
| **Switch Call** | Click `🔍 Audit (Call 1) ▾` in header | Returns to playbook selector to pick Call 1 or Call 2 |
| **Toggle STT Engine** | Click `[🍏 Apple]` / `[☁️ Deepgram]` | Toggles between local Apple Neural Engine and Deepgram cloud |
| **Objection Card** | `⌃1`–`⌃9`, `⌃0` | Shows that branch's rebuttal; the script position never moves |
| **Bare Objection Keys** | `1`–`9`, `0` (toggle in HUD) | Same cards with no Control key — only while a session is listening |
| **Prospect Audio** | `👂 Prospect audio` toggle | Transcribes the call and suggests the objection card automatically |
| **Re-sync / Dismiss** | `⌘⇧⌥R` / `⌘⇧⌥D` | Re-anchors at the current line / closes the objection card |
| **Reposition HUD** | Click and drag grab bar (when unlocked) | Moves HUD across screens; geometry persists automatically |

---

## Testing & Quality Assurance

StealthPrompter is backed by comprehensive unit and integration test suites:

```bash
# Run frontend unit tests (Vitest)
npm run test

# Typecheck frontend
npm run build

# Run Rust backend test suite (75 tests covering audio, tracker, Jev parsing, keys, STT, objections)
cargo test --manifest-path src-tauri/Cargo.toml

# Lint Rust codebase
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

### Simulated Call Testing (Offline Replay)
You can test the entire tracking pipeline deterministically using pre-recorded or synthetic audio fixtures:

```bash
STEALTH_AUTOSTART=1 STEALTH_SIMULATE_WAV=/path/to/audio.wav npm run tauri dev
```

---

## Security & Privacy Architecture

- **Zero Cloud Audio (Apple STT)**: When running Apple STT, speech recognition is executed entirely on-device by macOS. No audio data ever leaves your computer.
- **Ephemerality (Deepgram STT)**: When using Deepgram STT, audio is streamed over TLS directly to Deepgram's streaming endpoint and is **never written to disk**.
- **Screen Capture Exclusion**: The HUD window is permanently flagged with `NSWindowSharingNone`. Screen recording tools (Zoom, Google Meet, Teams, QuickTime, OBS) record the windows behind the HUD as if StealthPrompter were completely invisible.
- **Secure Key Storage**: API tokens are fetched from the macOS Keychain at runtime and masked in logs (`KEYS deepgram=32a8…5425`).


---

## Architecture

```
src-tauri/
  src/
    lib.rs            1330  app wiring, commands, session lifecycle, overlay policy, tray, hotkeys
    tracker.rs         939  two-stage line tracker, utterance completion auto-advance, cue metadata
    audio.rs           394  cpal capture, resampler, WAV replay for Simulated Call Mode
    overlay_macos.rs   335  NSWindow policy via objc2: capture exclusion, Spaces, level, probes
    apple_stt.rs       250  Apple on-device Speech framework engine & silence endpointing
    apple_speech.m     226  Objective-C ARC bridge to Speech.framework (SFSpeechRecognizer)
    stt.rs             240  Deepgram Nova-3 streaming client
    mic_permission.rs  108  TCC microphone permission preflight and prompt
    keys.rs            100  environment → Keychain key lookup
    main.rs              5  entry point
  build.rs              18  build script compiling apple_speech.m with -fobjc-arc & linking frameworks
  Info.plist            14  NSMicrophoneUsageDescription & NSSpeechRecognitionUsageDescription
  Entitlements.plist     9  com.apple.security.device.audio-input

src/
  App.tsx              444  HUD: Call 1/Call 2 selector, Apple/Deepgram toggle, highlight, cues, drag
  lib/script.ts        124  script loading and body derivation
  lib/cues.ts           40  cue metadata → badge chips
  fixtures/
    call1_audit_script.json  Call 1: The 10-Minute Audit (31 lines, 4 phases)
    call2_sales_script.json  Call 2: 20-Min Prototype & Close (25 lines, 4 phases)
  lib/*.test.ts        147  vitest suites (24 tests)
```

### Tauri Commands

`start_session`, `stop_session`, `list_devices`, `key_status`, `script_lines`, `select_script`,
`tracker_snapshot`, `nudge`, `audio_level`, `hud_state`, `hud_report`, `set_click_through`,
`open_mic_settings`, `open_speech_settings`, `input_probe`, `drag_probe`.

### Files on Disk

| Path | Contents |
|---|---|
| `~/Library/Application Support/com.gaganrajput11.stealthprompter/window.json` | window position/size, written 700 ms after a move settles |
| `~/Library/Logs/StealthPrompter.log` | mirror of stdout/stderr when launched from Finder |
| macOS Keychain | `DEEPGRAM_API_KEY`, `JEV_API_KEY` (or `OPENROUTER_API_KEY`) |

Log mirroring exists because a Finder-launched `.app` has no terminal, so every diagnostic went
nowhere — a large part of why a silent microphone or speech recognition issue took so long to diagnose.

---

## The Overlay: What Makes It "Stealth"

All of this lives in `overlay_macos.rs` and is applied by `apply_overlay_policy`.

| Property | Value | Why |
|---|---|---|
| `NSWindow.sharingType` | `NSWindowSharingNone` (0) | the window is excluded from screen capture and screen sharing — measured 1 → 0 |
| `collectionBehavior` | `0x111` = `CanJoinAllSpaces` + `FullScreenAuxiliary` + `Stationary` | present across desktops and fullscreen Spaces |
| `level` | 25 (`NSStatusWindowLevel`) | above ordinary app windows; `alwaysOnTop` alone only reaches 5 |
| `hidesOnDeactivate` | `false` | otherwise the window disappears the moment another app takes focus |
| `acceptFirstMouse` | `true` | a floating HUD is never the key window, so the first click must be accepted |
| click-through | **off at launch** | a HUD that starts transparent to input looks broken |

**A watchdog re-applies the level, `hidesOnDeactivate` and collection behaviour every 1.5 s.** The
level is reverted to floating 5 within 500 ms of startup, so setting it once is useless. Every
correction is dispatched to the main thread, because mutating `NSWindow` off the main thread
deadlocks.

### Locking and Unlocking

The HUD starts **unlocked**: clicking and dragging work immediately, and it shows a cyan outline
with a grab bar.

| Action | How |
|---|---|
| Lock (click-through, for a real call) | `⌘⇧⌥L` or the menu bar item |
| Show / hide | `⌘⇧⌥H` or the menu bar item |
| Previous / next line | `⌘⇧⌥↑` / `⌘⇧⌥↓`, arrow keys, or the menu bar |
| Drag | anywhere on the card while unlocked |

Three modifiers deliberately. These shortcuts are registered **globally**, so a two-modifier
combination is stolen from every other app. `Cmd+Shift+E` was the original lock key and it is VS
Code's file-explorer shortcut, which meant using VS Code silently locked the HUD into click-through.
When locked, the HUD shows a **🔒 LOCKED** badge so the state is visible even though clicks pass through.

The menu bar title shows the current state — `🔒` locked, `🔓` unlocked, with `⚠️` appended when the
HUD is not on the active Space. Lock state is deliberately **not** persisted: relaunching while locked
would recreate the "I can't click anything" bug.

### Dragging

Dragging is implemented in JavaScript via pointer events:

```
pointerdown → read window position → pointermove → setPosition(start + delta × devicePixelRatio)
            → throttled to animation frames → pointerup ends the drag
```

This needs only `core:window:allow-set-position` in `capabilities/default.json`.

---

## Controls and UI

- **Playbook Selector** — Choose between Call 1 (Audit) and Call 2 (Prototype & Close) on launch or in-session via the `call-pill`.
- **STT Engine Switcher** — Segmented control on startup and header pill badge in HUD (`🍏 Apple` vs `☁️ Deepgram`).
- **Stage badge** — `STAGE 1 (LOCAL)` when the local matcher decided, `STAGE 2 (JEV)` when the model did, `MANUAL` after a manual nudge.
- **Hormozi Cue badges** — `[⬇️ DOWN]`, `[🐢 SLOW & LOW]`, `[⏸️ PAUSE 1s]`, `[⚡ PACE UP]`, `[🤔 CURIOUS]` rendered per line.
- **Level meter** — live RMS from the capture callback.
- **Microphone picker** — defaults to `CrystalSound Microphone` if detected, persisted in `localStorage`.
- **Centring scroll** — active line stays centered with past lines dimmed at 50% and upcoming lines at 35%.

---

## Log Lines Worth Knowing

| Line | Meaning |
|---|---|
| `MIC_PERMISSION status=…` | `authorized` / `denied` / `not-determined` |
| `APPLE_SPEECH_PERMISSION status=…` | Apple Speech framework authorization state |
| `AUDIO_STARTED device=…` | capture opened, with real rate/channels/format |
| `AUDIO_STATS rms=…` | periodic signal check while listening |
| `AUDIO_SILENT` | 3 s of pure zeros — no signal, wrong device, or no permission |
| `STT_READY` / `STT_FINAL` | STT engine connected / finalized an utterance |
| `APPLE_SPEECH_ENDPOINTING finalize=…` | on-device speech finalized on pause and rotated |
| `ACTIVE_LINE id=… reason=…` | the highlight moved (`line-completed`, `stage1-local`, `stage2-jev`) |
| `TRACKER_ROUTE …` | which stage decided the candidate line |
| `OVERLAY_PRESENCE_ENFORCED …` | a window property had to be corrected |
| `WINDOW_REPORT …` | full overlay state readback |

| Line | Meaning |
|---|---|
| `MIC_PERMISSION status=…` | `authorized` / `denied` / `not-determined` |
| `AUDIO_STARTED device=…` | capture opened, with real rate/channels/format |
| `AUDIO_STATS rms=…` | once a second while listening |
| `AUDIO_SILENT` | 3 s of pure zeros — no signal, wrong device, or no permission |
| `AUDIO_SOURCE_CLOSED` | producer finished, so silence afterwards is not an error |
| `STT_READY` / `STT_FINAL` | Deepgram connected / finalized a sentence |
| `ACTIVE_LINE id=… reason=…` | the highlight moved, and why |
| `TRACKER_ROUTE …` | which stage decided |
| `OVERLAY_PRESENCE_ENFORCED …` | a window property had to be corrected |
| `WINDOW_REPORT …` | full overlay state readback |

## Known limitations

### Native fullscreen cannot be overlaid — macOS restriction

A native fullscreen app owns an exclusive Space, and macOS will not put another app's window on it.
This was measured, not assumed, by driving Chrome into real fullscreen (confirmed via the View menu
offering "Exit Full Screen") and reading `NSWindow.isOnActiveSpace` for the HUD.

| Configuration tried | HUD on the fullscreen Space |
|---|---|
| level 5 / 25 / 101 / 1000 | no |
| `FullScreenAuxiliary` alone | no |
| `+ CanJoinAllSpaces` | no |
| `+ Stationary` (all three) | no |
| accessory (agent) activation policy | no |

`FullScreenAuxiliary` is documented for auxiliary windows of the *same* application, and it does not
cross application boundaries.

**What works:** a **maximised** Chrome window (Option-click the green button, or double-click the
title bar) rather than native fullscreen. There the HUD is on the active Space and reports itself
visible. Because the fullscreen case cannot be fixed, the menu bar shows `⚠️` and explains it in the
tooltip whenever the HUD is not on the active Space, so it is never silently missing.

### Stage 2 is Jev, and needs an OpenRouter key

Stage 2 asks **Jev** (TypeSafe's "System One" decision model, via OpenRouter) a single typed
`choice` question — "which script line is being delivered?" — and only acts when Jev's
`confidence` clears the floor. Without a `JEV_API_KEY` (or `OPENROUTER_API_KEY`) the semantic
tie-break stays dark, the log shows `TRACKER_ROUTE stage1-local (no jev key)`, and only stage 1
decides.

### Microphone permission is granted outside the app

The app can request it and report it, but only a human can approve it. A denial is permanent until
changed in System Settings, and macOS never prompts again on its own.

### Global hotkeys are unproven

The shortcuts register successfully, but this machine has never granted the Accessibility permission
needed to synthesize input, so no shortcut has been observed actually firing. The menu bar item
exists precisely because a click-through HUD must not depend on one.

### Not yet built

- Per-app audio filtering (the prospect tap currently hears the whole Mac)
- A model tier on ambiguous objections (keywords only today)
- A confirm chip instead of showing the card immediately
- Cue coaching from audio features (pace, inflection) — cues are authored metadata today
- Voice-activity gating to cut Deepgram cost on silence
- Rate limiting / backpressure when the model is slow but up

---

## Lessons from building this

Recorded because each cost real time and most of them produced a **green result for the wrong
reason**:

1. **Two gates, not one, decide whether a click lands.** `ignoresMouseEvents` says the window accepts
   clicks; `acceptsFirstMouse` decides whether the webview receives one while the window is not key.
   Measuring the first and declaring the second proven was wrong.
2. **A denied microphone looks like a broken app.** CoreAudio opens the device and returns zeros.
   There is no error to catch; only an explicit permission check finds it.
3. **`data-tauri-drag-region` needs `core:window:allow-start-dragging`**, which is not in
   `core:default`. The injected script does not await the invoke, so the denial was completely
   silent — the attribute matched, the mousedown fired, nothing moved, no error anywhere.
4. **Window level does not survive startup.** It reverts to 5 within 500 ms; enforcement has to be
   continuous.
5. **`NSWindow` must be mutated on the main thread.** Doing it from a worker thread deadlocks;
   verified by hanging one.
6. **A test that cannot go red proves nothing.** Three separate checks here passed for the wrong
   reason first: a presence assertion that ignored timing, a fixture that finished naturally instead
   of testing the stop flag, and a fullscreen test that was actually measuring a maximised window.
   Every guard in this repo is now paired with a failing case.
7. **A silence watchdog must distinguish "no data" from "silent data".** The replay path published no
   level at all, so the watchdog false-alarmed in Simulated Call Mode until the replay path also
   reported RMS, and until end-of-source was signalled explicitly.

---

## Repository layout

```
.
├── src/                      React HUD
│   ├── App.tsx
│   ├── lib/{script,cues}.ts
│   └── fixtures/remodeler_script.json   26-line script
├── src-tauri/
│   ├── src/                  Rust core (see Architecture)
│   ├── capabilities/default.json
│   ├── Info.plist            NSMicrophoneUsageDescription
│   └── tauri.conf.json       window config, signing identity
├── .github/workflows/ci.yml  frontend (ubuntu) + rust (macos)
└── .zenith/missions/stealthprompter-v1/   original contract, tasks, evidence trail
```

---

## Security and privacy

- Audio is streamed to Deepgram in real time and **never written to disk**.
- Keys come from the environment or the Keychain and are masked in logs (`KEYS deepgram=32a8…5425`).
- CI fails if a credential-shaped string appears in source, after a live key once reached the public
  repository through a unit test.
- The window is excluded from screen capture, so the script is not visible in a screen share.



