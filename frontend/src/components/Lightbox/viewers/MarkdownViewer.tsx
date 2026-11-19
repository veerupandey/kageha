import { useEffect, useMemo, useRef, useState } from "react";
import { initMermaidBlocks } from "../../../lib/codeEnhancer";
import { renderMarkdownHtml } from "../../../lib/markdownHtml";
import { Icon } from "../../../lib/icons";
import { useCopy, useDocText } from "./useDocContent";

export function MarkdownViewer({ url, initialText = "", name: _name }: { url: string; initialText?: string; name: string }) {
  const { text, loading } = useDocText(url, initialText, "# Could not load document\nPlease check if the file still exists or download it directly.");
  const [mode, setMode] = useState<"rendered" | "raw">("rendered");
  const { copied, copy } = useCopy();
  const markdownRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (mode === "rendered" && markdownRef.current && text) initMermaidBlocks(markdownRef.current);
  }, [text, mode]);

  const stats = useMemo(() => {
    if (!text) return { words: 0, lines: 0, readTimeMinutes: 1 };
    const lines = text.split("\n").length;
    const words = text.trim() ? text.trim().split(/\s+/).length : 0;
    return { words, lines, readTimeMinutes: Math.max(1, Math.ceil(words / 200)) };
  }, [text]);

  if (loading) {
    return (
      <div className="flex h-full w-full items-center justify-center text-sm text-muted">
        <Icon.Loader className="mr-2 animate-spin text-accent" size={18} />
        Loading document…
      </div>
    );
  }

  return (
    <div className="flex h-full w-full min-h-0 flex-col bg-[var(--color-surface)]">
      <div className="flex shrink-0 items-center justify-between border-b border-line px-6 py-2.5 bg-[var(--color-canvas)] text-xs text-muted">
        <div className="flex items-center gap-3">
          <div className="inline-flex rounded-lg border border-line bg-[var(--color-surface)] p-0.5 shadow-xs">
            <button
              type="button"
              className={`rounded-md px-3 py-1 font-medium transition-colors ${mode === "rendered" ? "bg-accent/15 text-accent font-semibold" : "text-muted hover:text-ink"}`}
              onClick={() => setMode("rendered")}
            >
              Formatted
            </button>
            <button
              type="button"
              className={`rounded-md px-3 py-1 font-medium transition-colors ${mode === "raw" ? "bg-accent/15 text-accent font-semibold" : "text-muted hover:text-ink"}`}
              onClick={() => setMode("raw")}
            >
              Raw Source
            </button>
          </div>
          <span className="text-[0.72rem] text-faint">
            {stats.words.toLocaleString()} words · {stats.readTimeMinutes} min read · {stats.lines} lines
          </span>
        </div>

        <button
          type="button"
          onClick={() => copy(text)}
          className="inline-flex items-center gap-1.5 rounded-lg border border-line bg-[var(--color-surface)] px-2.5 py-1 text-xs font-medium text-ink transition-colors hover:bg-line/40"
          title="Copy markdown content"
        >
          {copied ? <Icon.Check size={14} className="text-emerald-500" /> : <Icon.Copy size={14} />}
          <span>{copied ? "Copied" : "Copy"}</span>
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-8 sm:px-8 lg:px-12">
        {mode === "rendered" ? (
          <div ref={markdownRef} className="markdown mx-auto max-w-4xl text-ink leading-relaxed" dangerouslySetInnerHTML={{ __html: renderMarkdownHtml(text) }} />
        ) : (
          <div className="mx-auto max-w-5xl rounded-xl border border-line bg-[#0d1117] p-4 text-[#c9d1d9] shadow-inner">
            <pre className="font-mono text-xs leading-relaxed whitespace-pre-wrap select-text">{text}</pre>
          </div>
        )}
      </div>
    </div>
  );
}
