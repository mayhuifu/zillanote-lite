# ZillaNote (lite)

Record a meeting, get minutes. One big button; transcription and minutes happen in the
background, on this machine except for the language model you choose.

- **Speech to text:** Qwen3-ASR 1.7B through a bundled `llama-server`, started only while a
  recording is being transcribed (about 3.3 GB of memory for that time, none when idle).
- **Minutes:** any OpenAI-compatible endpoint (LM Studio, Ollama, or a hosted API with a key).
  Three templates: discussion flow, business meeting, interview. The system prompt is
  editable in Settings.
- **Calls:** the computer's own sound is recorded next to the microphone (a second channel
  in `audio.wav`), so the other side of a Teams or Zoom call is in the transcript. It needs
  macOS 14.2 and the "System Audio Recording" permission, asked for on the first recording;
  without either, the microphone is recorded alone. On loudspeakers the microphone hears the
  other side a second time: that echo is found by how the two channels' loudness moves
  together and left out of what is transcribed. It can be turned off in Settings.
- **Speakers:** before transcribing, the recording is searched for who spoke when (pyannote
  segmentation and WeSpeaker embeddings on ONNX Runtime, all on this Mac), and the
  recognizer's chunks are cut where the speaker changes, so every line of the transcript has
  one speaker: "Speaker 1", "Speaker 2". Click a speaker above the transcript to give the
  name; the voice is remembered (`voices.json`) and named by itself in later meetings.
  Recognizing a voice never adds to what is stored about it; only naming does. The two
  models (32 MB) live in `models/speakers/` of the data folder; without them transcripts
  simply have no names.
- **Window:** it starts as a small bar that floats at the right edge of the screen: logo, level
  meter, record/stop, a progress ring while a meeting is processed. The blue tab at the bottom
  opens the full window; closing that window (or its down-arrow) goes back to the bar. Drag the
  bar by its logo.
- **Email:** fill in one address in Settings, plus the mail account that sends (its app
  password, or authorization code for QQ and 163), and every set of minutes is mailed there
  as soon as it is written. The server is worked out from the sender's address for Gmail,
  Outlook.com, iCloud, Yahoo, QQ, 163 and 126; any other provider shows a server field. A
  mail that fails never fails the meeting: the reason is shown with a "Send again" button.
- **Storage:** plain files, one folder per meeting, under
  `~/Library/Application Support/com.zillanote.lite/meetings/`.

## Run

```bash
node scripts/fetch-llama-server.mjs     # once: the pinned speech engine, 26 MB
node scripts/fetch-onnxruntime.mjs      # once: the library the speaker models run on, linked into the app
cargo run -p zillanote                  # the app
```

The Qwen3-ASR model files are picked up from LM Studio if it already has
`ggml-org/Qwen3-ASR-1.7B-GGUF`. Otherwise the window offers to download them (2.8 GB),
together with the speaker models (32 MB): pinned revisions, checked against pinned sizes
and checksums, and a download that stops continues from where it was. `ZILLANOTE_DATA_DIR`
points the app at another data folder (used by the tests).

## Build the installer

```bash
cd app && pnpm dlx @tauri-apps/cli@2.11.4 build -- --offline
```

The `.app` and `.dmg` land in `target/release/bundle/` (13 MB installer, 30 MB installed).
`cargo clean` gives the build folder back whenever disk space matters; a full rebuild takes
about a minute.

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
cargo test -p engine live_download -- --ignored --nocapture     # fetches the speaker models (32 MB) and checks them
cargo test -p engine live_system_audio -- --ignored --nocapture   # plays a sound, expects to hear it in the tap
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
| `scripts/make-icon.py` | draws the icon; `tauri icon` turns it into every format |

## Not built yet

- A menu-bar icon and import of existing recordings.
- Keeping the API key and the mail password in the system keychain (today: `settings.json`, readable only by you).
