import { useEffect, useMemo, useRef, useState } from "react";
import hljs from "highlight.js/lib/core";
import jsonLang from "highlight.js/lib/languages/json";
import { Icon } from "../../../lib/icons";
import { useCopy, useDocText } from "./useDocContent";

if (!hljs.getLanguage("json")) hljs.registerLanguage("json", jsonLang);

export function JsonDocViewer({ url, initialText = "", name: _name }: { url: string; initialText?: string; name: string }) {
  const { text, loading } = useDocText(url, initialText, '{\n  "error": "Could not load JSON file"\n}');
  const [formatMode, setFormatMode] = useState<"pretty" | "raw">("pretty");
  const { copied, copy } = useCopy();
  const codeRef = useRef<HTMLElement>(null);

  const formattedText = useMemo(() => {
    if (!text || formatMode === "raw") return text;
    try {
      return JSON.stringify(JSON.parse(text), null, 2);
    } catch {
      return text;
    }
  }, [text, formatMode]);

  const lines = useMemo(() => formattedText.split("\n"), [formattedText]);

  useEffect(() => {
    if (codeRef.current && formattedText) {
      try {
        codeRef.current.innerHTML = hljs.highlight(formattedText, { language: "json" }).value;
      } catch {
        codeRef.current.textContent = formattedText;
      }
    }
  }, [formattedText]);

  if (loading) {
    return (
      <div className="flex h-full w-full items-center justify-center text-sm text-muted">
        <Icon.Loader className="mr-2 animate-spin text-accent" size={18} />
        Loading JSON…
      </div>
    );
  }

  return (
    <div className="flex h-full w-full min-h-0 flex-col bg-[#0d1117] text-[#c9d1d9]">
      <div className="flex shrink-0 items-center justify-between border-b border-[#30363d] bg-[#161b22] px-4 py-2 text-xs">
        <div className="flex items-center gap-3">
          <div className="inline-flex rounded-lg border border-[#30363d] bg-[#21262d] p-0.5">
            <button
              type="button"
              className={`rounded-md px-2.5 py-0.5 font-medium transition-colors ${formatMode === "pretty" ? "bg-accent/20 text-accent font-semibold" : "text-[#8b949e] hover:text-[#c9d1d9]"}`}
              onClick={() => setFormatMode("pretty")}
            >
              Formatted
            </button>
            <button
              type="button"
              className={`rounded-md px-2.5 py-0.5 font-medium transition-colors ${formatMode === "raw" ? "bg-accent/20 text-accent font-semibold" : "text-[#8b949e] hover:text-[#c9d1d9]"}`}
              onClick={() => setFormatMode("raw")}
            >
              Raw
            </button>
          </div>
          <span className="text-[0.72rem] text-[#8b949e]">
            {lines.length.toLocaleString()} lines · {formattedText.length.toLocaleString()} characters
          </span>
        </div>

        <button
          type="button"
          onClick={() => copy(formattedText)}
          className="inline-flex items-center gap-1.5 rounded-md border border-[#30363d] bg-[#21262d] px-2.5 py-1 text-xs font-medium text-[#c9d1d9] transition-colors hover:bg-[#30363d]"
          title="Copy JSON"
        >
          {copied ? <Icon.Check size={14} className="text-emerald-400" /> : <Icon.Copy size={14} />}
          <span>{copied ? "Copied" : "Copy"}</span>
        </button>
      </div>

      <div className="relative min-h-0 flex-1 overflow-auto font-mono text-[0.8rem] leading-relaxed">
        <div className="flex min-w-full">
          <div className="sticky left-0 z-10 flex shrink-0 select-none flex-col border-r border-[#30363d] bg-[#161b22] py-4 pr-3 pl-4 text-right text-[0.75rem] text-[#484f58]">
            {lines.map((_, i) => (
              <span key={i} className="leading-relaxed">{i + 1}</span>
            ))}
          </div>

          <pre className="flex-1 p-4 overflow-visible font-mono whitespace-pre">
            <code ref={codeRef} className="hljs text-[#c9d1d9]">{formattedText}</code>
          </pre>
        </div>
      </div>
    </div>
  );
}
