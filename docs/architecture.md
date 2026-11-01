# Architecture

Durable engineering decisions. The full mission trail (contract, evidence, decisions) lives in
`.zenith/`, which is git-ignored by default.

## Shape

```
Microphone (cpal, Rust)            ← never getUserMedia: unreliable in macOS WKWebView
    │ 16 kHz / mono / i16
    ▼
Deepgram Nova-3 WebSocket          ← KeepAlive every 3–5s or the socket dies at 10s (NET-0001)
    │ interim + final transcript
    ▼
Tracker (Rust)
    S0 gate        no new speech → do nothing
    S1 local match ~5–15 ms, offline, free; candidate window = active ± 3 blocks
    S2 deepseek-flash  only on ambiguity; thinking disabled; 800 ms timeout
    │ hysteresis + monotonic forward bias
    ▼
HUD (React)                        ← dumb renderer; receives active_id + cues only
```

## Why the LLM is a tie-breaker, not the decider

The original spec called the classifier on every speech buffer. That is slow (~500 ms),
costs money on every cycle, and a single bad `active_id` yanks the highlight mid-sentence.

Most blocks are unambiguous, so a cheap local match settles them in ~10 ms. The LLM is reserved
for genuine ambiguity. The consequences are: faster switches, ~80% fewer API calls, and the HUD
keeps working when the network or the model fails.

Prompt ordering is deliberate: static script JSON first, volatile `active_id` + buffer last, so
DeepSeek's prefix cache hits (50× cheaper and lower latency).

## Honest latency budget

Detection is **not** sub-250 ms. Realistic total from words spoken to highlight moved:

| Stage | Typical |
|---|---|
| Deepgram interim | 300–500 ms |
| Debounce (6 words or 800 ms pause) | 1000–2400 ms |
| DeepSeek round-trip (non-thinking) | 300–700 ms |

Asserted budget: **p50 < 1.5 s, p95 < 2.5 s**. Perceptual speed comes from always rendering the
next block at low opacity plus a forward bias, not from pretending the pipeline is instant.

## Overlay constraints (macOS)

- **Capture exclusion is mandatory.** `set_content_protected(true)` maps to `NSWindowSharingNone`.
  Without it the prospect can read the script when the rep shares their screen.
- **`backdrop-filter` cannot blur another app's window.** It only blurs sibling DOM inside the
  same webview. Real glass comes from native vibrancy (`windowEffects`), shipped by the
  `window-vibrancy` crate that Tauri already depends on.
- **A click-through window receives no keystrokes.** Manual overrides must be global shortcuts,
  and bare arrow keys are off-limits because they would hijack the whole desktop.
- **Fullscreen apps live in their own Space.** Always-on-top alone will not float over a native
  fullscreen Meet window; that needs `set_visible_on_all_workspaces` plus possibly raw `objc2`.

## Delivery cues are data, not decoration

Blocks carry `tone`, `pace` and `pauseAfterMs`. The coaching surface (downward inflection,
slowed pace, deliberate silence after a question or the price) is impossible to render if the
model does not carry it, and tonality matters more to close rate than verbatim script recall.

## Audio realism limits

Raw `cpal` capture has no echo cancellation, so laptop speakers leak the prospect's voice into
the transcript. v1 requires headphones. Real recordings are the only Tier-A evidence for
tracking accuracy; TTS fixtures are an optimistic ceiling. See
`.zenith/missions/stealthprompter-v1/decisions/002-evidence-floor.md`.

## Distribution

Locally signed with a **self-signed certificate** (`StealthPrompter Local Signing`), no Apple
Developer ID, no notarization (`decisions/003-distribution.md`).

Why not ad-hoc: macOS keys the microphone grant to the bundle ID plus a *designated
requirement*. Under ad-hoc signing that requirement is cdhash-derived, and the cdhash changes on
every build — so macOS treated each rebuild as a new app and re-prompted. Anchoring it to a
certificate leaf makes it content-independent:

```
designated => identifier "com.gaganrajput11.stealthprompter"
              and certificate leaf = H"bba3b7faf676c681c31853ab985ab3dc6a15c998"
```

The signing identity lives in its own keychain (`stealthprompter-signing.keychain-db`) rather
than the login keychain, purely so signing is non-interactive: the login keychain would demand a
GUI password prompt on every `codesign` call. Unlock it before building.
