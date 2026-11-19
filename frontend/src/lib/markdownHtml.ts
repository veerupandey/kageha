import { marked } from "marked";
import DOMPurify from "dompurify";
import remend from "remend";
import { rewriteMarkdownMediaHtml } from "./markdownMedia";
import { enhanceJsonBlocks } from "./jsonRenderer";
import { enhanceCodeBlocks } from "./codeEnhancer";
import { injectMathHtml, protectMathInMarkdown } from "./markdownMath";

marked.setOptions({ gfm: true, breaks: true });

const MARKDOWN_SANITIZE = {
  ADD_ATTR: ["data-kageha-math"],
};

const MD_CACHE_MAX = 200;
const mdCache = new Map<string, string>();

const STREAMING_CACHE_MAX = 60;
const streamingCache = new Map<string, string>();

function cacheKey(id: string, text: string, sessionId: string): string {
  return JSON.stringify([sessionId, id, text]);
}

function streamingCacheKey(text: string, sessionId: string): string {
  return JSON.stringify([sessionId, text]);
}

export function renderMarkdownHtml(
  text: string,
  sessionId: string | null = null,
): string {
  const prose = (text || "").replace(/```[\s\S]*?```|`[^`\n]+`/g, "");
  if (/<tool_call>\s*[\w.-]+\s*<arg_key>/i.test(prose)) {
    return '<p role="alert">The model returned invalid tool-call text. This text is not evidence that any tools ran.</p>';
  }
  const hasMath = text ? (text.includes("$") || text.includes("\\[") || text.includes("\\(")) : false;
  const { text: withMathPlaceholders, mathBlocks } = hasMath
    ? protectMathInMarkdown(text)
    : { text: text || "", mathBlocks: [] };
  const raw = marked.parse(withMathPlaceholders, { async: false }) as string;
  let html = DOMPurify.sanitize(raw, MARKDOWN_SANITIZE);
  if (mathBlocks.length) {
    html = injectMathHtml(html, mathBlocks);
  }
  if (html.includes("<code>") || html.includes("data-artifact") || html.includes("href=") || html.includes("src=")) {
    html = rewriteMarkdownMediaHtml(html, sessionId);
  }
  if (html.includes("language-json")) {
    html = enhanceJsonBlocks(html);
  }
  if (html.includes("<pre>")) {
    html = enhanceCodeBlocks(html);
  }
  return html;
}

export function renderMarkdownCached(
  id: string,
  text: string,
  sessionId: string | null,
): string {
  const sid = sessionId || "";
  const key = cacheKey(id, text, sid);
  const hit = mdCache.get(key);
  if (hit != null) return hit;
  const html = renderMarkdownHtml(text, sessionId);
  mdCache.set(key, html);
  if (mdCache.size > MD_CACHE_MAX) {
    const first = mdCache.keys().next().value;
    if (first != null) mdCache.delete(first);
  }
  return html;
}

/**
 * Render a token stream without making incomplete Markdown look broken.
 *
 * remend closes temporary emphasis/code/link delimiters for the parser; the
 * durable message is still rendered from the original text when streaming
 * ends. Keep this separate from the normal cache because the live text is
 * intentionally short-lived and changes frequently.
 */
export function renderStreamingMarkdownHtml(
  text: string,
  sessionId: string | null = null,
): string {
  const sid = sessionId || "";
  const key = streamingCacheKey(text || "", sid);
  const hit = streamingCache.get(key);
  if (hit != null) return hit;

  const repaired = remend(text || "");
  // remend handles inline delimiters, while marked needs an explicit closing
  // fence before it will produce a code block for an in-flight code sample.
  const fences = [...repaired.matchAll(/^\s{0,3}(`{3,}|~{3,})/gm)].map(
    (match) => match[1],
  );
  let html: string;
  if (fences.length % 2 === 1) {
    const opener = fences[fences.length - 1];
    html = renderMarkdownHtml(`${repaired}\n${opener}`, sessionId);
  } else {
    html = renderMarkdownHtml(repaired, sessionId);
  }

  streamingCache.set(key, html);
  if (streamingCache.size > STREAMING_CACHE_MAX) {
    const first = streamingCache.keys().next().value;
    if (first != null) streamingCache.delete(first);
  }
  return html;
}
