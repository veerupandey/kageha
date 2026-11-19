import { expect, it } from "vitest";
import { enhanceJsonBlocks } from "./jsonRenderer";

it("preserves long JSON strings in the formatted view", () => {
  const value = "Beginning. " + "A long JSON field. ".repeat(50) + "Ending & more.";
  const html = enhanceJsonBlocks(`<pre><code class="language-json">${JSON.stringify({ content: value })}</code></pre>`);
  const el = document.createElement("div");
  el.innerHTML = html;
  expect(el.querySelector(".json-formatted")?.textContent).toContain(value);
});
