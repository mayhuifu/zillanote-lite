# Building ZillaNote lite

How to run, build and test it from source. What each part does is in
[`HOW-IT-WORKS.md`](HOW-IT-WORKS.md); how to send a change is in
[`CONTRIBUTING.md`](../CONTRIBUTING.md).

## Run

```bash
node scripts/fetch-llama-server.mjs     # once: the pinned speech engine, 26 MB
node scripts/fetch-onnxruntime.mjs      # once: the library the speaker models run on, linked into the app
cargo run -p zillanote                  # the app
```

On the first start the window offers to download the speech model and the speaker models
(see [Models and downloads](HOW-IT-WORKS.md#models-and-downloads)). `ZILLANOTE_DATA_DIR`
points the app at another data folder (used by the tests).

## Build the installer

```bash
cd app && pnpm dlx @tauri-apps/cli@2.11.4 build -- --offline
```

The `.app` and `.dmg` land in `target/release/bundle/` (20 MB installer, 45 MB installed, of which the speech engine is 24 MB and ONNX Runtime 15 MB).
`cargo clean` gives the build folder back whenever disk space matters; a full rebuild takes
about a minute.

The bundle is signed ad hoc (`signingIdentity: "-"`), not with an Apple Developer ID. A Mac
that downloaded the installer therefore refuses the first start: open **System Settings →
Privacy & Security**, find "ZillaNote was blocked", and press **Open Anyway**; or run
`xattr -dr com.apple.quarantine /Applications/ZillaNote.app`. Without the ad hoc signature
macOS calls a downloaded copy "damaged" and offers nothing but the Trash.

## Windows

The Windows build is made and started on a Windows machine by
[`.github/workflows/windows.yml`](../.github/workflows/windows.yml) (unit tests, the installer,
and a start of the built app until both pages report ready). Push the branch `windows-port`,
push a version tag, or start the job by hand; the installer
(`ZillaNote_<version>_x64-setup.exe`, for the current user, no administrator needed) is kept
with the run and attached to a tag's release, also as `ZillaNote_x64-setup.exe` for the
download links that always fetch the latest version. The speech engine for Windows is llama.cpp's
CPU build, fetched by the same script as on the Mac.

## Test

```bash
cargo test                                   # unit tests
node --test ui/tests/*.mjs                   # the Markdown renderer
ZILLANOTE_TEST_AUDIO=/path/to.wav cargo test -p engine live_pipeline -- --ignored --nocapture
                                             # add ZILLANOTE_SPEAKER_MODELS=<folder> to check speaker names too
cargo test -p qwen3-asr --test live -- --ignored --nocapture
cargo test -p engine live_smtp -- --ignored --nocapture   # real mail servers, wrong password, sends nothing
ZILLANOTE_SPEAKER_MODELS=<folder> ZILLANOTE_TEST_AUDIO=/path/to.wav \
  cargo test -p speakers --test live -- --ignored --nocapture    # the speaker models on a real recording
cargo test -p engine live_keychain -- --ignored --nocapture     # a throwaway item in the login keychain
cargo test -p engine live_download -- --ignored --nocapture     # fetches the speaker models (32 MB) and checks them
cargo test -p engine live_system_audio -- --ignored --nocapture   # plays a sound, expects to hear it in the tap
cargo test -p engine live_microphone_users -- --ignored --nocapture   # prints who holds the microphone, ten seconds long
cargo test -p engine live_recording -- --ignored --nocapture      # plays 8 s of noise, records both channels
```

## Layout

| Path | What |
|---|---|
| `crates/qwen3-asr` | model files, `llama-server` lifecycle, transcription client |
| `crates/speakers` | who spoke when: segmentation, Kaldi filterbank features, voice embeddings, clustering, matching named voices (from Anarlog's MIT layer, see `LICENSE-ANARLOG`) |
| `crates/engine` | recorder (microphone and system audio), mixdown, pause-based chunking, cutting at speaker turns, named voices, model download, pipeline, minutes, templates, file store |
| `app` | the Tauri shell: commands, events, one window |
| `ui` | two pages (`mini.html` the bar, `index.html` the full window): plain HTML, CSS and JavaScript, no build step |
| `scripts/make-icon.py` | draws the icon (an ear in violet light on graphite) and the menu-bar mark; `tauri icon` turns the icon into every format |
| `scripts/third-party-crates.py` | rewrites the crate table at the end of `THIRD-PARTY-NOTICES.md` from `Cargo.lock` |
