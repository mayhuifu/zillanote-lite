import assert from "node:assert/strict";
import test from "node:test";

import { renderTranscript } from "../transcript.js";

test("a speaker's name is set apart from what was said", () => {
  const html = renderTranscript("[00:02] Speaker 1: Hello there.\n[1:02:05] Hui: 我们开始。", ["Speaker 1", "Hui"]);

  assert.equal(
    html,
    "<p><time>00:02</time><b>Speaker 1</b> Hello there.</p><p><time>1:02:05</time><b>Hui</b> 我们开始。</p>",
  );
});

test("a colon in what was said is not a speaker", () => {
  const html = renderTranscript("[00:10] Note: this is said, not a name.", ["Speaker 1"]);

  assert.equal(html, "<p><time>00:10</time>Note: this is said, not a name.</p>");
});

test("names and speech are escaped", () => {
  const html = renderTranscript("[00:10] <b>x</b>: a < b", ["<b>x</b>"]);

  assert.equal(html, "<p><time>00:10</time><b>&lt;b&gt;x&lt;/b&gt;</b> a &lt; b</p>");
});
