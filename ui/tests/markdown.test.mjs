// node --test ui/tests/
import assert from "node:assert/strict";
import { test } from "node:test";

import { renderMarkdown } from "../markdown.js";

test("minutes structure: headings, bullets, a table and emphasis", () => {
  const html = renderMarkdown(
    [
      "## 决定",
      "- 采用 **Qwen3-ASR**",
      "- Ship in *October*",
      "",
      "| Action | Owner | Due |",
      "|---|---|---|",
      "| Send the deck | unassigned | Friday |",
      "",
      "1. first",
      "2. second",
    ].join("\n"),
  );

  assert.match(html, /<h2>决定<\/h2>/);
  assert.match(html, /<ul><li>采用 <strong>Qwen3-ASR<\/strong><\/li><li>Ship in <em>October<\/em><\/li><\/ul>/);
  assert.match(html, /<th>Owner<\/th>/);
  assert.match(html, /<td>unassigned<\/td>/);
  assert.match(html, /<ol><li>first<\/li><li>second<\/li><\/ol>/);
});

test("model output can never inject markup", () => {
  const html = renderMarkdown('## <img src=x onerror=alert(1)>\n[click](javascript:alert(1)) <script>x</script>');

  assert.doesNotMatch(html, /<img|<script|<a /);
  assert.match(html, /&lt;img src=x onerror=alert\(1\)&gt;/);
});

test("lines of one paragraph stay together and blank lines separate paragraphs", () => {
  assert.equal(renderMarkdown("one\ntwo\n\nthree"), "<p>one<br>two</p>\n<p>three</p>");
});
