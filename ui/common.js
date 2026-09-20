export const { invoke } = window.__TAURI__.core;
export const { listen } = window.__TAURI__.event;

export function clock(seconds) {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = String(Math.floor((total % 3600) / 60)).padStart(2, "0");
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${m}:${s}` : `${m}:${s}`;
}

export const BUSY = new Set(["recording", "transcribing", "summarizing"]);
