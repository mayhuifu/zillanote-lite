import { BUSY, clock, invoke, listen } from "./common.js";
import { renderMarkdown, escapeHtml } from "./markdown.js";

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

  $("minutes").innerHTML = detail.minutes
    ? renderMarkdown(detail.minutes)
    : `<p class="placeholder">${busy ? "Working on it…" : "No minutes yet."}</p>`;
  $("transcript").innerHTML = detail.transcript
    ? renderTranscript(detail.transcript)
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

function renderTranscript(text) {
  return text
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => {
      const match = line.match(/^\[([\d:]+)\]\s*(.*)$/);
      return match
        ? `<p><time>${match[1]}</time>${escapeHtml(match[2])}</p>`
        : `<p>${escapeHtml(line)}</p>`;
    })
    .join("");
}

function renderTabs() {
  for (const tab of document.querySelectorAll(".tab")) {
    tab.classList.toggle("active", tab.dataset.tab === activeTab);
  }
  $("minutes").hidden = activeTab !== "minutes";
  $("transcript").hidden = activeTab !== "transcript";
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
  if (!ready.model_found) {
    problems.push(
      `The Qwen3-ASR model files were not found. Download ggml-org/Qwen3-ASR-1.7B-GGUF in LM Studio, or put the two GGUF files in ${ready.model_install_dir}`,
    );
  }
  if (!ready.llm_configured) {
    problems.push("No language model is chosen yet, so recordings will be transcribed but get no minutes. Open Settings.");
  }
  $("notice").hidden = problems.length === 0;
  $("notice").textContent = problems.join("\n\n");
  $("readiness").textContent = [
    `Speech model: ${ready.model_found ? ready.model_location : "not found"}`,
    `Speech engine: ${ready.server_found ? ready.server_location : "not found"}`,
  ].join("\n");
}

$("open-settings").addEventListener("click", async () => {
  const settings = await call("get_settings");
  $("llm-url").value = settings.llm_base_url;
  $("llm-model").value = settings.llm_model;
  $("llm-key").value = settings.llm_api_key;
  $("default-template").value = settings.default_template;
  $("system-prompt").value = settings.system_prompt;
  $("vocabulary").value = settings.vocabulary.join("\n");
  $("email-to").value = settings.email.to;
  $("email-from").value = settings.email.from;
  $("email-password").value = settings.email.password;
  $("email-server").value = settings.email.server;
  showServerField();
  $("settings").dataset.maxChars = settings.max_chars_per_call;
  await showReadiness();
  $("settings").showModal();
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
  try {
    await call("send_test_email", { settings: readSettings() });
    toast(`Test sent to ${$("email-to").value.trim()}`);
  } finally {
    button.disabled = false;
    button.textContent = "Send a test email";
  }
});

$("reset-prompt").addEventListener("click", async () => {
  $("system-prompt").value = await call("default_system_prompt");
});
$("cancel-settings").addEventListener("click", () => $("settings").close());
$("collapse").addEventListener("click", () => invoke("show_mini"));

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
