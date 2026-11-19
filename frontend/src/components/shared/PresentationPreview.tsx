import { useEffect, useState } from "react";
import { api } from "../../api/client";
import type { CanvasItem } from "../../lib/artifactMedia";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";

export interface PresentationSlide {
  index: number;
  title: string;
  text: string;
  notes: string;
  images: { index: number; contentType: string; url: string }[];
}

export interface PresentationDeck {
  path: string;
  slideCount: number;
  slides: PresentationSlide[];
}

interface PresentationPreviewProps {
  item: CanvasItem;
  compact?: boolean;
}

export function PresentationPreview({ item, compact = false }: PresentationPreviewProps) {
  const sessionId = useAppStore((s) => s.sessionId);
  const [deck, setDeck] = useState<PresentationDeck | null>(null);
  const [index, setIndex] = useState(0);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!sessionId) return;
    let cancelled = false;
    setError("");
    setIndex(0);
    void api<PresentationDeck>(
      `/api/sessions/${encodeURIComponent(sessionId)}/presentations?path=${encodeURIComponent(item.path)}`,
    )
      .then((next) => {
        if (!cancelled) setDeck(next);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : "Could not open slides");
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, item.path, item.modifiedAt]);

  const slide = deck?.slides[index];
  const total = deck?.slideCount || 0;
  const image = slide?.images[0];

  const go = (next: number) => {
    if (!total) return;
    setIndex(Math.max(0, Math.min(total - 1, next)));
  };

  if (error) {
    return (
      <div className="flex h-full min-h-[12rem] flex-col items-center justify-center gap-2 px-4 text-center">
        <p className="text-xs text-muted">Could not preview this deck</p>
        <p className="text-[0.65rem] text-faint">{error}</p>
        <a href={item.url} className="text-xs font-medium text-accent hover:underline">
          Download PPTX
        </a>
      </div>
    );
  }

  if (!deck || !slide) {
    return (
      <div className="flex h-full min-h-[12rem] items-center justify-center text-xs text-muted">
        Opening slides…
      </div>
    );
  }

  return (
    <div className={cn("flex h-full w-full min-h-0 flex-col bg-[#0d1117]", compact ? "min-h-[16rem]" : "min-h-[20rem]")}>
      <div className="relative min-h-0 flex-1">
        <div className="absolute inset-0 flex items-center justify-center p-3">
          <div className="relative aspect-video w-full max-h-full overflow-hidden rounded-lg border border-white/10 bg-[#161b22] shadow-lg">
            {image ? (
              <img src={image.url} alt={slide.title || `Slide ${index + 1}`} className="h-full w-full object-contain bg-black" />
            ) : (
              <div className="flex h-full flex-col justify-center gap-3 px-8 py-6 text-white">
                <p className="text-lg font-semibold leading-snug">{slide.title || `Slide ${index + 1}`}</p>
                {slide.text ? (
                  <p className="whitespace-pre-wrap text-sm leading-relaxed text-white/70">{slide.text}</p>
                ) : null}
              </div>
            )}
            {image && (slide.title || slide.text) ? (
              <div className="pointer-events-none absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/75 to-transparent px-3 py-2">
                <p className="truncate text-[0.7rem] font-medium text-white">{slide.title || `Slide ${index + 1}`}</p>
              </div>
            ) : null}
          </div>
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-2 border-t border-white/10 px-2 py-1.5">
        <button
          type="button"
          className="inline-flex h-7 w-7 items-center justify-center rounded-md text-white/70 hover:bg-white/10 hover:text-white disabled:opacity-30"
          onClick={() => go(index - 1)}
          disabled={index <= 0}
          aria-label="Previous slide"
        >
          <Icon.Back size={14} />
        </button>
        <span className="min-w-0 flex-1 truncate text-center text-[0.65rem] text-white/55">
          {index + 1} / {total}
          {slide.title ? ` · ${slide.title}` : ""}
        </span>
        <button
          type="button"
          className="inline-flex h-7 w-7 items-center justify-center rounded-md text-white/70 hover:bg-white/10 hover:text-white disabled:opacity-30"
          onClick={() => go(index + 1)}
          disabled={index >= total - 1}
          aria-label="Next slide"
        >
          <Icon.Forward size={14} />
        </button>
      </div>
    </div>
  );
}
