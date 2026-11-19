import { memo, useEffect, useMemo, useRef, useState } from "react";
import { initJsonCardHandlers } from "../../lib/jsonRenderer";
import { initCodeBlockHandlers, initMermaidBlocks } from "../../lib/codeEnhancer";
import { chatArtifactFromClick } from "../../lib/chatArtifactClick";
import {
  renderMarkdownCached,
  renderStreamingMarkdownHtml,
} from "../../lib/markdownHtml";
import { cn } from "../../lib/cn";

export const StreamingMarkdown = memo(function StreamingMarkdown({
  id,
  text,
  streaming,
  sessionId,
  isUser,
  className,
  onArtifactClick,
}: {
  id: string;
  text: string;
  streaming?: boolean;
  sessionId: string | null;
  isUser?: boolean;
  className?: string;
  onArtifactClick?: (path: string, expand: boolean) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [streamingHtml, setStreamingHtml] = useState(() =>
    streaming && text ? renderStreamingMarkdownHtml(text, sessionId) : "",
  );
  const html = useMemo(() => {
    if (streaming || !text) return "";
    return renderMarkdownCached(id, text, sessionId);
  }, [id, text, sessionId, streaming]);

  // Keep publishing during continuous token arrival; avoid redundant renders when text has not changed.
  const latest = useRef({ text, sessionId });
  latest.current = { text, sessionId };
  const lastPublished = useRef<string>("");
  const timer = useRef<number | null>(null);
  useEffect(() => {
    if (!streaming) return;
    const publish = () => {
      const currentText = latest.current.text;
      const currentSid = latest.current.sessionId;
      const signature = `${currentSid}:${currentText}`;
      if (signature === lastPublished.current) return;
      lastPublished.current = signature;
      setStreamingHtml(renderStreamingMarkdownHtml(currentText, currentSid));
    };
    publish();
    timer.current = window.setInterval(publish, 120);
    return () => {
      if (timer.current !== null) window.clearInterval(timer.current);
      timer.current = null;
    };
  }, [streaming, sessionId, id]);

  useEffect(() => {
    const el = containerRef.current;
    if (!el || streaming || !html) return;
    initJsonCardHandlers(el);
    initCodeBlockHandlers(el);
    initMermaidBlocks(el);
  }, [html, streaming]);

  if (streaming) {
    if (!text) {
      return (
        <div className={cn("text-sm text-muted", className)}>
          <span className="ka-stream-cursor" aria-hidden="true" />
        </div>
      );
    }
    return (
      <div className={cn("markdown ka-streaming-text", className)}>
        <div
          ref={containerRef}
          dangerouslySetInnerHTML={{ __html: streamingHtml }}
        />
        <span className="ka-stream-cursor" aria-hidden="true" />
      </div>
    );
  }

  if (!html) return null;

  return (
    <div
      ref={containerRef}
      className={cn(
        "markdown",
        isUser && "rounded-xl bg-surface px-3.5 py-2.5 shadow-[0_0_0_1px_var(--color-line)]",
        className,
      )}
      dangerouslySetInnerHTML={{ __html: html }}
      onClick={(e) => {
        const target = e.target as HTMLElement | null;
        if (!target) return;
        const pre = target.closest("pre") as HTMLPreElement | null;
        if (pre && target.tagName !== "A") {
          const rect = pre.getBoundingClientRect();
          const clickX = e.clientX - rect.left;
          const clickY = e.clientY - rect.top;
          if (clickX > rect.width - 60 && clickY < 32) {
            e.preventDefault();
            const code = pre.querySelector("code")?.textContent || pre.textContent || "";
            void navigator.clipboard.writeText(code).catch(() => {});
            pre.style.outline = "2px solid var(--color-accent)";
            window.setTimeout(() => {
              pre.style.outline = "";
            }, 600);
            return;
          }
        }
        if (!onArtifactClick) return;
        const hit = chatArtifactFromClick(target);
        if (!hit) return;
        e.preventDefault();
        onArtifactClick(hit.path, hit.expand);
      }}
    />
  );
});
