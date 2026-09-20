# ZillaNote (lite)

Record a meeting, get minutes. One big button; transcription and minutes happen in the
background, on this machine except for the language model you choose.

- **Speech to text:** Qwen3-ASR through a bundled `llama-server`, started only while a
  recording is being transcribed (memory for that time only, none when idle). Settings
  offers four models, each with its download size, the time that takes on this connection
  (measured, not guessed) and the memory it needs: 1.7B at 8 bits (the most accurate, and the
  one for meetings that mix English and Mandarin), 1.7B at 4 bits, 0.6B at 8 bits and 0.6B at
  4 bits (the default: a 0.9 GB download and 1.6 GB of memory while transcribing). Both
  precisions of a size share one audio encoder, so switching costs one file.
- **Minutes:** any OpenAI-compatible endpoint (LM Studio, Ollama, or a hosted API with a key).
  The system prompt, editable in Settings (its default is in
  [`docs/default-system-prompt.md`](docs/default-system-prompt.md)), holds the rules and,
  spelled out, the shape of the minutes: numbered topics with their aspects, decisions, next steps with the owner in
  brackets, and "AI suggestions" for what was raised and left open. It asks for a synthesis
  on one page, for mis-heard terms and numbers in their written form, and for dates in place
  of "tomorrow". The three templates (discussion flow, business meeting, interview) only say
  how they differ from that shape. The model is also given the meeting's date and the list
  of names and terms from Settings. A prompt that was never edited moves on to the new
  default by itself; an edited one stays, and gets the shape added if it names none. They are written by themselves as soon as the transcript is ready
  (a switch in Settings turns that off). A call that fails for a passing reason (no
  connection, no answer in time, a busy server) is tried twice more; minutes cut short by
  closing the app are written at the next start.
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
- **Window:** it starts as a small bar (38 by 142 points) that floats at the right edge of
  the screen: logo, level meter, record/stop, a progress ring while a meeting is processed.
  The clay-coloured tab at the bottom opens the full window; closing that window (or its down-arrow) goes back to the bar. Drag the
  bar by its logo.
- **Email:** fill in one address in Settings, plus the mail account that sends (its app
  password, or authorization code for QQ and 163), and every set of minutes is mailed there
  as soon as it is written. The server is worked out from the sender's address for Gmail,
  Outlook.com, iCloud, Yahoo, QQ, 163 and 126; any other provider shows a server field.
  "Send a test email" leaves the server's answer on the page and in the log; a refused
  password comes with what that provider wants instead (Gmail: an app password, which needs
  2-Step Verification). A
  mail that fails never fails the meeting: the reason is shown with a "Send again" button.
- **Menu bar:** an ear in the menu bar starts and stops a recording, opens the window,
  imports a recording and quits.
- **Quitting:** the × that appears on the bar under the pointer, the power button in the
  window, the menu-bar icon and ⌘Q all take the same way out. If a recording is running or
  a meeting is being worked on, it asks first and says what happens to it. Whichever way the
  app goes (Quit from the Dock and a shutdown included, where nothing can be asked), a
  recording under way is closed into a whole file and the speech engine is stopped.
- **Import:** "Import a recording", or a file dropped on the window, makes a meeting from a
  recording made elsewhere (WAV anywhere; m4a, mp3, mp4 and the rest through macOS's own
  converter). The original is only read.
- **Secrets:** the installed app keeps the API key and the mail password in the macOS
  keychain, and moves any that an older version left in `settings.json`. If the keychain
  refuses, the secret stays in the file rather than being lost; if it cannot be read (you
  said no to its question), what it holds is left alone, whatever Settings is saved as. Development builds
  (`cargo run`) keep them in `settings.json`, readable only by you: the keychain ties an item
  to the program's signature, and a development build has a new one every time. Use
  `ZILLANOTE_DATA_DIR` for a development data folder of its own.
- **Storage:** plain files, one folder per meeting, under
  `~/Library/Application Support/com.zillanote.lite/meetings/`. Next to it, `zillanote.log`
  says what the app did and when, for the day something did not happen.

## Run

```bash
node scripts/fetch-llama-server.mjs     # once: the pinned speech engine, 26 MB
node scripts/fetch-onnxruntime.mjs      # once: the library the speaker models run on, linked into the app
cargo run -p zillanote                  # the app
```

The Qwen3-ASR model files are picked up from LM Studio if it already has
`ggml-org/Qwen3-ASR-1.7B-GGUF`. Otherwise the window offers to download the chosen model
(0.9 to 2.8 GB), together with the speaker models (32 MB): pinned revisions, checked against
pinned sizes and SHA-256 checksums, and a download that stops continues from where it was.
Every file has a second source for where the first cannot be reached (hf-mirror.com for
Hugging Face, jsDelivr for GitHub); a mirror is not trusted, only the checksum is. Web
requests use the system's TLS, so they work behind the proxies that curl and Safari work
behind. A recording made before the
speech model is there fails at once with that reason, and is transcribed by itself when the
download ends. `ZILLANOTE_DATA_DIR`
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
| `scripts/make-icon.py` | draws the icon (an ear in terracotta on warm paper) and the menu-bar mark; `tauri icon` turns the icon into every format |

## Not built yet

- Windows. The recorder's system audio, the import converter and the keychain are macOS
  code behind `cfg`; each needs its Windows counterpart.
- Signing and notarizing the app, which also ends the keychain question at every update.
