# ZillaNote (lite)

Record a meeting, get minutes. One big button; transcription and minutes happen in the
background, on this machine except for the language model you choose.

- **Speech to text:** Qwen3-ASR 1.7B through a bundled `llama-server`, started only while a
  recording is being transcribed (about 3.3 GB of memory for that time, none when idle).
- **Minutes:** any OpenAI-compatible endpoint (LM Studio, Ollama, or a hosted API with a key).
  Three templates: discussion flow, business meeting, interview. The system prompt is
  editable in Settings.
- **Storage:** plain files, one folder per meeting, under
  `~/Library/Application Support/com.zillanote.lite/meetings/`.

## Run

```bash
node scripts/fetch-llama-server.mjs     # once: the pinned speech engine, 26 MB
cargo run -p zillanote                  # the app
```

The Qwen3-ASR model files are picked up from LM Studio if it already has
`ggml-org/Qwen3-ASR-1.7B-GGUF`; otherwise put the two GGUF files in the folder Settings
shows. `ZILLANOTE_DATA_DIR` points the app at another data folder (used by the tests).

## Test

```bash
cargo test                                   # unit tests
node --test ui/tests/*.mjs                   # the Markdown renderer
ZILLANOTE_TEST_AUDIO=/path/to.wav cargo test -p engine live_pipeline -- --ignored --nocapture
cargo test -p qwen3-asr --test live -- --ignored --nocapture
```

## Layout

| Path | What |
|---|---|
| `crates/qwen3-asr` | model files, `llama-server` lifecycle, transcription client |
| `crates/engine` | recorder, pause-based chunking, pipeline, minutes, templates, file store |
| `app` | the Tauri shell: commands, events, one window |
| `ui` | the page: plain HTML, CSS and JavaScript, no build step |

## Not built yet

- System audio capture, so the other side of a Teams or Zoom call is recorded (today: the
  microphone only).
- Speaker labels (diarization) and remembered voices.
- Downloading the model from inside the app.
- A menu-bar icon, import of existing recordings, emailing the minutes.
