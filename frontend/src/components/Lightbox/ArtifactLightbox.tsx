import { useCallback, useEffect, useMemo, useRef } from "react";
import type { CanvasItem } from "../../lib/artifactMedia";
import {
  artifactDownloadUrl,
  artifactDisplayPath,
  fileExt,
  formatBytes,
  kindLabel,
} from "../../lib/artifactMedia";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";
import { LightboxPreview } from "./LightboxPreview";

interface ArtifactLightboxProps {
  itemPath: string | null;
  onClose: () => void;
  onNavigate: (direction: "prev" | "next") => void;
}

const FOCUSABLE_SELECTOR =
  'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

export function ArtifactLightbox({ itemPath, onClose, onNavigate }: ArtifactLightboxProps) {
  const canvasItems = useAppStore((s) => s.canvasItems);
  const sessionId = useAppStore((s) => s.sessionId);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const previouslyFocused = useRef<HTMLElement | null>(null);
  const touchStartRef = useRef<{ x: number; y: number } | null>(null);

  const item: CanvasItem | undefined = useMemo(
    () => canvasItems.find((i) => i.path === itemPath),
    [canvasItems, itemPath],
  );
  const itemIndex = useMemo(
    () => canvasItems.findIndex((i) => i.path === itemPath),
    [canvasItems, itemPath],
  );
  const isMedia = item?.kind === "image" || item?.kind === "video";
  const hasMultiple = canvasItems.length > 1;
  const name = item?.caption || item?.path.split("/").pop() || "artifact";

  const handleDownload = useCallback(() => {
    if (!item?.url) return;
    const url = (sessionId && artifactDownloadUrl(sessionId, item.path)) || item.url;
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
  }, [item, name, sessionId]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      const editing = e.target instanceof Element && e.target.closest(
        'input, textarea, select, [contenteditable]:not([contenteditable="false"]), audio, video, [role="slider"], [role="textbox"]',
      );
      if (hasMultiple && !editing && !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey
          && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        e.preventDefault();
        onNavigate(e.key === "ArrowLeft" ? "prev" : "next");
        return;
      }
      if (e.key !== "Tab") return;
      // Focus trap: keep Tab/Shift+Tab cycling within the lightbox.
      const container = containerRef.current;
      if (!container) return;
      const focusable = Array.from(
        container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
      ).filter((el) => !el.hasAttribute("disabled"));
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (e.shiftKey) {
        if (active === first || !container.contains(active)) {
          e.preventDefault();
          last.focus();
        }
      } else if (active === last || !container.contains(active)) {
        e.preventDefault();
        first.focus();
      }
    },
    [onClose, onNavigate, hasMultiple],
  );

  useEffect(() => {
    if (!itemPath) return;
    previouslyFocused.current = document.activeElement as HTMLElement | null;
    document.body.classList.add("ka-lightbox-open");
    window.addEventListener("keydown", handleKeyDown);
    // Move focus into the dialog once it mounts.
    const container = containerRef.current;
    const first = container?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
    first?.focus();
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      document.body.classList.remove("ka-lightbox-open");
      previouslyFocused.current?.focus();
    };
  }, [itemPath, handleKeyDown]);

  if (!itemPath || !item) return null;

  const SWIPE_THRESHOLD_PX = 50;

  const handleTouchStart = (e: React.TouchEvent) => {
    const t = e.touches[0];
    if (t) touchStartRef.current = { x: t.clientX, y: t.clientY };
  };

  const handleTouchEnd = (e: React.TouchEvent) => {
    const start = touchStartRef.current;
    touchStartRef.current = null;
    if (!start) return;
    const t = e.changedTouches[0];
    if (!t) return;
    const dx = t.clientX - start.x;
    const dy = t.clientY - start.y;
    // Horizontal swipes only — ignore vertical scroll gestures.
    if (Math.abs(dx) < SWIPE_THRESHOLD_PX || Math.abs(dx) <= Math.abs(dy)) return;
    onNavigate(dx < 0 ? "next" : "prev");
  };

  const badgeColors: Record<string, string> = {
    image: "bg-pink-500/15 text-pink-400 border-pink-500/20",
    video: "bg-purple-500/15 text-purple-400 border-purple-500/20",
    audio: "bg-indigo-500/15 text-indigo-400 border-indigo-500/20",
    pdf: "bg-red-500/15 text-red-400 border-red-500/20",
    webpage: "bg-cyan-500/15 text-cyan-400 border-cyan-500/20",
    markdown: "bg-blue-500/15 text-blue-400 border-blue-500/20",
    code: "bg-emerald-500/15 text-emerald-400 border-emerald-500/20",
    text: "bg-slate-500/15 text-slate-400 border-slate-500/20",
    document: "bg-sky-500/15 text-sky-400 border-sky-500/20",
    presentation: "bg-orange-500/15 text-orange-400 border-orange-500/20",
    spreadsheet: "bg-green-500/15 text-green-400 border-green-500/20",
  };
  const badgeColor = badgeColors[item.kind] || "bg-slate-500/15 text-slate-400 border-slate-500/20";
  const badgeLabel =
    fileExt(item.path).replace(".", "").toUpperCase().slice(0, 4) ||
    kindLabel(item.kind).toUpperCase().slice(0, 3);

  return (
    <div
      className="ka-lightbox-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="Artifact preview"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="ka-lightbox-frame" ref={containerRef}>
        {/* Navigation arrows sit in the gutter, outside the canvas on desktop */}
        {hasMultiple ? (
          <>
            <button
              type="button"
              className="ka-lightbox-nav absolute -left-14 top-1/2 z-10 inline-flex h-10 w-10 -translate-y-1/2 items-center justify-center rounded-full border border-line bg-[var(--color-surface)]/90 text-muted shadow-lg backdrop-blur-sm hover:bg-[var(--color-surface)] hover:text-ink transition-colors"
              aria-label="Previous artifact"
              onClick={() => onNavigate("prev")}
            >
              <Icon.ChevronRight size={18} className="rotate-180" />
            </button>
            <button
              type="button"
              className="ka-lightbox-nav absolute -right-14 top-1/2 z-10 inline-flex h-10 w-10 -translate-y-1/2 items-center justify-center rounded-full border border-line bg-[var(--color-surface)]/90 text-muted shadow-lg backdrop-blur-sm hover:bg-[var(--color-surface)] hover:text-ink transition-colors"
              aria-label="Next artifact"
              onClick={() => onNavigate("next")}
            >
              <Icon.ChevronRight size={18} />
            </button>
          </>
        ) : null}

        <div className={`ka-lightbox${isMedia ? " ka-lightbox--media" : ""}`}>
          {/* Top toolbar: file info + breadcrumb + actions */}
          <div className="ka-lightbox-toolbar">
            <div className="flex items-center gap-3 min-w-0">
              <span
                className={`flex h-6 px-2 items-center justify-center rounded-md text-[0.62rem] font-bold tracking-wider uppercase border shrink-0 ${badgeColor}`}
              >
                {badgeLabel}
              </span>
              <div className="flex flex-col min-w-0">
                <span className="ka-lightbox-title leading-tight" title={name}>
                  {name}
                </span>
                <span
                  className="text-[0.65rem] text-muted truncate leading-none mt-0.5"
                  title={item.path}
                >
                  {artifactDisplayPath(item.path)}
                  {item.size ? ` · ${formatBytes(item.size)}` : ""}
                </span>
              </div>
            </div>

            <div className="ka-lightbox-toolbar-actions">
              {item.url ? (
                <a
                  href={item.url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="ka-lightbox-tool-btn"
                  title="Open file in new tab"
                >
                  <Icon.External size={14} />
                  <span className="hidden sm:inline">Open</span>
                </a>
              ) : null}

              <button
                type="button"
                className="ka-lightbox-tool-btn"
                aria-label="Download"
                onClick={handleDownload}
                title="Download file"
              >
                <Icon.Export size={16} />
                <span>Download</span>
              </button>

              <button
                type="button"
                className="ka-lightbox-close ka-lightbox-tool-btn ka-lightbox-tool-btn--icon"
                aria-label="Close"
                onClick={onClose}
                title="Close (Esc)"
              >
                <Icon.Close size={18} />
              </button>
            </div>
          </div>

          <LightboxPreview
            item={item}
            onTouchStart={handleTouchStart}
            onTouchEnd={handleTouchEnd}
          >
            {hasMultiple ? (
              <>
                <button
                  type="button"
                  className="ka-lightbox-mobile-nav ka-lightbox-mobile-nav--prev"
                  aria-label="Previous artifact"
                  onClick={() => onNavigate("prev")}
                >
                  <Icon.ChevronRight size={18} className="rotate-180" />
                </button>
                <button
                  type="button"
                  className="ka-lightbox-mobile-nav ka-lightbox-mobile-nav--next"
                  aria-label="Next artifact"
                  onClick={() => onNavigate("next")}
                >
                  <Icon.ChevronRight size={18} />
                </button>
                <span className="ka-lightbox-mobile-counter" aria-live="polite">
                  {itemIndex + 1} / {canvasItems.length}
                </span>
              </>
            ) : null}
          </LightboxPreview>
        </div>
      </div>
    </div>
  );
}
