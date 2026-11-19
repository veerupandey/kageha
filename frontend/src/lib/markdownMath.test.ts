import { describe, expect, it } from "vitest";
import { injectMathHtml, protectMathInMarkdown, renderMathToHtml } from "./markdownMath";

describe("markdownMath", () => {
  it("protects display and inline math outside code fences", () => {
    const source = [
      "Inline $\\mathcal{L}_{\\text{SupCon}}$ and block:",
      "",
      "$$\\mathcal{L}_{\\text{Damage}} = \\lambda_{\\text{dice}} \\mathcal{L}_{\\text{GenDice}}$$",
    ].join("\n");
    const { text, mathBlocks } = protectMathInMarkdown(source);
    expect(mathBlocks).toHaveLength(2);
    expect(text).toContain('data-kageha-math="0"');
    expect(text).toContain('data-kageha-math="1"');
    expect(text).not.toContain("\\mathcal{L}_{\\text{Damage}}");
  });

  it("does not treat dollar amounts as math delimiters", () => {
    const source =
      "Evolution IQ Senior AI/ML Engineer ($200K+, fully remote), Workiva Sr. ML Engineering Manager ($193K-$308K)";
    const { text, mathBlocks } = protectMathInMarkdown(source);
    expect(mathBlocks).toHaveLength(0);
    expect(text).toBe(source);
  });

  it("leaves math inside fenced code untouched", () => {
    const source = "```\n$\\alpha$\n$$\\beta$$\n```";
    const { text, mathBlocks } = protectMathInMarkdown(source);
    expect(mathBlocks).toHaveLength(0);
    expect(text).toBe(source);
  });

  it("renders subscripts and operators with KaTeX", () => {
    const html = renderMathToHtml("\\mathcal{L}_{\\text{GenDice}} = 1 - 2", true);
    expect(html).toContain("katex");
    expect(html).toContain("GenDice");
  });

  it("injects rendered math into sanitized placeholders", () => {
    const { text, mathBlocks } = protectMathInMarkdown("$$x^2$$");
    const html = injectMathHtml(
      `<div data-kageha-math="0" class="katex-display"></div>`,
      mathBlocks,
    );
    expect(html).toContain("katex");
    expect(html).not.toContain("data-kageha-math");
    expect(text).toContain("data-kageha-math");
  });
});
