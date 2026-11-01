# StealthPrompter

A transparent, always-on-top desktop HUD for macOS that follows your sales script during a live
video call. It listens **only to your microphone**, transcribes your speech in real time, and
semantically tracks which script block you are currently delivering — so the highlight follows
you when you paraphrase, ad-lib, or get interrupted, with no clicking.

Built with **Tauri v2** (Rust + React/TypeScript).

## Status

Early. The overlay shell and build pipeline exist; the AI pipeline is not wired up yet.
See `.zenith/missions/stealthprompter-v1/` for the contract, task list and evidence trail.

## Requirements

- macOS 11+ on Apple Silicon or Intel
- Node 22+ and npm
- Rust stable (`rustup`) — installed via <https://rustup.rs>

## Develop

```bash
npm install
npm run tauri dev
```

## Checks

```bash
npm run typecheck   # tsc --noEmit
npm run test        # vitest
npm run build       # tsc + vite build
cd src-tauri && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings
```

## Build a local app

```bash
npx tauri build --bundles app
```

> Note: `npm run tauri build --bundles app` does **not** work — npm consumes `--bundles` as its
> own config flag and passes `app` to `cargo`, which fails with `unexpected argument 'app'`.

The build is **ad-hoc signed** (`signingIdentity: "-"`). There is no Apple Developer ID and no
notarization, because this app targets the developer's own machine. See
`docs/architecture.md` for the consequences (microphone permission behaviour across rebuilds).

## Design notes

The non-obvious decisions — why audio is captured in Rust instead of the webview, why the HUD is
excluded from screen capture, why the LLM is only a tie-breaker — are recorded in
[`docs/architecture.md`](docs/architecture.md).
