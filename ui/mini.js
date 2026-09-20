import { invoke, listen, clock } from "./common.js";

const $ = (id) => document.getElementById(id);
const bar = document.querySelector(".bar");
const bars = [...$("meter").children];
const WEIGHTS = [0.45, 0.8, 1, 0.7, 0.5];

let recording = null; // { startedAt }
let doneTimer;

function render() {
  const on = recording !== null;
  bar.classList.toggle("recording", on);
  $("record").title = on ? "Stop recording" : "Start recording";
  $("record").setAttribute("aria-label", $("record").title);
  if (!on) {
    $("timer").textContent = "";
    bars.forEach((el) => (el.style.height = "4px"));
  }
}

setInterval(() => {
  if (recording) $("timer").textContent = clock((Date.now() - recording.startedAt) / 1000);
}, 500);

$("record").addEventListener("click", async () => {
  $("record").disabled = true;
  try {
    await invoke(recording ? "stop_recording" : "start_recording");
  } catch (error) {
    // The bar has no room for a message; the full window shows what went wrong.
    showStatus("failed", String(error));
  } finally {
    $("record").disabled = false;
  }
});

$("expand").addEventListener("click", () => invoke("show_main"));
$("status").addEventListener("click", () => invoke("show_main"));

function showStatus(kind, title, progress = 0) {
  const el = $("status");
  clearTimeout(doneTimer);
  el.hidden = false;
  el.className = `status ${kind}`;
  el.title = title;
  el.style.setProperty("--progress", progress);
  el.querySelector(".glyph").textContent = kind === "done" ? "✓" : kind === "failed" ? "!" : "";
  if (kind === "done") doneTimer = setTimeout(() => (el.hidden = true), 6000);
}

await listen("recording-changed", (event) => {
  recording = event.payload ? { startedAt: Date.now() - event.payload.elapsed_seconds * 1000 } : null;
  render();
});

await listen("recording-level", (event) => {
  if (!recording) return;
  // Speech sits around 0.02 to 0.2 RMS; spread that over the bar heights.
  const level = Math.min(1, Math.sqrt(event.payload) * 2.2);
  bars.forEach((el, i) => {
    const wobble = 0.75 + Math.random() * 0.5;
    el.style.height = `${Math.round(4 + 16 * level * WEIGHTS[i] * wobble)}px`;
  });
});

await listen("meeting-updated", (event) => {
  const meeting = event.payload;
  if (meeting.status === "transcribing") {
    const percent = Math.round((meeting.progress || 0) * 100);
    showStatus("progress", `Transcribing ${percent}%`, meeting.progress || 0);
  } else if (meeting.status === "summarizing") {
    showStatus("spin", "Writing minutes");
  } else if (meeting.status === "done") {
    showStatus("done", meeting.has_minutes ? "Minutes are ready" : "Transcript is ready");
  } else if (meeting.status === "failed") {
    showStatus("failed", meeting.error || "Something went wrong");
  }
});

const current = await invoke("recording_state");
if (current) recording = { startedAt: Date.now() - current.elapsed_seconds * 1000 };
render();
invoke("ui_ready");
