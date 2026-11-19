import { describe, expect, it } from "vitest";
import { renderMarkdownCached, renderMarkdownHtml, renderStreamingMarkdownHtml } from "./markdownHtml";

describe("Markdown cache identity", () => {
  for (const render of [
    (text: string) => renderStreamingMarkdownHtml(text, "collision-chat"),
    (text: string) => renderMarkdownCached("same-message", text, "collision-chat"),
  ]) {
    it("distinguishes equal-length edits with identical surrounding content", () => {
      const prefix = "Shared introduction. ".repeat(6);
      const suffix = " Shared closing text.".repeat(6);
      const first = `${prefix}alpha${suffix}`;
      const second = `${prefix}omega${suffix}`;
      expect(first.length).toBe(second.length);
      expect(render(first)).toContain("alpha");
      const updated = render(second);
      expect(updated).toContain("omega");
      expect(updated).not.toContain("alpha");
      expect(render(first)).toContain("alpha");
    });
  }
});

it("shows malformed wire calls as an error instead of concatenated prose", () => {
  const output = renderMarkdownHtml("I'll search.<tool_call>web_search<arg_key>query</arg_key><arg_value>DeepSeek</arg_value></tool_call>");
  expect(output).toContain('role="alert"');
  expect(output).not.toContain("web_searchquery");
  expect(renderMarkdownHtml('Example: `<tool_call>web_search<arg_key>query</arg_key>`')).toContain("<code>");
});

describe("renderMarkdownHtml", () => {
  it("turns standalone inline artifact paths into scoped clickable links", () => {
    const html = renderMarkdownHtml("See `artifacts/report.md`", "chat1");
    expect(html).toContain('data-artifact="artifacts/report.md"');
    expect(html).toContain('/api/sessions/chat1/files/artifacts/report.md');
  });
  it("keeps ordinary inline code and fenced code from becoming artifact links", () => {
    expect(renderMarkdownHtml("Run `print(1)`", "chat1")).not.toContain('data-artifact=');
    expect(renderMarkdownHtml("```text\nartifacts/report.md\n```", "chat1")).not.toContain('class="artifact-path"');
  });
  it("renders block and inline LaTeX in assistant markdown", () => {
    const source = [
      "### Loss",
      "",
      "$$\\mathcal{L}_{\\text{Damage}} = \\lambda_{\\text{dice}} \\mathcal{L}_{\\text{GenDice}}$$",
      "",
      "Temperature $\\tau = 0.07$.",
    ].join("\n");
    const html = renderMarkdownHtml(source);
    expect(html).toContain("katex");
    expect(html).toContain("Damage");
    expect(html).toContain("0.07");
    expect(html).not.toContain("$$");
  });
});
