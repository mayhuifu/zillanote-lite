# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

ZillaNote lite is a small Tauri 2 desktop app (macOS; Windows as a preview) that records a
meeting, finds who spoke, transcribes locally with Qwen3-ASR, writes minutes with any
OpenAI-compatible language model, and mails them. `README.md` is the public landing page;
`docs/HOW-IT-WORKS.md` describes every feature's behaviour in detail and
`docs/DEVELOPMENT.md` the build and the full list of live tests. Keep those in step with
behaviour changes.

## Commands

```bash
node scripts/fetch-llama-server.mjs     # once: pinned llama-server into app/resources/llama-server/ (gitignored)
node scripts/fetch-onnxruntime.mjs      # once: static ONNX Runtime into vendor/onnxruntime/ (gitignored)

ZILLANOTE_DATA_DIR=<scratch dir> cargo run -p zillanote   # the app, on a data folder of its own
cargo test                                                # every crate's unit tests (the root is a virtual workspace)
cargo test -p engine <name filter>                        # one crate, or one test
node --test ui/tests/*.mjs                                # the UI's Markdown and transcript helpers
cd app && pnpm dlx @tauri-apps/cli@2.11.4 build -- --offline   # .app and .dmg into target/release/bundle/
```

Live tests are `#[ignore]`d because they need hardware, the network or model files: run one
with `cargo test -p <crate> <name> -- --ignored --nocapture` and the `ZILLANOTE_*` variables
listed in `docs/DEVELOPMENT.md`. The recorder and system-audio ones play sound for a few
seconds.

Always point development runs and smoke launches at a scratch `ZILLANOTE_DATA_DIR`: the
default folder is the real user's meetings and settings.

## Architecture

A Cargo workspace plus a page folder:

- `crates/engine`: "the whole product minus the window". Recorder, mixdown, chunker,
  pipeline, minutes, templates, email, import, downloads, call watch, secrets, file store.
- `crates/qwen3-asr`: finds the model files, runs `llama-server`, sends audio, cleans the text.
- `crates/speakers`: diarization on ONNX Runtime (pyannote segmentation-3.0 + WeSpeaker
  embedding, clustering, matching named voices). Partly from Anarlog's MIT layer; keep
  `LICENSE-ANARLOG` and the header in `lib.rs` intact.
- `app`: the Tauri shell. `app/src/main.rs` is thin: `#[tauri::command]`s that call
  `engine::pipeline::Pipeline` and `engine::store::Store`, plus events (`meeting-updated`,
  `recording-level`, `recording-changed`, `notice`, `download-progress`, `call-state`) sent to
  both windows. Two windows: `mini` (the 38×142 floating bar, `ui/mini.html`) and `main`
  (`ui/index.html`).
- `ui`: plain HTML, CSS and ES modules. No npm, no bundler, no build step: Tauri serves the
  folder as is (`frontendDist: ../ui`, `withGlobalTauri`), and `ui/common.js` takes
  `invoke`/`listen` from `window.__TAURI__`.

**After Stop** (`Pipeline::process` in `crates/engine/src/pipeline.rs`), one model in memory
at a time:
`audio.wav` (two channels: microphone and the computer's sound) → `mixdown::recognition_signal`
(drops the far side's loudspeaker echo from the mic) → `chunker::speech_chunks` (pause-based)
→ diarization, best effort (no models means no names), and `turns::split_at_turns` so every
chunk has one speaker → a bundled `llama-server` is started, transcribes the chunks and is
stopped again → `minutes::write_minutes` (skipped when `auto_minutes` is off; passing
failures retried) → email if configured. A failure is recorded on the meeting (status
`failed` plus the reason), not returned. Meetings that failed with `MODEL_MISSING` are
processed by themselves once the model download ends.

**Storage** is plain files; the layout is documented at the top of
`crates/engine/src/store.rs`. `zillanote.log` in the data folder records what the app did;
read it first when something "did not happen".

**Call watch:** a thread in `app/src/main.rs` (`watch_calls`) asks once a second which
processes hold the microphone (Core Audio's process list on macOS 14+, WASAPI audio
sessions on Windows). `engine::calls::CallWatch` is the pure, unit-tested state machine. The
thread reads `Store::settings_without_secrets()`, because `settings()` goes to the keychain.

## Things that bite

- **llama-server memory:** keep `MEMORY_LIMIT_ARGS` (`--ctx-size 4096 --parallel 1
  --cache-ram 0`) in `crates/qwen3-asr/src/server.rs`; without them the server takes about
  three times the memory. The `--alias zillanote-lite-qwen3-asr` is how `kill_stale_servers`
  tells our servers from a user's own `llama-server`.
- **The default system prompt** lives in `crates/engine/src/templates.rs`, and
  `docs/default-system-prompt.md` must match it byte for byte (a test checks). Regenerate
  with `ZILLANOTE_WRITE_PROMPT_DOC=1 cargo test -p engine the_prompt_in_docs`. When you
  change the prompt, add the old text to `PREVIOUS_SYSTEM_PROMPTS`, so that users who never
  edited theirs move on by themselves.
- **LF everywhere** (`.gitattributes`): a CRLF checkout breaks that byte-for-byte test on Windows.
- **Renamed model or setting ids** stay readable through serde `alias` plus strum `serialize`
  (see `Qwen3AsrModel`). Never list a name in both `serialize` and `to_string`.
- **Secrets:** only a macOS release build uses the keychain (`cfg(all(target_os = "macos",
  not(debug_assertions)))` in `secrets.rs`). Debug builds keep the API key and mail password
  in `settings.json`, so a dev build on the folder of an installed app has no API key.
- **TLS:** reqwest uses `native-tls` on purpose: some local proxies drop rustls'
  post-quantum ClientHello.
- **ONNX Runtime** is linked statically from `vendor/`. `.cargo/config.toml` turns off
  pkg-config, or a Homebrew copy gets linked dynamically. On Windows the static library needs
  `DirectML` and `dxcore` (`crates/speakers/build.rs`), and `DirectML.dll` ships next to the
  exe (`app/tauri.windows.conf.json`).
- **Windows cannot be built or run on a Mac.** Windows-only code sits behind `cfg(windows)`;
  `cargo check --target x86_64-pc-windows-msvc` of the whole workspace fails in aws-lc-sys,
  so type-check Windows-only code in a scratch crate. The real check is
  `.github/workflows/windows.yml`, which runs on a push to the `windows-port` branch, on a
  `v*` tag, or by hand. It runs the tests, builds the NSIS installer, starts the app until
  `ui_ready` is logged twice, fails unless the logged `bar_size` is 38×142, and on a tag
  attaches the exe to that tag's release (`--clobber`).
- **The Windows bar:** Windows imposes a minimum width on any window with a caption, and
  `narrow_mini()` in `main.rs` undoes it. Keep the app menu off the mini window on Windows.
- **macOS signing is ad hoc** (`signingIdentity: "-"`, `hardenedRuntime: false`); without
  it, a downloaded copy is reported as "damaged". If a Developer ID arrives and the hardened
  runtime is turned on, the microphone needs the `com.apple.security.device.audio-input`
  entitlement.
- **The DMG step fails now and then** (the Finder AppleScript in `bundle_dmg.sh`). Detach
  `/Volumes/dmg.*`, delete `target/release/bundle/macos/rw.*.dmg` and build again.
- **The version** is in both `app/Cargo.toml` and `app/tauri.conf.json`.
- **Third-party notices:** run `scripts/third-party-crates.py` after `Cargo.lock` changes;
  anything newly bundled or downloaded gets its own section in `THIRD-PARTY-NOTICES.md`.

## Looking at the UI without the app

Serve a copy of `ui/` with `common.js` replaced by a stub of `invoke`/`listen` and screenshot
it with headless Chrome (`--headless=new --screenshot`). Headless Chrome has a minimum window
width, so show the 38×142 bar in an iframe of that size on a zoomed page. Turn transitions
off, including on `::before`/`::after`, or pseudo-elements get caught mid-animation.

## Releasing

Bump the version, then push an annotated tag `vX.Y.Z` and create the GitHub release right
away, so the Windows job's attach step finds it. Build the DMG locally and upload it to the
release. The release notes carry the SHA-256 of both installers: take them from the
uploaded assets, because a re-run of the Windows job replaces the exe.

## Style

Comments say why; names say what; no new dependency without a reason (see
`CONTRIBUTING.md`). Test names are sentences
(`an_untouched_prompt_moves_on_with_the_app_and_an_edited_one_stays`). Commit subjects are
plain descriptions, often `Area: what changes` ("Windows: the bar at the width it was
drawn, not at Windows' smallest window width").
