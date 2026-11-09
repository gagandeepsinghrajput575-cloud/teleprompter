# HANDOFF — StealthPrompter + Pulse Dialer integration

> Read this first after a context reset. It records the current state, the integration contract,
> the failure points, and the next steps. Last updated by the build that added the loopback
> context webhook hardening and the Scalix cold-call script.

## What it is
A macOS Tauri HUD teleprompter for live sales calls: transparent, click-through, excluded from
screen capture, follows the script as you speak, shows prospect context pushed by the Pulse
Dialer, and surfaces pre-written objection battlecards on Control+number keys.

## Repos
- **Teleprompter:** `/Users/gaganrajput11/teleprompter` (public GitHub `gagandeepsinghrajput575-cloud/teleprompter`).
  Committed **locally** on `main`; **NOT pushed** (it contains proprietary sales scripts).
- **Pulse Dialer:** `/Users/gaganrajput11/Downloads/workspace-01a0f89f-f130-7eed-997f-341234f82d1e/dialer`
  (public). Integration commit `0912e1a` is pushed; later commits are local only.

## Machine
MacBook Neo (Mac17,5) · Apple A18 Pro · 6 cores (2P+4E) · **8 GB RAM**. Keep VS Code closed
during calls; run few other apps. WebKit GPU can sit near 100% CPU (the HUD webview).

## Build / run / test
```bash
cd /Users/gaganrajput11/teleprompter
npm run tauri dev            # dev
npm run tauri build          # release .app -> src-tauri/target/release/bundle/macos/
cp -R src-tauri/target/release/bundle/macos/StealthPrompter.app /Applications/
open -n /Applications/StealthPrompter.app
# gates
cd src-tauri && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all --check
cd .. && npm run typecheck && npm run test && npm run build
```
**Logs:** `~/Library/Logs/StealthPrompter.log` (append). Useful tags: `SCRIPT_LOADED`,
`SCRIPT_SWITCHED`, `ACTIVE_LINE`, `CALL_CONTEXT`, `CALL_CONTEXT_LISTENING`, `CALL_CONTEXT_REJECTED`,
`HOTKEYS registered`.

## Architecture map
- `src-tauri/src/lib.rs` — Tauri setup, commands, global hotkeys, STT event loop, tray.
- `src-tauri/src/tracker.rs` — two-stage tracker + all matching/completion logic (heavily tested).
- `src-tauri/src/apple_stt.rs` — on-device SFSpeechRecognizer loop + time-based endpointing.
- `src-tauri/src/apple_speech.m/.h` — Objective-C Speech.framework bridge.
- `src-tauri/src/stt.rs` — Deepgram Nova-3 streaming fallback.
- `src-tauri/src/audio.rs` — cpal capture, 16 kHz mono resample, WAV replay.
- `src-tauri/src/context_server.rs` — loopback HTTP webhook for the dialer.
- `src-tauri/src/overlay_macos.rs` — NSWindow stealth/capture-exclusion/space behaviour.
- `src/App.tsx` — HUD, call selector, prospect context, objection overlay, scroll.
- `src/lib/script.ts` — annotation parsing; `src/lib/cues.ts` — cue chips;
  `src/lib/objections.ts` — the 10 battlecards; `src/fixtures/*.json` — scripts.
- STT engine today is **legacy Apple SFSpeechRecognizer**. SpeechAnalyzer (macOS 26) is NOT
  integrated (proven available; `en_US` model installed) — planned upgrade.

## Tracking behaviour (important)
- Matching vocabulary is the spoken **body only**; labels/phases are excluded.
- Placeholders (`[Name]`, `[Niche]`, `[Company]`, `[firstname]`, `[company domain]`) are stripped
  from the matching vocabulary (`spoken_content`) so they never count toward completion.
- **Instant advance:** an interim that covers ~70% of the active line *and* contains its closing
  word completes it immediately (`LIVE_COMPLETE_COVERAGE = 0.70`).
- **Fallback:** a finalized utterance completes on `coverage >= 0.6`, or closing words with
  `coverage >= 0.4`, or next-line start with `coverage >= 0.3`.
- **Endpointing:** `SILENCE_MS_TO_FINALIZE = 400` measured in real audio samples (NOT chunk count —
  that bug shredded speech into single-word finals).
- **Forward-only auto.** Backward needs 3 confirmations. A manual jump re-anchors and flushes the
  stale audio buffer, then auto continues forward from that line (no pause).
- Per-line token sets are precomputed; transcript UI updates are throttled; centring uses a direct
  `scrollTop` assignment.

## Controls
| Key | Action |
|---|---|
| ⌘⇧⌥← / ⌘⇧⌥→ | line back / forward (re-anchor) |
| ⌘⇧⌥↑ / ⌘⇧⌥↓ | previous / next section (phase) |
| ⌘⇧⌥R | re-sync at current line |
| ⌘⇧⌥D | dismiss objection card |
| ⌃1–⌃9, ⌃0 | objection battlecards |
| ⌘⇧⌥L | lock/unlock (click-through) |
| ⌘⇧⌥H | hide/show |

Window is **resizable (min 260×220)**. A click-through window cannot be resized — unlock with
⌘⇧⌥L first. Size persists across launches.

## Scripts & objections
- Fixtures: `call1_audit_script.json` (31 lines), `call2_sales_script.json` (24),
  `coldcall_scalix.json` (**12-line centre flow only**).
- The Scalix flowchart is modelled as: **centre gold line = the script** (Opening → If
  "what is this regarding" → If "is he expecting" → Introduce → Hook → Leak → Offer → Lock time →
  Narrow → Confirm email → Spell back → Sign off); **side branches = the objection cards**.
- Objections (`src/lib/objections.ts`), exact authored wording:
  ⌃1 Price/why free · ⌃2 Not interested · ⌃3 Don't take cold calls · ⌃4 Where number ·
  ⌃5 Send email · ⌃6 Web/tech guy · ⌃7 No AI/old-school · ⌃8 Booked out · ⌃9 Busy/on-site ·
  ⌃0 Employee on-site.
- Objection cards also substitute `[Name]`/`[Niche]`/etc. for display.

## Pulse Dialer integration (contract)
Teleprompter runs a **loopback** HTTP server on `127.0.0.1:8765` (override `TELEPROMPTER_PORT`):
- `POST /context` (alias `/call/start`) body `{ "name", "company", "niche", "script"? }` →
  stores + emits `call_context`; `script` ∈ {`call1`,`call2`,`coldcall`} auto-selects that script.
- `GET /health` → `{"ok":true}`.
- Rejects a request carrying a browser `Origin` header with **403**; caps body at 1 MB; one thread
  per connection so a slow client cannot block a push.

Dialer side (`server/services/teleprompterNotify.js`), called from `callService.start()` **after the
call is committed**:
- Fire-and-forget, 400 ms timeout, errors swallowed, disabled when `PULSE_TEST=1`.
- If the HUD is not running it launches it (`open -g -a StealthPrompter`) and retries up to 3×.
- **Default `script` is `coldcall`**; `TELEPROMPTER_SCRIPT` overrides (`none` omits).
- Env: `TELEPROMPTER_URL`, `TELEPROMPTER_APP`, `TELEPROMPTER_DISABLED`.

Verified: a Node `fetch` (the dialer's mechanism) produces `{"ok":true}` and the log shows
`CALL_CONTEXT … script=Some("coldcall")` then `SCRIPT_SWITCHED to 'Cold Call: Scalix Elite'`.

## Prospect (system-audio) tap — how the AI suggests the reply
Goal: hear the **prospect**, not just you, without touching Pulse Dialer.

- `src-tauri/src/system_audio.m` — ScreenCaptureKit stream, audio only (2×2 surface, no video
  output), 48 kHz stereo → mono `int16` in C. Reads what macOS is already playing; never touches
  the microphone or the dialer's audio routing.
- `src-tauri/src/system_audio.rs` — FFI + channel, plus `prospect_status` events.
- A **second** `apple_stt::run` consumes that channel, so prospect speech can never move your line.
- `src-tauri/src/objection_detect.rs` — keyword tier-0 classifier: normalisation, word-boundary
  phrases, weights, ties to the more specific rule, 12 s per-index cooldown. Returns the ⌃N index.
- `lib.rs` emits the existing `objection` event → the battlecard appears by itself. ⌘⇧⌥D dismisses.
- HUD: `👂 Prospect audio` toggle plus a dim `👂 …` echo of what it is hearing (own component, so
  it never re-renders the script).

Permissions (one-time per signature):
1. Click the toggle → macOS asks for **Speech** (ad-hoc re-signing re-asks after each rebuild).
2. Then **Screen & System Audio Recording** (`kTCCServiceAudioCapture`) → allow; relaunch if asked.
3. Toggle again. The `settings` link opens that exact pane.

Diagnostics: `STEALTH_PROSPECT_AUDIO=1` starts the tap at launch; log lines are
`PROSPECT_AUDIO_STATUS`, `PROSPECT_STT_READY`, `PROSPECT_AUDIO_FIRST`, `PROSPECT_OBJECTION`.

Caveats: it taps the **whole** system output (music would be transcribed too) and two concurrent
Speech tasks are unproven across long calls. Per-app Core Audio process taps are the follow-up.

## Known limitations / places it can fail
1. **Ad-hoc signing** (`signingIdentity "-"`): macOS **re-prompts Speech Recognition after every
   rebuild**. Click Allow. A self-signed certificate (see `docs/architecture.md`) would persist.
2. **Native fullscreen apps:** the HUD cannot join another app's exclusive fullscreen Space; use a
   maximised window. Tray shows ⚠️ when it thinks it is off-Space.
3. **SpeechAnalyzer not integrated** (legacy recogniser): latency is limited by its warm-up and
   interim cadence. ~0.2 s end-of-speech is the target once migrated.
4. **Prospect audio tap is built but needs a one-time grant.** ScreenCaptureKit + a second STT
   stream + keyword classifier are wired (`src-tauri/src/system_audio.m`, `system_audio.rs`,
   `objection_detect.rs`), but macOS requires **Screen & System Audio Recording**
   (`kTCCServiceAudioCapture`) for THIS app. Until granted, the stream starts but delivers no audio.
   Ad-hoc signing also re-prompts **Speech** after every rebuild.
5. **Jev stage-2 is unconfigured** (no `JEV_API_KEY`; Deepgram key is present). Stage 2 stays off.
6. **Mutex `.lock().unwrap()` throughout**: a panic while holding a lock would poison it and later
   commands would panic. Low risk (tracker is panic-tested) but a known sharp edge.
7. **Port 8765 in use** → `CALL_CONTEXT_LISTEN_FAILED` is logged and the app still runs, but the
   dialer integration is silently unavailable.
8. **Dialer default `coldcall`** may be wrong if you dial Call1/Call2 from the dialer; set
   `TELEPROMPTER_SCRIPT` accordingly.
9. **Teleprompter repo not pushed** (public repo + proprietary scripts). Dialer integration pushed;
   later notify commit is local.

## Next steps (priority order)
1. Migrate the Apple engine to **SpeechAnalyzer/SpeechTranscriber** (Swift bridge compiled via
   `build.rs`, legacy fallback) → ~0.2 s and word timings.
2. **Prospect tap follow-ups:** per-app Core Audio process tap (so only the dialer is heard), an
   LLM tier on ambiguous objections, and a confirm chip instead of showing the card immediately.
3. HUD GPU trim — done: high-frequency updates isolated, `font-size` animation removed; idle GPU 0%.
4. Push policy: move scripts out of the public repo or use a private remote before pushing.
5. Optional: self-signed certificate to stop the Speech TCC re-prompts.

## Performance profile (measured)
- Idle HUD after optimisation: Rust shell 0.3% CPU / 28 MB RSS; WebKit **GPU 0.0%**,
  WebContent 0.6% (~14 MB). Previously the GPU process sat near 100%.
- High-frequency updates are isolated: `SpokenText` owns the transcript listener (150 ms) and
  `LevelMeter` owns `audio_level` polling (200 ms), so neither re-renders the 31-line list.
- `font-size` is **not** transitioned (it forced layout every animation frame); only opacity animates.
- Per-cycle routing logs are off by default (they wrote a line per interim word). Set
  `STEALTH_VERBOSE=1` to restore `TRACKER_ROUTE` / `TRACKER_RETAIN` lines when diagnosing.

## Test counts / current health
75 Rust tests · 24 Vitest · clippy + fmt + typecheck clean · `tauri build EXIT=0`.
Last live check: `/health` 200, valid context 200, bad JSON 400, browser Origin 403, app running.

