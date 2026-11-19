import { useEffect, useRef, useState } from "react";
import DOMPurify from "dompurify";
import type { CanvasItem } from "../../lib/artifactMedia";

export function WordDocumentPreview({ item, compact = false }: { item: CanvasItem; compact?: boolean }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const controller = new AbortController();
    let cancelled = false;
    container.replaceChildren();
    setStatus("loading");

    void (async () => {
      try {
        const response = await fetch(item.url, { signal: controller.signal, cache: "no-store" });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const data = await response.arrayBuffer();
        const { renderAsync } = await import("docx-preview");
        if (cancelled) return;

        // Each request renders into a detached, isolated tree. A slow previous
        // document can never overwrite the file the user selected next.
        const host = document.createElement("div");
        const shadow = host.attachShadow({ mode: "open" });
        const styles = document.createElement("div");
        const body = document.createElement("div");
        shadow.append(styles, body);
        await renderAsync(data, body, styles, {
          className: "word-document",
          ignoreWidth: compact,
          useBase64URL: true,
          renderAltChunks: false,
        });
        if (cancelled) return;
        DOMPurify.sanitize(body, { IN_PLACE: true, FORBID_TAGS: ["iframe", "object", "embed"] });
        body.querySelectorAll("a").forEach((link) => {
          link.target = "_blank";
          link.rel = "noopener noreferrer";
        });
        const layout = document.createElement("style");
        layout.textContent = `
          :host { display: block; color: #111827; color-scheme: light; }
          .word-document-wrapper { padding: ${compact ? "12px" : "24px"}; background: #e5e7eb; }
          section.word-document { box-sizing: border-box; margin: 0 auto 16px; background: white; }
          .word-document table { color: inherit; }
          ${compact ? "section.word-document { width: 100%; padding: 24px !important; }" : ""}
          img { max-width: 100%; }
        `;
        shadow.append(layout);
        container.replaceChildren(host);
        setStatus("ready");
      } catch {
        if (!cancelled) setStatus("error");
      }
    })();
    return () => {
      cancelled = true;
      controller.abort();
      container.replaceChildren();
    };
  }, [item.url, item.path, item.modifiedAt, compact]);

  return (
    <div className="relative h-full min-h-[16rem] w-full overflow-auto bg-slate-200" aria-label={`Word document preview: ${item.caption || item.path}`} aria-busy={status === "loading"}>
      {status === "loading" && <p role="status" className="p-6 text-center text-sm text-slate-600">Opening document…</p>}
      {status === "error" && (
        <div role="alert" className="flex min-h-[16rem] flex-col items-center justify-center gap-3 p-6 text-center">
          <p className="text-sm text-slate-700">Could not preview this Word document.</p>
          <a href={item.url} download={item.caption} className="text-sm font-medium text-blue-700 hover:underline">Download DOCX</a>
        </div>
      )}
      <div ref={containerRef} />
    </div>
  );
}
