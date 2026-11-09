# StealthPrompter — Read This First

A plain-English summary of the latest changes: what is new, what you must click once, and how to
check it works. No jargon.

---

## The short version

Four updates:

| # | Update | What you get |
|---|---|---|
| 1 | **Bare number keys** | Press `1`–`9`/`0` with no Control key to show an objection card |
| 2 | **It hears the prospect** | The call audio is transcribed and the right card pops up by itself |
| 3 | **Faster HUD** | Idle GPU went from ~100% to ~0%. Same look |
| 4 | **Safer dialer link** | The local dialer webhook ignores random web pages and can't be stalled |

Pulse Dialer was **not** changed. Not one line.

---

## 1. Bare number keys (`1`–`9`, `0`)

**What it does:** pressing `3` does what `⌃3` already did — shows the third objection card.

**How to turn it on**

1. Open the HUD and press **Start listening**.
2. Click **`1–0 Bare keys OFF`** in the HUD.
3. macOS asks for **Accessibility** → allow it.
4. Click the toggle again if needed.

**How it behaves**

| You press | What happens |
|---|---|
| `3` during a call | Card appears. The `3` is **not** typed into the dialer |
| `3` when not listening | Normal `3` — the app is not touching your keyboard |
| `⌘1`, `⌃C`, `⇧4` | Normal macOS behaviour, always |
| Hold a key down | Ignored, no repeated cards |

**Why not a normal shortcut?** A global shortcut for `1` would steal that key in every app, all the
time. This one only takes it while a call is listening, which is why it needs Accessibility.

**If the toggle complains:** click the `settings` link next to it, switch **StealthPrompter** on in
the Accessibility list, then toggle again. You only do this once per install.

---

## 2. It can hear the prospect now

**What it does:** the HUD listens to what your Mac is playing (the dialer call), transcribes it,
and opens the matching objection card on its own.

**How to turn it on**

1. In the HUD, click **`👂 Prospect audio OFF`**.
2. Allow **Speech**, then **Screen & System Audio Recording**.
3. If macOS asks to relaunch, relaunch and click the toggle again.

**You will see**

- A dim line starting with 👂 — that is what it is hearing from the call.
- A card appears when the prospect says something like *"how much does it cost?"*, *"we already
  have a web guy"*, *"just send me an email"*.
- `⌘⇧⌥D` closes a card.

**Good to know**

- It reacts to the prospect's **first words** (about 1–2 seconds). It does not predict the future.
- The same card will not pop again for 12 seconds.
- It hears **everything** your Mac plays. Music or a YouTube tab would be transcribed too. Use it
  during calls, or close other audio.
- It does not move your script. Your microphone still drives the line tracking.

---

## 3. Why it feels faster

**Before:** the whole script list redrew on every mic tick and every transcript tick, and the active
line animated its font size. Animating text size forces the Mac to re-layout every frame.

**Now:** only the small pieces update. The list redraws only when the line actually changes.

Measured while idle, after the fix:

| Part | CPU |
|---|---|
| WebKit GPU | **0.0%** (was ~100%) |
| Web content | 0.6% |
| App itself | 0.3%, 28 MB RAM |

Also, the app no longer writes a log line for every word you say during a call.

---

## 4. The Pulse Dialer link (behaviour unchanged)

When a call starts, the dialer sends the lead to the HUD on `127.0.0.1:8765`. The HUD loads the
right script and the prospect's name/company.

What changed is only the safety of that link:

- A web page in your browser can no longer push a fake lead (browser requests get `403`).
- A stuck client can no longer freeze the link (each connection is handled separately).
- Broken data gets `400` instead of being half-accepted.

The dialer itself still does exactly what it did before.

---

## Permissions cheat sheet

| macOS permission | Needed for | What happens without it |
|---|---|---|
| **Microphone** | Your voice → script follows you | Nothing is heard |
| **Speech Recognition** | Transcribing (mic and prospect) | No text, no tracking |
| **Accessibility** | Bare `1`–`9`/`0` keys | The toggle reports an error; nothing breaks |
| **Screen & System Audio Recording** | Hearing the prospect | The tap starts but stays silent |

**After every rebuild** macOS may ask again — the app is signed "ad-hoc", which macOS treats as a
new app each time. Just allow it. This is a known limit, not a new bug.

---

## Try it in 60 seconds

1. Open StealthPrompter, pick **Cold Call: Scalix Elite**.
2. Press **Start listening** (allow Microphone + Speech if asked).
3. Click **`1–0 Bare keys OFF`** → allow **Accessibility** → click it again.
4. Press `1`, `2`, `3` — the cards should appear, and no numbers should be typed anywhere.
5. Press **Stop**, then type `123` in any app — normal typing again.
6. Want the automatic version too? Click **`👂 Prospect audio OFF`** and allow the two prompts,
   then play any speech out loud. The 👂 line should show words and a card should appear on an
   objection phrase.

---

## Known limits (honest list)

- **Bare keys only work while listening.** That is the safety rail; press Stop and your keyboard
  is completely normal again.
- **Prospect audio hears the whole Mac**, not just the dialer. Music gets transcribed too.
- **Two speech engines at once** (you + prospect) is new. It works in testing, but a 45-minute call
  is the real test.
- **The objection suggestion is keyword-based**, not a language model. It catches the ten Scalix
  objections in their normal wording, not clever rephrasings.
- **Permission prompts after every rebuild**, because of ad-hoc signing.
- **The code is not pushed to GitHub yet** — the repo is public and contains your scripts.

---

## What changed on disk

| File | Why |
|---|---|
| `src-tauri/src/bare_keys.m` / `bare_keys.rs` | The bare `1`–`0` key tap |
| `src-tauri/src/system_audio.m` / `system_audio.rs` | Captures the Mac's audio (the prospect) |
| `src-tauri/src/objection_detect.rs` | Turns prospect words into an objection number |
| `src-tauri/src/context_server.rs` | Dialer link, now with the safety checks |
| `src-tauri/src/lib.rs` | Wires the new pieces together |
| `src-tauri/build.rs` | Compiles the new Objective-C files |
| `src/App.tsx`, `src/styles.css` | The two toggles, the 👂 line, faster rendering |
| `HANDOFF.md` | Technical notes for a future session |
| `README_FIRST.md` | This file |

---

## If something looks wrong

| Symptom | Likely cause | Fix |
|---|---|---|
| Toggle shows a permission message | Permission not granted | Click `settings`, enable the app, toggle again |
| 👂 line stays empty | Screen & System Audio Recording is off | Same as above, then relaunch |
| Bare keys do nothing | Not listening, or Accessibility off | Press Start; check Accessibility |
| Numbers stopped typing in another app | Bare keys are ON and a session is listening | Press Stop, or toggle bare keys off |
| Card appears for the wrong thing | Keyword overlap | Press `⌘⇧⌥D`; wording is tuned in `objection_detect.rs` |

---

## Health check (for the technical reader)

- 75 Rust tests, 24 JavaScript tests, all passing.
- TypeScript, Rust formatting and linting are clean.
- Release build succeeds; the installed app answers `GET /health` with `200`.
- Dialer repository: unchanged (`git status` is empty).

