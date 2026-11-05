# StealthPrompter

A click-through HUD for macOS that follows a sales script while you talk. It listens **only to
your microphone**, transcribes your speech in real time, and moves the highlight to the line you
are actually delivering — through paraphrase, tangents and interruptions — with no clicking.

The window is excluded from screen capture, so the script never appears in a screen share.

Built with **Tauri v2.12** (Rust) + **React 19** / **Vite 8** / **TypeScript**.

---

## Status

| Area | State |
|---|---|
| Overlay shell (transparent, always-on-top, capture-excluded) | Working |
| Microphone capture → 16 kHz mono | Working |
| Deepgram Nova-3 live transcription | Working |
| Line tracking (local lexical + LLM tie-break) | Working, **stage 1 only** |
| Stage 2 (DeepSeek) | **Never received a real verdict** — key returns 401 |
| Native fullscreen overlay | **Not possible** (macOS restriction, measured — see below) |
| Global hotkeys | Registered, never actually pressed (no Accessibility grant to synthesize input) |

Roughly 3,120 lines of Rust and 620 lines of TypeScript.

---

## How it works

```
 microphone
     │  cpal, device rate/channels (e.g. 48 kHz stereo f32)
     ▼
 Resampler ──► 16 kHz mono i16
     │
     ├──► RMS level ──► HUD level meter + silence watchdog
     │
     ▼
 Deepgram Nova-3 (streaming WebSocket, KeepAlive ~4 s so long silences don't drop it)
     │  interim + final transcripts
     ▼
 Tracker ──► stage 0: interims update a replaceable partial tail
            stage 1: local lexical match over an 18-word window
            stage 2: DeepSeek tie-break, only when stage 1 is not confident
     │
     ▼
 HUD ──► highlight, cue badges, smooth centring scroll
```

The design goal is **latency under a sentence**: a cycle runs every 2 words, and interim
transcripts are used, so the highlight moves while you are still speaking the line rather than
after Deepgram finalizes it.

---

## Quick start

### Requirements

- macOS 11+ (Apple Silicon or Intel)
- Node 22+ and npm
- Rust stable (`rustup`) — <https://rustup.rs>

### 1. Install and run

```bash
npm install
npm run tauri dev
```

### 2. Provide the API keys

Keys are read from the **environment first**, then from the **macOS Keychain**. They are never
written to disk by the app.

```bash
security add-generic-password -a stealthprompter -s DEEPGRAM_API_KEY -w "<key>" -U
security add-generic-password -a stealthprompter -s DEEPSEEK_API_KEY -w "<key>" -U
```

- `DEEPGRAM_API_KEY` — required. Without it, starting a session fails.
- `DEEPSEEK_API_KEY` — optional. Without a *working* key, stage 2 stays off and every advance is
  `reason=stage1-local`.

### 3. Grant microphone access — required

macOS does not fail a capture when permission is missing. It opens the device and hands the app
**digital silence**: the stream starts, the callback fires, every sample is zero, and nothing
errors. There is no way for the app to tell that apart from a broken microphone, so it must ask.

The app now requests access on first Start and reports the result. If it was already denied, open
**System Settings → Privacy & Security → Microphone**, enable **StealthPrompter**, then restart
the app. The HUD shows an **Open Settings** button when this is the problem.

You can check the current state at any time:

```bash
grep MIC_PERMISSION ~/Library/Logs/StealthPrompter.log
```

### 4. Build a signed app

```bash
security unlock-keychain -p tpsign ~/Library/Keychains/stealthprompter-signing.keychain-db
npx tauri build --bundles app
cp -R src-tauri/target/release/bundle/macos/StealthPrompter.app /Applications/
```

The bundle is signed with a local self-signed identity, `StealthPrompter Local Signing`, kept in a
dedicated keychain with a certificate-anchored designated requirement.

Replacing the `.app` (a rebuild and reinstall) resets the microphone decision back to
`not-determined`, so macOS asks again — measured, not assumed. If you reinstall, expect to re-approve
the microphone, and check `MIC_PERMISSION` in the log if audio is silent afterwards.

## The script and how matching works

The script fixture (`src/fixtures/remodeler_script.json`) is 26 sentence-level lines across 4
phases:

| Phase | Purpose |
|---|---|
| `PHASE 1: SET THE FRAME` | open, get permission to run discovery |
| `PHASE 2: THE 4 DISCOVERY BUCKETS` | past projects, lead flow, personal time, budget |
| `PHASE 3: CLINICAL DIAGNOSIS` | name the cost, test the pain |
| `PHASE 4: LOCK CALL 2` | close on the next step |

Each line carries a `label`, the `raw` authored text (with `[Name]` placeholders), a derived
`body`, and per-line cue metadata.

### Two-stage tracking

**Stage 0 — interims.** Non-final transcripts are kept in a replaceable *partial tail* that is
included in the match window. A finalized segment clears the tail so text is superseded, never
duplicated. This is the single biggest latency win in the system: waiting for Deepgram to finalize
a sentence means the highlight arrives when the line is almost over.

**Stage 1 — local lexical.** An 18-word window is scored against every line. Each shared word is
weighted by inverse document frequency, so a word shared by one line counts far more than a word
every line uses. Four gates must all pass before stage 1 trusts itself:

| Constant | Value | Guards against |
|---|---|---|
| `MIN_HITS` | 2 | a single incidental word counting as evidence |
| `MIN_DOMINANT_SCORE` | 1.6 | a vacuous margin pass when every other line scores 0 |
| `MIN_COVERAGE` | 0.25 | two chance words clearing the first two gates while covering a fifth of a line |
| `LOCAL_MARGIN` | 1.25× | a near-tie with the runner-up |

`MIN_COVERAGE` exists because of a real regression: off-script speech ("my cousin is driving down
from Ohio and back") advanced to the tire-kickers line on the words *driving* and *back* alone.
Fast thresholds make that class of coincidence easier to hit, not harder.

**Stage 2 — DeepSeek tie-break.** Only runs when stage 1 is not confident. Throttled to one call
per 700 ms with a 900 ms timeout and a circuit breaker, because frequent cycles would otherwise
flood the model. **It has never returned a real verdict**: every supplied key has been a gateway
credential rather than a platform key, so every `ACTIVE_LINE` is `reason=stage1-local`.

### Timing constants

| Constant | Value | Reason |
|---|---|---|
| `WORDS_PER_CYCLE` | 2 | 6 was too slow; a 10-word line was 60% spoken before classifying |
| `PARTIAL_WORDS_TRIGGER` | 2 | lets an interim trigger a cycle |
| `BUFFER_WORDS` | 18 | enough context for a paraphrase, short enough to stay current |
| LLM throttle | 700 ms | one tie-break call per cycle burst, at most |
| LLM timeout | 900 ms | a slow model must never stall the highlight |

---

## Architecture

```
src-tauri/src/
  lib.rs            1240  app wiring, commands, session lifecycle, overlay policy, tray, hotkeys
  tracker.rs         700  two-stage line tracker, script loading, cue metadata
  audio.rs           394  cpal capture, resampler, WAV replay for Simulated Call Mode
  overlay_macos.rs   335  NSWindow policy via objc2: capture exclusion, Spaces, level, probes
  stt.rs             238  Deepgram Nova-3 streaming client
  mic_permission.rs  108  TCC microphone permission preflight and prompt
  keys.rs            100  environment → Keychain key lookup
  main.rs              5  entry point

src/
  App.tsx            302  HUD: highlight, cues, meter, drag, device picker, controls
  lib/script.ts      124  script loading and body derivation
  lib/cues.ts         40  cue metadata → badge chips
  lib/*.test.ts      147  vitest suites
```

### Tauri commands

`start_session`, `stop_session`, `list_devices`, `key_status`, `script_lines`, `tracker_snapshot`,
`nudge`, `audio_level`, `hud_state`, `hud_report`, `set_click_through`, `open_mic_settings`,
`input_probe`, `drag_probe`.

### Files on disk

| Path | Contents |
|---|---|
| `~/Library/Application Support/com.gaganrajput11.stealthprompter/window.json` | window position/size, written 700 ms after a move settles |
| `~/Library/Logs/StealthPrompter.log` | mirror of stdout/stderr when launched from Finder |
| macOS Keychain | `DEEPGRAM_API_KEY`, `DEEPSEEK_API_KEY` |

Log mirroring exists because a Finder-launched `.app` has no terminal, so every diagnostic went
nowhere — a large part of why a silent microphone took so long to diagnose.

## The overlay: what makes it "stealth"

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
deadlocks — verified by hanging a thread doing exactly that.

### Locking and unlocking

The HUD starts **unlocked**: clicking and dragging work immediately, and it shows a cyan outline
with a grab bar.

| Action | How |
|---|---|
| Lock (click-through, for a real call) | `⌘⇧⌥L` or the menu bar item |
| Show / hide | `⌘⇧⌥H` or the menu bar item |
| Previous / next line | `⌘⇧⌥↑` / `⌘⇧⌥↓`, arrow keys, or the menu bar |

Three modifiers deliberately. These shortcuts are registered **globally**, so a two-modifier
combination is stolen from every other app. `Cmd+Shift+E` was the original lock key and it is VS
Code's file-explorer shortcut, which meant using VS Code silently locked the HUD into click-through
— indistinguishable from a broken app. When locked, the HUD shows a **🔒 LOCKED** badge so the state
is visible even though clicks pass through.
| Drag | anywhere on the card while unlocked |

The menu bar title shows the current state — `🔒` locked, `🔓` unlocked, with `⚠️` appended when the
HUD is not on the active Space (see fullscreen below). Lock state is deliberately **not** persisted:
relaunching while locked would recreate the "I can't click anything" bug.

### Dragging

Dragging is implemented in JavaScript, not with `data-tauri-drag-region`:

```
pointerdown → read window position → pointermove → setPosition(start + delta × devicePixelRatio)
            → throttled to animation frames → pointerup ends the drag
```

This needs only `core:window:allow-set-position` in `capabilities/default.json` (the default
capability set does **not** include it, nor `allow-start-dragging`).

---

## Controls and UI

- **Stage badge** — `STAGE 1 (LOCAL)` when the local matcher decided, `STAGE 2 (DEEPSEEK)` when the
  model did, `MANUAL` after a manual nudge.
- **Cue micro-badges** — ⬇️ DOWN, 🤔 CURIOUS, 🐢 SLOW & LOW, ⏸️ 1.5s, rendered per line.
- **Level meter** — live RMS from the capture callback.
- **Microphone picker** — shown while stopped; the system default is not always the microphone you
  are speaking into (a USB speakerphone or a virtual loopback can win the default). Persisted in
  `localStorage`.
- **Centring scroll** — the active line stays centred with neighbours visible above and below.

---

## Testing

### Gates

```bash
npm run typecheck                                  # tsc --noEmit
npm run test                                       # vitest, 24 tests
npm run build                                      # tsc + vite build
cd src-tauri
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test                                         # 31 tests
```

CI runs the frontend job on Ubuntu (typecheck, vitest, build, plus a secret scan that fails the
build if a credential-shaped string appears in source) and the Rust job on macOS (fmt, clippy).

### Simulated Call Mode

The real Deepgram socket and the real tracker can be exercised without a microphone by replaying a
16 kHz mono WAV. This is how every latency and accuracy claim in this README was verified.

```bash
STEALTH_AUTOSTART=1 STEALTH_SIMULATE_WAV=/path/to/clip.wav npm run tauri dev
```

Fixtures used during development:

| Clip | Expected |
|---|---|
| on-script walk (paraphrased delivery) | 6 advances, 7→8→9→10→11→12 |
| off-script tangent | 0 advances |

### Test hooks

All are environment variables, read once at startup. They exist so behaviour can be measured
instead of assumed.

| Variable | Purpose |
|---|---|
| `STEALTH_AUTOSTART` | begin a session 1.2 s after launch |
| `STEALTH_AUTOSTOP_MS` | stop the session after N ms |
| `STEALTH_SIMULATE_WAV` | replay a WAV instead of the microphone |
| `STEALTH_EDIT_TOGGLE_MS` | toggle lock at N ms and log the OS-level flag |
| `STEALTH_TEST_MOVE` | move the window once, to prove persistence |
| `STEALTH_CLICK_TEST` / `_MS` | post a synthetic `mouseDown` and check it reaches the webview |
| `STEALTH_DRAG_TEST` | post a full drag gesture and assert the window actually moved |
| `STEALTH_FOCUS_TEST` | activate Chrome, then report whether the HUD is still on screen |
| `STEALTH_FULLSCREEN_TEST` | put Chrome in fullscreen and measure again |

### Log lines worth knowing

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

### Stage 2 has never run

`DEEPSEEK_API_KEY` has to be a real platform key from `platform.deepseek.com`. The keys supplied so
far were gateway credentials and returned 401 against the chat, models and balance endpoints. Until
that is fixed, the semantic tie-break is dark and only stage 1 decides.

### Microphone permission is granted outside the app

The app can request it and report it, but only a human can approve it. A denial is permanent until
changed in System Settings, and macOS never prompts again on its own.

### Global hotkeys are unproven

The shortcuts register successfully, but this machine has never granted the Accessibility permission
needed to synthesize input, so no shortcut has been observed actually firing. The menu bar item
exists precisely because a click-through HUD must not depend on one.

### Not yet built

- Stage 2 tuning against real model output
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



