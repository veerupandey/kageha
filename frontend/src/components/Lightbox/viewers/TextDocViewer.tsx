import { useMemo, useState } from "react";
import { Icon } from "../../../lib/icons";
import { useCopy, useDocText } from "./useDocContent";

export function TextDocViewer({ url, initialText = "", name: _name }: { url: string; initialText?: string; name: string }) {
  const { text, loading } = useDocText(url, initialText, "Could not load text file.");
  const [wrapLines, setWrapLines] = useState(true);
  const [query, setQuery] = useState("");
  const { copied, copy } = useCopy();

  const allLines = useMemo(() => text.split("\n"), [text]);

  const filteredLines = useMemo(() => {
    if (!query.trim()) return allLines.map((line, i) => ({ line, originalIndex: i + 1 }));
    const q = query.toLowerCase();
    return allLines
      .map((line, i) => ({ line, originalIndex: i + 1 }))
      .filter(({ line }) => line.toLowerCase().includes(q));
  }, [allLines, query]);

  if (loading) {
    return (
      <div className="flex h-full w-full items-center justify-center text-sm text-muted">
        <Icon.Loader className="mr-2 animate-spin text-accent" size={18} />
        Loading text…
      </div>
    );
  }

  return (
    <div className="flex h-full w-full min-h-0 flex-col bg-[var(--color-surface)] text-ink">
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3 border-b border-line bg-[var(--color-canvas)] px-4 py-2 text-xs">
        <span className="text-[0.72rem] text-muted">
          {allLines.length.toLocaleString()} lines · {text.length.toLocaleString()} characters{query.trim() ? ` · (${filteredLines.length} matching)` : ""}
        </span>

        <div className="flex items-center gap-2">
          <div className="relative">
            <Icon.Search size={13} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-muted" />
            <input
              type="text"
              placeholder="Find in file…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="h-7 w-40 rounded-lg border border-line bg-[var(--color-surface)] pl-7 pr-3 text-xs text-ink placeholder:text-faint focus:border-accent focus:outline-none focus:ring-1 focus:ring-accent"
            />
          </div>

          <button
            type="button"
            className={`inline-flex items-center gap-1.5 rounded-lg border border-line px-2.5 py-1 text-xs transition-colors ${wrapLines ? "bg-accent/15 text-accent font-semibold" : "bg-[var(--color-surface)] text-muted hover:text-ink"}`}
            onClick={() => setWrapLines(!wrapLines)}
            title="Toggle word wrap"
          >
            <Icon.WrapText size={13} />
            <span>Wrap</span>
          </button>

          <button
            type="button"
            onClick={() => copy(text)}
            className="inline-flex items-center gap-1.5 rounded-lg border border-line bg-[var(--color-surface)] px-2.5 py-1 text-xs font-medium text-ink transition-colors hover:bg-line/40"
            title="Copy text"
          >
            {copied ? <Icon.Check size={14} className="text-emerald-500" /> : <Icon.Copy size={14} />}
            <span>{copied ? "Copied" : "Copy"}</span>
          </button>
        </div>
      </div>

      <div className="relative min-h-0 flex-1 overflow-auto font-mono text-[0.8rem] leading-relaxed">
        <div className="flex min-w-full">
          <div className="sticky left-0 z-10 flex shrink-0 select-none flex-col border-r border-line bg-[var(--color-canvas)] py-4 pr-3 pl-4 text-right text-[0.72rem] text-faint">
            {filteredLines.map(({ originalIndex }) => (
              <span key={originalIndex} className="leading-relaxed">{originalIndex}</span>
            ))}
          </div>

          <pre className={`flex-1 p-4 font-mono text-ink ${wrapLines ? "whitespace-pre-wrap break-all" : "whitespace-pre overflow-x-auto"}`}>
            {filteredLines.map(({ line }, idx) => (
              <div key={idx} className="leading-relaxed">{line || " "}</div>
            ))}
          </pre>
        </div>
      </div>
    </div>
  );
}
