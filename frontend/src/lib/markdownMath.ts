import katex from "katex";

export interface MathBlock {
  latex: string;
  display: boolean;
}

const CODE_FENCE_RE = /(```[\s\S]*?```|`[^`\n]+`)/g;
const DISPLAY_DOLLAR_RE = /\$\$([\s\S]+?)\$\$/g;
const DISPLAY_BRACKET_RE = /\\\[([\s\S]+?)\\\]/g;
// Mirrors Pandoc's tex_math_dollars heuristic so currency ranges like
// "$200K, ... ($193K-$308K)" aren't mistaken for LaTeX: the opener can't be
// followed by whitespace/another "$", the closer can't be preceded by
// whitespace, and the closer can't be immediately followed by a digit
// (which is what a second price in the same sentence would do).
const INLINE_DOLLAR_RE = /(?<![\\$])\$(?!\$|\s)([^\$\n]+?)(?<!\s)\$(?!\$)(?!\d)/g;
const INLINE_PAREN_RE = /\\\(([\s\S]+?)\\\)/g;

function createPlaceholder(blocks: MathBlock[], latex: string, display: boolean): string {
  const idx = blocks.length;
  blocks.push({ latex, display });
  if (display) {
    return `\n\n<div data-kageha-math="${idx}" class="katex-display"></div>\n\n`;
  }
  return `<span data-kageha-math="${idx}"></span>`;
}

function protectMathInSegment(text: string, blocks: MathBlock[]): string {
  let out = text;
  out = out.replace(DISPLAY_DOLLAR_RE, (_, latex: string) => createPlaceholder(blocks, latex, true));
  out = out.replace(DISPLAY_BRACKET_RE, (_, latex: string) => createPlaceholder(blocks, latex, true));
  out = out.replace(INLINE_DOLLAR_RE, (_, latex: string) => createPlaceholder(blocks, latex, false));
  out = out.replace(INLINE_PAREN_RE, (_, latex: string) => createPlaceholder(blocks, latex, false));
  return out;
}

/** Replace math delimiters with HTML placeholders before markdown parsing. */
export function protectMathInMarkdown(text: string): { text: string; mathBlocks: MathBlock[] } {
  const mathBlocks: MathBlock[] = [];
  if (!text) return { text: "", mathBlocks };
  if (!text.includes("$") && !text.includes("\\[") && !text.includes("\\(")) {
    return { text, mathBlocks };
  }

  const parts: string[] = [];
  let last = 0;
  for (const match of text.matchAll(CODE_FENCE_RE)) {
    const index = match.index ?? 0;
    if (index > last) {
      parts.push(protectMathInSegment(text.slice(last, index), mathBlocks));
    }
    parts.push(match[0]);
    last = index + match[0].length;
  }
  if (last < text.length) {
    parts.push(protectMathInSegment(text.slice(last), mathBlocks));
  }

  return { text: parts.join(""), mathBlocks };
}

export function renderMathToHtml(latex: string, display: boolean): string {
  try {
    return katex.renderToString(latex.trim(), {
      displayMode: display,
      throwOnError: false,
      trust: false,
      strict: "ignore",
    });
  } catch {
    const escaped = latex
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
    return `<code>${escaped}</code>`;
  }
}

const PLACEHOLDER_RE =
  /<(?:div|span)\s+[^>]*data-kageha-math="(\d+)"[^>]*>\s*<\/(?:div|span)>/g;

/** Swap sanitized placeholders for KaTeX HTML. */
export function injectMathHtml(html: string, mathBlocks: MathBlock[]): string {
  if (!mathBlocks.length || !html) return html;
  return html.replace(PLACEHOLDER_RE, (_, idx: string) => {
    const block = mathBlocks[Number(idx)];
    if (!block) return _;
    const rendered = renderMathToHtml(block.latex, block.display);
    return block.display ? `<div class="katex-display">${rendered}</div>` : rendered;
  });
}
