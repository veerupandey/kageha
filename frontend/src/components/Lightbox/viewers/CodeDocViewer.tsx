import { useEffect, useMemo, useRef, useState } from "react";
import hljs from "highlight.js/lib/core";
import { Icon } from "../../../lib/icons";
import "../../shared/CodeBlock";
import { useCopy, useDocText } from "./useDocContent";

const EXT_TO_LANG: Record<string, string> = {
  ".py": "python", ".js": "javascript", ".mjs": "javascript", ".cjs": "javascript",
  ".ts": "typescript", ".tsx": "typescript", ".jsx": "javascript", ".sh": "bash",
  ".bash": "bash", ".zsh": "bash", ".json": "json", ".yml": "yaml", ".yaml": "yaml",
  ".sql": "sql", ".css": "css", ".html": "xml", ".xml": "xml", ".go": "go",
  ".rs": "rust", ".java": "java", ".c": "cpp", ".cpp": "cpp", ".rb": "ruby",
  ".swift": "swift", ".kt": "kotlin", ".php": "php", ".md": "markdown",
};

export function CodeDocViewer({ url, initialText = "", name, language }: { url: string; initialText?: string; name: string; language?: string }) {
  const { text, loading } = useDocText(url, initialText, "// Could not load source code.");
  const [wrapLines, setWrapLines] = useState(false);
  const { copied, copy } = useCopy();
  const codeRef = useRef<HTMLElement>(null);

  const ext = name.includes(".") ? "." + name.split(".").pop()!.toLowerCase() : "";
  const detectedLang = language || EXT_TO_LANG[ext] || "text";

  useEffect(() => {
    if (codeRef.current && text) {
      try {
        const result = detectedLang && hljs.getLanguage(detectedLang)
          ? hljs.highlight(text, { language: detectedLang })
          : hljs.highlightAuto(text);
        codeRef.current.innerHTML = result.value;
      } catch {
        codeRef.current.textContent = text;
      }
    }
  }, [text, detectedLang]);

  const lines = useMemo(() => (text ? text.split("\n") : []), [text]);

  if (loading) {
    return (
      <div className="flex h-full w-full items-center justify-center text-sm text-muted">
        <Icon.Loader className="mr-2 animate-spin text-accent" size={18} />
        Loading code…
      </div>
    );
  }

  return (
    <div className="flex h-full w-full min-h-0 flex-col bg-[#0d1117] text-[#c9d1d9]">
      <div className="flex shrink-0 items-center justify-between border-b border-[#30363d] bg-[#161b22] px-4 py-2 text-xs">
        <div className="flex items-center gap-3">
          <span className="rounded bg-accent/20 px-2 py-0.5 text-[0.68rem] font-semibold tracking-wide uppercase text-accent">
            {detectedLang}
          </span>
          <span className="text-[0.72rem] text-[#8b949e]">
            {lines.length.toLocaleString()} lines · {text.length.toLocaleString()} characters
          </span>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs transition-colors ${wrapLines ? "bg-accent/20 text-accent font-medium" : "text-[#8b949e] hover:bg-[#30363d] hover:text-[#c9d1d9]"}`}
            onClick={() => setWrapLines(!wrapLines)}
            title="Toggle word wrap"
          >
            <Icon.WrapText size={14} />
            <span>Wrap lines</span>
          </button>

          <button
            type="button"
            onClick={() => copy(text)}
            className="inline-flex items-center gap-1.5 rounded-md border border-[#30363d] bg-[#21262d] px-2.5 py-1 text-xs font-medium text-[#c9d1d9] transition-colors hover:bg-[#30363d]"
            title="Copy code"
          >
            {copied ? <Icon.Check size={14} className="text-emerald-400" /> : <Icon.Copy size={14} />}
            <span>{copied ? "Copied" : "Copy"}</span>
          </button>
        </div>
      </div>

      <div className="relative min-h-0 flex-1 overflow-auto font-mono text-[0.8rem] leading-relaxed">
        <div className="flex min-w-full">
          <div className="sticky left-0 z-10 flex shrink-0 select-none flex-col border-r border-[#30363d] bg-[#161b22] py-4 pr-3 pl-4 text-right text-[0.75rem] text-[#484f58]">
            {lines.map((_, i) => (
              <span key={i} className="leading-relaxed">{i + 1}</span>
            ))}
          </div>

          <pre className={`flex-1 p-4 overflow-visible font-mono ${wrapLines ? "whitespace-pre-wrap break-all" : "whitespace-pre"}`}>
            <code ref={codeRef} className="hljs text-[#c9d1d9]">{text}</code>
          </pre>
        </div>
      </div>
    </div>
  );
}
