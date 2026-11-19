import { memo, useEffect, useMemo, useState } from "react";
import type { ComputerFrame, ToolCard } from "../api/types";
import { activityTarget, activityUrl } from "../lib/activityUi";
import {
  artifactDownloadUrl,
  artifactFileUrl,
  canvasKindForPath,
  fileBasename,
  isChatMediaArtifact,
  kindLabel,
  showcaseSortKey,
} from "../lib/artifactMedia";
import { cn } from "../lib/cn";
import { useAppStore } from "../store";

export const LiveToolPulse = memo(function LiveToolPulse({
  cards,
  streaming,
  onOpenBrowser,
  onOpenComputer,
}: {
  cards: ToolCard[];
  streaming: boolean;
  onOpenBrowser?: (url?: string) => void;
  onOpenComputer?: () => void;
}) {
  if (!streaming || !cards.length) return null;
  const running = cards.filter((c) => !c.status || c.status === "running" || c.status === "pending");
  const source = running.length ? running : cards.slice(-2);
  const names: string[] = [];
  for (const card of source) {
    const name = String(card.name || "").trim();
    if (!name || names.includes(name)) continue;
    names.push(name);
    if (names.length >= 3) break;
  }
  if (!names.length) return null;
  const activeCard = source[0];
  const target = activityTarget(activeCard?.name || "");
  const url = activityUrl(activeCard?.argsPreview, activeCard?.resultPreview);
  return (
    <button
      type="button"
      disabled={!target}
      className={cn(
        "mb-2 flex w-full items-center gap-2 rounded-sm font-mono text-sm text-faint",
        target && "cursor-pointer hover:bg-accent-soft hover:text-accent",
      )}
      aria-live="polite"
      onClick={() => {
        if (target === "browser") onOpenBrowser?.(url || undefined);
        if (target === "computer") onOpenComputer?.();
      }}
    >
      <span className="inline-block h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-accent" />
      <span className="min-w-0 truncate text-muted">{names.map((name) => name.replace(/^computer_/, "").replace(/_/g, " ")).join(" · ")}</span>
      {target === "computer" ? <span className="ml-auto shrink-0 text-[0.65rem]">Watch live ↗</span> : target ? <span className="ml-auto shrink-0 text-[0.65rem]">Open ↗</span> : null}
    </button>
  );
});

export const ComputerFrames = memo(function ComputerFrames({ frames }: { frames: ComputerFrame[] }) {
  const setPrefs = useAppStore((s) => s.setPrefs);
  const pinLiveSurface = useAppStore((s) => s.pinLiveSurface);
  const setComputerPanelMode = useAppStore((s) => s.setComputerPanelMode);
  const frame = [...frames].reverse().find((item) => item.app !== "Kageha Browser") || frames[frames.length - 1];
  if (!frames.length || !frame || frame.app === "Kageha Browser") return null;
  return (
    <button
      type="button"
      className="group mb-2 block w-64 overflow-hidden rounded-md border border-line bg-surface text-left hover:border-accent"
      title="Open live desktop view"
      onClick={() => {
        setPrefs({ rightPanelCollapsed: false });
        pinLiveSurface("computer");
        setComputerPanelMode("expanded");
      }}
    >
      <div className="relative">
        <img src={frame.url} alt={frame.caption || "Live computer preview"} className="h-32 w-full object-cover" />
        <span className="absolute bottom-1 right-1 rounded bg-black/75 px-1.5 py-0.5 text-[0.62rem] text-white/85">Open live view ↗</span>
      </div>
      <span className="flex items-center gap-1.5 truncate px-2 py-1.5 text-[0.7rem] text-muted">
        <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-500" />
        Live desktop · {frame.action || frame.app || "latest frame"}
      </span>
    </button>
  );
});

function ArtifactThumb({ path, url, kind }: { path: string; url?: string; kind: ReturnType<typeof canvasKindForPath> }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [url]);
  const name = fileBasename(path);
  if (kind === "image" && url && !failed) {
    return <img key={url} src={url} alt={name} className="h-full w-full object-cover transition-transform duration-200 group-hover:scale-[1.02]" onError={() => setFailed(true)} />;
  }
  if (kind === "audio" && url) {
    return (
      <div className="flex h-full w-full flex-col justify-between bg-gradient-to-b from-canvas to-line/40 px-3 py-2.5">
        <div className="flex items-center gap-2"><span className="flex h-8 w-8 items-center justify-center rounded-lg bg-surface text-xs font-semibold text-accent shadow-sm ring-1 ring-line">♪</span><span className="line-clamp-2 text-[0.72rem] font-medium leading-snug text-ink">{name}</span></div>
        <audio src={url} controls preload="metadata" className="w-full" onClick={(e) => e.stopPropagation()} />
      </div>
    );
  }
  const glyph = kind === "video" ? "▶" : kind === "pdf" ? "PDF" : kind === "presentation" ? "PPT" : kind === "markdown" ? "MD" : kind === "document" ? "DOC" : "◇";
  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-2 bg-gradient-to-b from-canvas to-line/40 px-3">
      <span className="flex h-9 w-9 items-center justify-center rounded-lg bg-surface text-[0.7rem] font-semibold tracking-wide text-accent shadow-sm ring-1 ring-line">{glyph}</span>
      <span className="line-clamp-2 text-center text-[0.72rem] font-medium leading-snug text-ink">{name}</span>
      <span className="text-[0.62rem] text-faint">{kindLabel(kind)}</span>
    </div>
  );
}

export const MessageArtifacts = memo(function MessageArtifacts({ paths }: { paths: string[] }) {
  const sessionId = useAppStore((s) => s.sessionId);
  const openCanvasItem = useAppStore((s) => s.openCanvasItem);
  const refs = useMemo(() => [...paths].filter(isChatMediaArtifact).sort((a, b) => {
    const [ra, pa] = showcaseSortKey(a); const [rb, pb] = showcaseSortKey(b);
    return ra - rb || pa.localeCompare(pb);
  }).slice(0, 8), [paths]);
  if (!refs.length) return null;
  return (
    <div className="mb-3 flex gap-2.5 overflow-x-auto pb-1">
      {refs.map((path) => {
        const kind = canvasKindForPath(path);
        const url = artifactFileUrl(sessionId, path);
        const dl = artifactDownloadUrl(sessionId, path);
        return (
          <div key={path} role="button" tabIndex={0} className="group relative h-32 w-44 shrink-0 cursor-pointer overflow-hidden rounded-xl border border-line bg-surface text-left shadow-[0_1px_2px_rgba(28,27,25,0.04)] transition hover:border-accent/35 hover:shadow-md" title={path} onClick={() => openCanvasItem(path, { expand: kind === "image" || kind === "video" })} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); openCanvasItem(path, { expand: kind === "image" || kind === "video" }); } }}>
            <ArtifactThumb path={path} url={url} kind={kind} />
            {kind === "image" ? <span className="pointer-events-none absolute inset-x-0 bottom-0 z-10 bg-gradient-to-t from-ink/55 to-transparent px-2.5 pb-2 pt-8 text-[0.68rem] font-medium text-white opacity-0 transition group-hover:opacity-100">{fileBasename(path)}</span> : null}
            {dl ? <a href={dl} download={fileBasename(path)} className="absolute right-1.5 top-1.5 z-10 rounded-md bg-surface/95 px-1.5 py-0.5 text-[0.65rem] font-medium text-ink opacity-0 shadow-sm ring-1 ring-line transition group-hover:opacity-100" title="Download" onClick={(e) => e.stopPropagation()}>Save</a> : null}
          </div>
        );
      })}
    </div>
  );
});
