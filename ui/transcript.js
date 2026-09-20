import { escapeHtml } from "./markdown.js";

// The transcript comes as lines of "[mm:ss] Speaker: text". `labels` are the meeting's
// speaker names, so a colon inside what was said is never taken for one.
export function renderTranscript(text, labels = []) {
  return text
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => {
      const match = line.match(/^\[([\d:]+)\]\s*(.*)$/);
      if (!match) return `<p>${escapeHtml(line)}</p>`;
      const speaker = labels.find((label) => match[2].startsWith(`${label}: `));
      const said = speaker ? match[2].slice(speaker.length + 2) : match[2];
      return `<p><time>${match[1]}</time>${speaker ? `<b>${escapeHtml(speaker)}</b> ` : ""}${escapeHtml(said)}</p>`;
    })
    .join("");
}
