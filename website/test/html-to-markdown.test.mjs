import { test } from "node:test";
import assert from "node:assert/strict";
import { toMarkdown } from "../scripts/html-to-markdown.mjs";

test("semantic tables retain headers, rows, and inline content between page blocks", () => {
  const html = `<main><h2>Protocols</h2><div><table>
    <thead><tr><th scope="col">Protocol</th><th scope="col">Use</th></tr></thead>
    <tbody>
      <tr><td>Responses</td><td><code>kind = &quot;codex&quot;</code></td></tr>
      <tr><td>Chat Completions</td><td><a href="/docs">Coding</a></td></tr>
      <tr><td>Messages</td><td><strong>Claude</strong></td></tr>
      <tr><td>Decisions</td><td>Routing<br>and\n review: <code>a | b</code> &amp; c</td></tr>
    </tbody></table></div><p>After the table.</p></main>`;
  assert.equal(toMarkdown(html, "https://horde.sh"), [
    "## Protocols",
    [
      "| Protocol | Use |",
      "| --- | --- |",
      '| Responses | `kind = "codex"` |',
      "| Chat Completions | [Coding](https://horde.sh/docs) |",
      "| Messages | **Claude** |",
      "| Decisions | Routing and review: `a \\| b` & c |",
    ].join("\n"),
    "After the table.",
  ].join("\n\n"));
});

test("tables without a header keep the first data row and pad missing cells", () => {
  assert.equal(toMarkdown("<main><table><tr><td>A</td><td>B</td></tr><tr><td>C</td></tr></table></main>"), [
    "|  |  |",
    "| --- | --- |",
    "| A | B |",
    "| C |  |",
  ].join("\n"));
});

test("empty tables do not emit empty Markdown blocks", () => {
  assert.equal(toMarkdown("<main><p>Before.</p><table></table><p>After.</p></main>"), "Before.\n\nAfter.");
});
