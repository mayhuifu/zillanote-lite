import { BUSY, clock, invoke, listen } from "./common.js";
import { renderMarkdown, escapeHtml } from "./markdown.js";
import { renderTranscript } from "./transcript.js";

const $ = (id) => document.getElementById(id);

const STATUS_LABEL = {
  recording: "Recording",
  transcribing: "Transcribing",
  summarizing: "Writing minutes",
  done: "Done",
  failed: "Failed",
};

let meetings = [];
let openId = null;
let activeTab = "minutes";
let recording = null; // { meetingId, startedAt }
let templates = [];
let speakerLabels = [];
let missing = null; // { bytes, what } while models are still to be downloaded

// --- helpers ---

function toast(message) {
  const el = $("toast");
  el.textContent = message;
  el.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => (el.hidden = true), 3500);
}

async function call(command, args) {
  try {
    return await invoke(command, args);
  } catch (error) {
    toast(String(error));
    throw error;
  }
}

function when(meeting) {
  const date = new Date(meeting.created_at);
  const day = date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const time = date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  const length = meeting.duration_seconds ? ` · ${clock(meeting.duration_seconds)}` : "";
  return `${day}, ${time}${length}`;
}

// --- recorder ---

function renderRecorder() {
  const button = $("record");
  const on = recording !== null;
  button.classList.toggle("recording", on);
  button.setAttribute("aria-label", on ? "Stop recording" : "Start recording");
  $("hint").textContent = on
    ? "Recording. Click to stop; the rest happens in the background."
    : "Click to start recording";
  if (!on) {
    $("timer").textContent = "00:00";
    button.style.removeProperty("--level");
  }
}

setInterval(() => {
  if (recording) $("timer").textContent = clock((Date.now() - recording.startedAt) / 1000);
}, 500);

$("record").addEventListener("click", async () => {
  const button = $("record");
  button.disabled = true;
  try {
    await call(recording ? "stop_recording" : "start_recording");
  } finally {
    button.disabled = false;
    renderRecorder();
  }
});

// --- import ---

async function importRecording(path) {
  const meeting = await call("import_recording", { path });
  if (meeting) toast(`Importing ${meeting.title}`);
}
$("import").addEventListener("click", () => importRecording(null));

// --- meetings list ---

function upsert(meeting) {
  const at = meetings.findIndex((m) => m.id === meeting.id);
  if (at >= 0) meetings[at] = meeting;
  else meetings.unshift(meeting);
  renderList();
  if (openId === meeting.id) showDetail(meeting.id, { keepTab: true });
}

function renderList() {
  const list = $("meeting-list");
  list.innerHTML = "";
  $("empty").hidden = meetings.length > 0;

  for (const meeting of meetings) {
    const item = document.createElement("li");
    const busy = BUSY.has(meeting.status);
    const percent = Math.round((meeting.progress || 0) * 100);
    const label =
      meeting.status === "transcribing" && percent > 0
        ? `Transcribing ${percent}%`
        : meeting.status === "done" && !meeting.has_minutes
          ? "Transcript only"
          : STATUS_LABEL[meeting.status];

    item.innerHTML = `
      <div class="row">
        <span class="name">${escapeHtml(meeting.title)}</span>
        <span class="chip ${meeting.status}">${label}</span>
      </div>
      <div class="when">${escapeHtml(when(meeting))}</div>
      ${
        busy && meeting.status !== "recording"
          ? `<div class="progress ${meeting.status === "summarizing" ? "indeterminate" : ""}">
               <span style="width:${percent}%"></span>
             </div>`
          : ""
      }`;
    item.addEventListener("click", () => showDetail(meeting.id));
    list.appendChild(item);
  }
}

// --- detail ---

async function showDetail(id, { keepTab = false } = {}) {
  const detail = await call("get_meeting", { id });
  // A refresh that comes back after the user has left this meeting must not bring it back.
  if (keepTab && openId !== id) return;
  const { meeting } = detail;
  openId = id;
  if (!keepTab) activeTab = meeting.has_minutes || !meeting.has_transcript ? "minutes" : "transcript";

  $("home").hidden = true;
  $("detail").hidden = false;
  if (document.activeElement !== $("title")) $("title").value = meeting.title;
  $("detail-meta").textContent = `${when(meeting)} · ${STATUS_LABEL[meeting.status]}`;

  const mail = $("detail-email");
  mail.hidden = !(meeting.emailed_at || meeting.email_error);
  mail.classList.toggle("problem", Boolean(meeting.email_error));
  mail.firstElementChild.textContent = meeting.email_error
    ? `Email not sent: ${meeting.email_error}`
    : meeting.emailed_at
      ? `Emailed to ${meeting.emailed_to} at ${new Date(meeting.emailed_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`
      : "";

  $("detail-error").hidden = !meeting.error;
  $("detail-error").textContent = meeting.error || "";

  const busy = BUSY.has(meeting.status);
  const bar = $("detail-progress");
  bar.hidden = !busy || meeting.status === "recording";
  bar.classList.toggle("indeterminate", meeting.status === "summarizing");
  bar.firstElementChild.style.width = `${Math.round((meeting.progress || 0) * 100)}%`;

  speakerLabels = detail.speakers.map((speaker) => speaker.label);
  renderSpeakers(detail.speakers);
  $("minutes").innerHTML = detail.minutes
    ? renderMarkdown(detail.minutes)
    : `<p class="placeholder">${busy ? "Working on it…" : "No minutes yet."}</p>`;
  $("transcript").innerHTML = detail.transcript
    ? renderTranscript(detail.transcript, speakerLabels)
    : `<p class="placeholder">${busy ? "Working on it…" : "No transcript yet."}</p>`;
  $("minutes").dataset.source = detail.minutes || "";
  $("transcript").dataset.source = detail.transcript || "";

  $("template").value = meeting.template;
  $("rewrite").textContent = meeting.has_minutes ? "Rewrite minutes" : "Write minutes";
  $("rewrite").disabled = busy || !meeting.has_transcript;
  $("reprocess").disabled = busy;
  $("delete").disabled = meeting.status === "recording";
  renderTabs();
}

// One chip per speaker. Clicking one turns it into a field for the person's name.
function renderSpeakers(speakers) {
  const strip = $("speakers");
  // Progress events redraw this view many times a minute; a name being typed stays.
  if (strip.contains(document.activeElement) && document.activeElement.matches(".speaker-name")) return;
  strip.innerHTML = "";
  for (const speaker of speakers) {
    const chip = document.createElement("button");
    chip.className = `speaker ${speaker.named ? "named" : ""}`;
    chip.title = speaker.named ? "Change the name" : "Name this speaker, and ZillaNote knows the voice next time";
    chip.textContent = `${speaker.label} · ${clock(speaker.seconds)}`;
    chip.addEventListener("click", () => editSpeaker(chip, speaker));
    strip.appendChild(chip);
  }
  strip.dataset.count = speakers.length;
  renderTabs();
}

function editSpeaker(chip, speaker) {
  const id = openId;
  const field = document.createElement("input");
  field.className = "speaker-name";
  field.placeholder = "Who is this?";
  field.value = speaker.named ? speaker.label : "";
  chip.replaceWith(field);
  field.focus();

  let settled = false;
  const settle = async (save) => {
    if (settled) return;
    settled = true;
    const name = field.value.trim();
    try {
      if (save && name !== (speaker.named ? speaker.label : "")) {
        await call("name_speaker", { id, index: speaker.index, name });
        toast(name ? `${name} it is. Rewrite the minutes to use the name there too.` : "Name removed");
      }
    } finally {
      // Whatever happened, the field goes and the chips show what is stored; but only if
      // this meeting is still the one on screen.
      field.remove();
      if (openId === id) showDetail(id, { keepTab: true });
    }
  };
  field.addEventListener("keydown", (event) => {
    if (event.key === "Enter") settle(true);
    if (event.key === "Escape") settle(false);
  });
  field.addEventListener("blur", () => settle(true));
}

function renderTabs() {
  for (const tab of document.querySelectorAll(".tab")) {
    tab.classList.toggle("active", tab.dataset.tab === activeTab);
  }
  $("minutes").hidden = activeTab !== "minutes";
  $("transcript").hidden = activeTab !== "transcript";
  $("speakers").hidden = activeTab !== "transcript" || !Number($("speakers").dataset.count);
}

for (const tab of document.querySelectorAll(".tab")) {
  tab.addEventListener("click", () => {
    activeTab = tab.dataset.tab;
    renderTabs();
  });
}

$("back").addEventListener("click", () => {
  openId = null;
  $("detail").hidden = true;
  $("home").hidden = false;
});

$("title").addEventListener("change", async () => {
  if (openId) upsert(await call("rename_meeting", { id: openId, title: $("title").value }));
});
$("title").addEventListener("keydown", (event) => {
  if (event.key === "Enter") $("title").blur();
});

$("copy").addEventListener("click", async () => {
  const source = $(activeTab).dataset.source;
  if (!source) return;
  await navigator.clipboard.writeText(source);
  toast(activeTab === "minutes" ? "Minutes copied" : "Transcript copied");
});

$("reveal").addEventListener("click", () => call("reveal_meeting", { id: openId }));
$("rewrite").addEventListener("click", () =>
  call("rewrite_minutes", { id: openId, template: $("template").value }),
);
$("reprocess").addEventListener("click", () => call("process_meeting", { id: openId }));
$("send-email").addEventListener("click", async () => {
  await call("send_minutes", { id: openId });
  toast("Sending…");
});
$("delete").addEventListener("click", async () => {
  if (!confirm("Delete this meeting, its recording and its minutes?")) return;
  await call("delete_meeting", { id: openId });
  meetings = meetings.filter((m) => m.id !== openId);
  renderList();
  $("back").click();
});

// --- settings ---

async function showReadiness() {
  const ready = await call("readiness");
  const problems = [];
  if (!ready.server_found) {
    problems.push("The speech engine (llama-server) was not found. Run: node scripts/fetch-llama-server.mjs");
  }
  if (!ready.llm_configured) {
    problems.push("No language model is chosen yet, so recordings will be transcribed but get no minutes. Open Settings.");
  }
  $("notice").hidden = problems.length === 0;
  $("notice").textContent = problems.join("\n\n");
  missing = ready.download_bytes
    ? {
        bytes: ready.download_bytes,
        what: !ready.model_found
          ? `The speech model (${ready.model_name}) is not on this Mac yet. Recordings are kept, and transcribed once it is here. Settings has smaller models to choose from.`
          : "Two small models are needed to tell speakers apart.",
      }
    : null;
  renderDownload(await call("download_status"));
  return ready;
  $("readiness").textContent = [
    `Speech model: ${ready.model_found ? ready.model_location : "not found"}`,
    `Speech engine: ${ready.server_found ? ready.server_location : "not found"}`,
    `Speaker names: ${ready.speaker_models_found ? "on" : "off, the speaker models are not installed"}`,
  ].join("\n");
}

// --- choosing the speech model ---

let downloadSpeed; // bytes a second on this connection; undefined until measured, null if unreachable

function duration(seconds) {
  const minutes = Math.max(1, Math.round(seconds / 60));
  return minutes < 60 ? `about ${minutes} min` : `about ${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}

function modelFacts(model) {
  const size = `${gigabytes(model.size_bytes)} download`;
  const memory = `needs ${gigabytes(model.memory_bytes)} of memory`;
  if (!model.missing_bytes) return `On this Mac · ${gigabytes(model.size_bytes)} · ${memory}`;
  const left = model.missing_bytes < model.size_bytes ? `${gigabytes(model.missing_bytes)} still to download` : size;
  const time =
    downloadSpeed === undefined
      ? "measuring your connection…"
      : downloadSpeed
        ? `${duration(model.missing_bytes / downloadSpeed)} on this connection`
        : "download servers not reachable right now";
  return `${left} · ${time} · ${memory}`;
}

function renderModels(ready) {
  $("models-help").textContent =
    `Transcription runs on this Mac (${Math.round(ready.total_memory_bytes / 2 ** 30)} GB of memory). The bigger the model, the fewer mis-heard words; it needs the memory shown only while a recording is being transcribed.`;
  const list = $("models");
  list.innerHTML = "";
  for (const model of ready.models) {
    const option = document.createElement("label");
    option.className = "model";
    option.innerHTML = `
      <input type="radio" name="asr-model" value="${model.id}" ${model.chosen ? "checked" : ""} />
      <span>
        <span class="name"><b>${escapeHtml(model.name)}</b>${model.is_default ? ' <em class="tag">default</em>' : ""}</span>
        <small>${escapeHtml(model.description)}</small>
        <small class="facts">${escapeHtml(modelFacts(model))}</small>
      </span>`;
    list.appendChild(option);
  }
}

async function measureConnection(ready) {
  if (downloadSpeed !== undefined || !ready.models.some((model) => model.missing_bytes)) return;
  downloadSpeed = (await invoke("probe_download_speed").catch(() => null)) || null;
  // Keep what the user has picked meanwhile.
  const picked = document.querySelector('input[name="asr-model"]:checked')?.value;
  renderModels({ ...ready, models: ready.models.map((model) => ({ ...model, chosen: model.id === picked })) });
}

// --- downloading the models ---

const gigabytes = (bytes) => (bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`);

function renderDownload(status) {
  $("download").hidden = !missing && !status.running;
  if ($("download").hidden) return;

  const bar = $("download-bar");
  bar.hidden = !status.running;
  bar.firstElementChild.style.width = `${status.total_bytes ? (100 * status.done_bytes) / status.total_bytes : 0}%`;
  $("download-start").hidden = status.running;
  $("download-stop").hidden = !status.running;
  $("download-start").textContent = status.error || status.done_bytes ? "Continue the download" : `Download (${gigabytes(missing?.bytes || 0)})`;
  $("download-text").textContent = status.running
    ? `Downloading: ${gigabytes(status.done_bytes)} of ${gigabytes(status.total_bytes)}`
    : status.error
      ? `${status.error} What has arrived is kept.`
      : missing?.what || "";
}

$("download-start").addEventListener("click", async () => {
  await call("download_models");
  renderDownload({ running: true, done_bytes: 0, total_bytes: missing?.bytes || 0 });
});
$("download-stop").addEventListener("click", () => call("cancel_download"));

async function showVoices() {
  const voices = await call("list_voices");
  const list = $("voices");
  list.innerHTML = voices.length ? "" : `<li class="help">None yet.</li>`;
  for (const voice of voices) {
    const item = document.createElement("li");
    item.innerHTML = `<span>${escapeHtml(voice.name)}</span>`;
    const forget = document.createElement("button");
    forget.type = "button";
    forget.className = "text danger";
    forget.textContent = "Forget";
    forget.addEventListener("click", async () => {
      await call("forget_voice", { id: voice.id });
      showVoices();
    });
    item.appendChild(forget);
    list.appendChild(item);
  }
}

$("open-settings").addEventListener("click", async () => {
  const settings = await call("get_settings");
  $("llm-url").value = settings.llm_base_url;
  $("llm-model").value = settings.llm_model;
  $("llm-key").value = settings.llm_api_key;
  $("default-template").value = settings.default_template;
  $("system-prompt").value = settings.system_prompt;
  $("vocabulary").value = settings.vocabulary.join("\n");
  $("system-audio").checked = settings.record_system_audio;
  $("auto-minutes").checked = settings.auto_minutes;
  $("email-to").value = settings.email.to;
  $("email-from").value = settings.email.from;
  $("email-password").value = settings.email.password;
  $("email-server").value = settings.email.server;
  showServerField();
  $("settings").dataset.maxChars = settings.max_chars_per_call;
  const ready = await showReadiness();
  renderModels(ready);
  await showVoices();
  $("settings").showModal();
  measureConnection(ready);
});

// The server is only asked for when the sender's provider is not one the app knows.
const KNOWN_MAIL = /@(gmail|googlemail|qq|foxmail|163|126|outlook|hotmail|live|msn|icloud|me|mac|yahoo)\.com$/i;
function showServerField() {
  const from = $("email-from").value.trim();
  $("email-server-row").hidden = !($("email-server").value.trim() || (from.includes("@") && !KNOWN_MAIL.test(from)));
}
$("email-from").addEventListener("input", showServerField);

function readSettings() {
  return {
    llm_base_url: $("llm-url").value.trim(),
    llm_model: $("llm-model").value.trim(),
    llm_api_key: $("llm-key").value.trim(),
    default_template: $("default-template").value,
    system_prompt: $("system-prompt").value,
    vocabulary: $("vocabulary").value.split("\n").map((term) => term.trim()).filter(Boolean),
    max_chars_per_call: Number($("settings").dataset.maxChars) || 24000,
    record_system_audio: $("system-audio").checked,
    auto_minutes: $("auto-minutes").checked,
    asr_model: document.querySelector('input[name="asr-model"]:checked')?.value,
    email: {
      to: $("email-to").value.trim(),
      from: $("email-from").value.trim(),
      password: $("email-password").value.trim(),
      server: $("email-server").value.trim(),
    },
  };
}

$("test-email").addEventListener("click", async () => {
  const button = $("test-email");
  button.disabled = true;
  button.textContent = "Sending…";
  // The answer stays on the page: a mail server's reason is too long for a passing note.
  const result = $("test-email-result");
  result.hidden = true;
  try {
    await invoke("send_test_email", { settings: readSettings() });
    result.textContent = `Test sent to ${$("email-to").value.trim()}. If it does not arrive, look in that mailbox's junk folder.`;
    result.className = "help result ok";
  } catch (error) {
    result.textContent = String(error);
    result.className = "help result problem";
  } finally {
    result.hidden = false;
    button.disabled = false;
    button.textContent = "Send a test email";
  }
});

$("open-system-audio").addEventListener("click", () => call("open_system_audio_settings"));

$("reset-prompt").addEventListener("click", async () => {
  $("system-prompt").value = await call("default_system_prompt");
});
$("cancel-settings").addEventListener("click", () => $("settings").close());
$("collapse").addEventListener("click", () => invoke("show_mini"));
// Asks first when a recording or a meeting in the works would be cut short.
$("quit").addEventListener("click", () => invoke("quit_app"));

$("settings-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  await call("save_settings", { settings: readSettings() });
  $("settings").close();
  toast("Settings saved");
  showReadiness();
});

// --- start ---

await listen("meeting-updated", (event) => upsert(event.payload));
// Recording can be started and stopped from the side bar as well as from here.
await listen("recording-changed", (event) => {
  recording = event.payload
    ? { meetingId: event.payload.meeting_id, startedAt: Date.now() - event.payload.elapsed_seconds * 1000 }
    : null;
  renderRecorder();
});
await listen("notice", (event) => toast(event.payload));
await listen("tauri://drag-drop", async (event) => {
  // One file that is no recording does not stop the others; `call` has shown why.
  for (const path of event.payload.paths || []) await importRecording(path).catch(() => {});
});
await listen("download-progress", (event) => {
  // The end of a download changes what is missing; until then only the numbers move.
  if (event.payload.running) renderDownload(event.payload);
  else showReadiness();
});
await listen("recording-level", (event) => {
  // Speech sits around 0.02 to 0.2 RMS; map that onto a ring between 0.82 and 1.0.
  const level = Math.min(1, Math.sqrt(event.payload) * 1.6);
  $("record").style.setProperty("--level", (0.82 + 0.18 * level).toFixed(3));
});

templates = await call("templates");
for (const select of [$("template"), $("default-template")]) {
  select.innerHTML = templates
    .map((t) => `<option value="${t.id}" title="${escapeHtml(t.description)}">${escapeHtml(t.name)}</option>`)
    .join("");
}

meetings = await call("list_meetings");
const current = await call("recording_state");
if (current) {
  recording = { meetingId: current.meeting_id, startedAt: Date.now() - current.elapsed_seconds * 1000 };
}
renderRecorder();
renderList();
showReadiness();
invoke("ui_ready");
