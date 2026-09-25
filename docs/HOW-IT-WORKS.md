# How ZillaNote lite works

Record a meeting, get minutes. One big button; transcription and minutes happen in the
background, on this machine except for the language model you choose. This page says what
each part does in detail; [`DEVELOPMENT.md`](DEVELOPMENT.md) says how to build and test it.

## The parts

- **Speech to text:** Qwen3-ASR through a bundled `llama-server`, started only while a
  recording is being transcribed (memory for that time only, none when idle). Settings
  offers two models, each with its download size, the time that takes on this connection
  (measured, not guessed) and the memory it needs: 1.7B at 8 bits (the most accurate, and the
  one for meetings that mix English and Mandarin; 2.8 GB to download, 3.5 GB of memory) and
  0.6B at 4 bits (the default: a 0.9 GB download and 1.6 GB of memory while transcribing).
  Settings that name one of the two in-between choices of earlier versions are read as the
  nearest of these. A downloaded model has a Delete button, to give the space back after
  switching to the other one (or to a service); only ZillaNote's own copy is deleted, never
  files LM Studio keeps, and not while a meeting is being worked on or a download runs.
- **A speech service instead:** Settings → Speech recognition can name a service with
  OpenAI's transcription API (OpenAI, Groq, SiliconFlow, a Whisper server of your own):
  endpoint, model and API key, with a Test button that sends a second of a quiet tone.
  Speakers are still found on this computer; each stretch of speech is then sent to the
  service with the names and terms from Settings, and no speech model has to be downloaded.
  A busy service is tried twice more before a stretch is given up. The key is kept like the
  language model's.
- **Minutes:** any OpenAI-compatible endpoint (LM Studio, Ollama, or a hosted API with a key).
  The system prompt, editable in Settings (its default is in
  [`default-system-prompt.md`](default-system-prompt.md)), holds the rules and,
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
- **When a call starts and ends:** the system says which programs have the microphone
  open, and every call program (Teams, Zoom, Tencent Meeting, a browser on Meet) holds it
  for exactly as long as the call. So when one opens the microphone, the bar's record button flashes and the
  window says "Meeting starts"; and when it lets go during a recording, the recording
  stops half a minute later as if the button had been pressed, after a countdown in the
  bar's timer and in the window, with "Keep recording" a click away. A program counts as a
  call only after a minute with the microphone, so dictation and voice messages change
  nothing; a program that opens it again within the half minute (a reconnect, the next
  meeting) is the call going on. Nothing is installed and no permission is asked; both are
  switches in Settings. Nothing ever starts a recording by itself.
- **Speakers:** before transcribing, the recording is searched for who spoke when (pyannote
  segmentation-3.0 and WeSpeaker's ResNet34 embedding on ONNX Runtime, all on this Mac), and the
  recognizer's chunks are cut where the speaker changes, so every line of the transcript has
  one speaker: "Speaker 1", "Speaker 2". Click a speaker above the transcript to give the
  name; the voice is remembered (`voices.json`) and named by itself in later meetings.
  Recognizing a voice never adds to what is stored about it; only naming does. The two
  models (32 MB, from this project's `speaker-models-1` release) live in `models/speakers/`
  of the data folder; without them transcripts simply have no names.
- **Window:** it starts as a small bar (38 by 142 points) that floats at the right edge of
  the screen: logo, level meter, record/stop, a progress ring while a meeting is processed.
  The violet tab at the bottom opens the full window; closing that window (or its down-arrow) goes back to the bar. Drag the
  bar by its logo.
- **Email:** fill in one address in Settings, plus the mail account that sends (its app
  password, or authorization code for QQ and 163), and every set of minutes is mailed there
  as soon as it is written, under a subject with the day and what the meeting was about
  ("Minutes 2026-09-21: Budget and revenue": the meeting's name if you gave it one, else the
  first heading of the minutes that says something). The server is worked out from the sender's address for Gmail,
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
- **Settings:** the first page holds the switches for recording and minutes; Speech
  recognition, Language model, Prompt and vocabulary, Email and Remembered voices each open
  a page of their own, with a line saying what is set there or what is missing. One Save
  keeps the changes of every page.
- **Secrets:** the installed app keeps the API keys and the mail password in the macOS
  keychain, and moves any that an older version left in `settings.json`. If the keychain
  refuses, the secret stays in the file rather than being lost; if it cannot be read (you
  said no to its question), what it holds is left alone, whatever Settings is saved as. Development builds
  (`cargo run`) keep them in `settings.json`, readable only by you: the keychain ties an item
  to the program's signature, and a development build has a new one every time. Use
  `ZILLANOTE_DATA_DIR` for a development data folder of its own.
- **Storage:** plain files, one folder per meeting, under
  `~/Library/Application Support/com.zillanote.lite/meetings/`. Next to it, `zillanote.log`
  says what the app did and when, for the day something did not happen.

## Models and downloads

The Qwen3-ASR model files are picked up from LM Studio if it already has
`ggml-org/Qwen3-ASR-1.7B-GGUF`. Otherwise the window offers to download the chosen model
(0.9 to 2.8 GB), together with the speaker models (32 MB): pinned revisions, checked against
pinned sizes and SHA-256 checksums, and a download that stops continues from where it was.
Every file has a second source for where the first cannot be reached (hf-mirror.com for
Hugging Face, jsDelivr for GitHub); a mirror is not trusted, only the checksum is. Web
requests use the system's TLS, so they work behind the proxies that curl and Safari work
behind. A recording made before the
speech model is there fails at once with that reason, and is transcribed by itself when the
download ends.

## Privacy

Audio is recorded to a file on this computer and read by programs on this computer: the
bundled speech engine and the speaker models. The one exception is your own choice: with a
speech service set in Settings → Speech recognition, each stretch of speech is sent to that
service to be transcribed. The transcript goes to one place, the
language-model endpoint set in Settings, which can be a program on this computer (LM Studio,
Ollama) or a hosted API of your choosing; the minutes go to the mail server set in Settings,
if any. Model files are downloaded from Hugging Face and GitHub, or their mirrors, and
checked against pinned checksums. Nothing else leaves the computer, and nothing is sent to
the authors of ZillaNote.

## On Windows

A preview: it is built and started on a Windows machine by the build job (see
[`DEVELOPMENT.md`](DEVELOPMENT.md#windows)), and has not yet been used for a real meeting
there.

- The computer's sound comes through WASAPI loopback, so the far side of a call is recorded
  as on the Mac, without a permission question.
- Who has the microphone open is read from the audio sessions of every microphone, so the
  flashing button and the stop after a call work the same; only checked by the build so far.
- The speech engine is llama.cpp's CPU build for Windows.
- The bar is the 38 by 142 points it is on the Mac. Windows has a smallest width of its own
  for a window, several times that; the app overrides it for the bar at start, and the build
  job fails if the bar reports any other size.
- The installer is not signed, so SmartScreen says "Windows protected your PC": choose
  **More info**, then **Run anyway**.
- Settings, meetings and the log are under `%APPDATA%\com.zillanote.lite\`.

## Not built yet

- On Windows: the API key and mail password in the Credential Manager (they stay in
  `settings.json` there), and importing anything but WAV (macOS's converter does that on
  the Mac).
- Signing and notarizing the app, which also ends the keychain question at every update.
