import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CanvasItem } from "../../lib/artifactMedia";
import {
  artifactDownloadUrl,
  artifactDisplayPath,
  fileExt,
  formatArtifactTime,
  formatBytes,
  kindLabel,
} from "../../lib/artifactMedia";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { initMermaidBlocks } from "../../lib/codeEnhancer";
import { renderMarkdownHtml } from "../../lib/markdownHtml";
import { useShallow } from "zustand/react/shallow";
import { useAppStore } from "../../store";
import { CodeBlock } from "../shared/CodeBlock";
import { PresentationPreview } from "../shared/PresentationPreview";
import { WordDocumentPreview } from "../shared/WordDocumentPreview";
import type { ArtifactFilter } from "./ThreadHeader";
import { matchesFilter, resolveArtifactSelection } from "./artifactFilter";
import { ArtifactThumb } from "./ArtifactThumb";
import { deliveredPaths } from "../../lib/deliverables";

function renderMd(text: string): string {
  return renderMarkdownHtml(text);
}

interface ArtifactPanelProps {
  filter: ArtifactFilter;
  onOpenLightbox: (path: string) => void;
  onCollapse?: () => void;
}

function storedListHeight(): number {
  try {
    const value = Number(localStorage.getItem("kageha.artifactListHeight"));
    return Number.isFinite(value) && value >= 100 ? value : 280;
  } catch {
    return 280;
  }
}

// ── File type icons (text-based, no emoji) ────────────────────────────

function FileIcon({ kind, ext }: { kind: CanvasItem["kind"]; ext: string }) {
  const colors: Record<string, string> = {
    image: "bg-pink-500/15 text-pink-400",
    video: "bg-purple-500/15 text-purple-400",
    audio: "bg-indigo-500/15 text-indigo-400",
    pdf: "bg-red-500/15 text-red-400",
    webpage: "bg-cyan-500/15 text-cyan-400",
    code: "bg-emerald-500/15 text-emerald-400",
    markdown: "bg-blue-500/15 text-blue-400",
    text: "bg-slate-500/15 text-slate-400",
    document: "bg-blue-500/15 text-blue-400",
    spreadsheet: "bg-green-500/15 text-green-400",
    presentation: "bg-orange-500/15 text-orange-400",
  };
  const color = colors[kind] || "bg-slate-500/15 text-slate-400";
  const label = ext ? ext.replace(".", "").toUpperCase().slice(0, 4) : kindLabel(kind).slice(0, 3).toUpperCase();

  return (
    <div
      className={cn(
        "flex h-9 w-9 shrink-0 items-center justify-center rounded-lg text-[0.6rem] font-bold tracking-wide",
        color,
      )}
    >
      {label}
    </div>
  );
}

// ── Inline Preview ────────────────────────────────────────────────────

function InlinePreview({
  item,
  onOpenFull,
}: {
  item: CanvasItem;
  onOpenFull: () => void;
}) {
  const sessionId = useAppStore((s) => s.sessionId);
  const runStatus = useAppStore((s) => s.runStatus);
  const [text, setText] = useState("");
  const [loading, setLoading] = useState(false);
  const markdownRef = useRef<HTMLDivElement | null>(null);

  // Fetch file content — and poll every 2s while agent is running
  useEffect(() => {
    if (item.kind !== "code" && item.kind !== "text" && item.kind !== "markdown" && item.kind !== "webpage") {
      setText("");
      return;
    }
    let cancelled = false;
    let inFlight = false;
    const fetchContent = () => {
      if (inFlight) return;
      inFlight = true;
      void fetch(item.url)
        .then((response) => {
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          return response.text();
        })
        .then((body) => {
          if (!cancelled) {
            setText(body);
            setLoading(false);
          }
        })
        .catch(() => {
          if (!cancelled) {
            setText("Could not load.");
            setLoading(false);
          }
        })
        .finally(() => {
          inFlight = false;
        });
    };
    setLoading(true);
    fetchContent();

    // Poll while agent is actively running
    let interval: ReturnType<typeof setInterval> | null = null;
    if (runStatus === "running") {
      interval = setInterval(fetchContent, 2000);
    }
    return () => {
      cancelled = true;
      if (interval) clearInterval(interval);
    };
  }, [item.url, item.kind, runStatus]);

  useEffect(() => {
    if (markdownRef.current) initMermaidBlocks(markdownRef.current);
  }, [text]);

  return (
    <div className="flex h-full min-h-0 flex-col overflow-hidden border-t border-line bg-[var(--color-canvas)]">
      {/* Preview header */}
      <div className="flex shrink-0 items-center justify-between px-3 py-2 border-b border-line/50">
        <div className="flex items-center gap-2 min-w-0">
          <FileIcon kind={item.kind} ext={fileExt(item.path)} />
          <div className="min-w-0">
            <div className="flex items-center gap-1.5">
              <p className="truncate text-xs font-medium text-ink">{item.caption}</p>
              {runStatus === "running" && (item.kind === "code" || item.kind === "text" || item.kind === "markdown" || item.kind === "webpage") && (
                <span className="flex items-center gap-1 rounded bg-accent/15 px-1.5 py-0.5 text-[0.55rem] font-medium text-accent">
                  <span className="h-1.5 w-1.5 rounded-full bg-accent animate-pulse" />
                  Live
                </span>
              )}
            </div>
            <p className="truncate text-[0.58rem] text-faint" title={item.path}>
              {artifactDisplayPath(item.path)}
            </p>
            <p className="text-[0.6rem] text-faint">
              {kindLabel(item.kind)}
              {item.size ? ` · ${formatBytes(item.size)}` : ""}
              {(item.createdAt || item.modifiedAt) ? ` · Generated ${formatArtifactTime(item.createdAt || item.modifiedAt)}` : ""}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-1.5">
          <button
            type="button"
            className="inline-flex items-center gap-1 rounded-md bg-accent/15 px-2.5 py-1 text-[0.65rem] font-medium text-accent hover:bg-accent/25 transition-colors"
            onClick={onOpenFull}
            title="Open fullscreen"
          >
            <span>⤢</span> Expand
          </button>
          {sessionId && (
            <a
              href={artifactDownloadUrl(sessionId, item.path) || item.url}
              download={item.caption}
              className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-[0.65rem] text-muted hover:bg-line/50 hover:text-ink transition-colors"
              title="Download"
            >
              ↓
            </a>
          )}
        </div>
      </div>

      {/* Content */}
      <div className="min-h-0 flex-1 overflow-hidden">
        {item.kind === "image" ? (
          <div className="ka-artifact-inline-image flex items-center justify-center bg-[#0d1117] p-2 sm:p-4">
            <img
              src={item.url}
              alt={item.caption}
              className="max-h-[20rem] max-w-full rounded object-contain sm:max-h-[20rem]"
              onClick={onOpenFull}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onOpenFull();
                }
              }}
              role="button"
              tabIndex={0}
              aria-label="Open fullscreen image preview"
            />
          </div>
        ) : item.kind === "video" ? (
          <div className="p-3">
            <video src={item.url} controls className="w-full rounded-lg max-h-[16rem]" />
          </div>
        ) : item.kind === "audio" ? (
          <div className="p-4">
            <audio src={item.url} controls className="w-full" />
          </div>
        ) : item.kind === "pdf" ? (
          <iframe
            src={item.url}
            title={item.caption}
            className="h-full min-h-[20rem] w-full border-0 bg-white"
          />
        ) : item.kind === "webpage" ? (
          <iframe
            src={item.url}
            title={item.caption}
            sandbox="allow-scripts"
            className="h-full w-full border-0"
            style={{ minHeight: "20rem" }}
          />
        ) : item.kind === "code" ? (
          loading ? (
            <div className="p-4 text-xs text-muted">Loading…</div>
          ) : (
            <div className="h-full min-h-0 p-2">
              <CodeBlock
                code={text}
                filename={item.caption}
                maxHeight="100%"
                className="h-full overscroll-contain"
              />
            </div>
          )
        ) : item.kind === "markdown" ? (
          loading ? (
            <div className="p-4 text-xs text-muted">Loading…</div>
          ) : (
            <div
              ref={markdownRef}
              className="markdown p-4 overflow-auto h-full"
              dangerouslySetInnerHTML={{ __html: renderMd(text) }}
            />
          )
        ) : item.kind === "text" ? (
          loading ? (
            <div className="p-4 text-xs text-muted">Loading…</div>
          ) : (
            <pre className="p-3 font-mono text-[0.7rem] leading-relaxed text-ink whitespace-pre-wrap overflow-auto h-full">
              {text}
            </pre>
          )
        ) : item.kind === "presentation" ? (
          <PresentationPreview item={item} compact />
        ) : item.kind === "document" && fileExt(item.path) === ".docx" ? (
          <WordDocumentPreview item={item} compact />
        ) : (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <FileIcon kind={item.kind} ext={fileExt(item.path)} />
            <p className="mt-2 text-xs text-muted">{kindLabel(item.kind)}</p>
            <button
              type="button"
              className="mt-2 rounded-md bg-accent/15 px-3 py-1.5 text-xs font-medium text-accent hover:bg-accent/25"
              onClick={onOpenFull}
            >
              Open
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// ── File list item ────────────────────────────────────────────────────

function FileListItem({
  item,
  active,
  updatedThisTurn,
  onSelect,
  indent = 0,
}: {
  item: CanvasItem;
  active: boolean;
  updatedThisTurn: boolean;
  onSelect: () => void;
  indent?: number;
}) {
  const ext = fileExt(item.path);
  const name = item.caption || item.path.split("/").pop() || "file";

  return (
    <button
      type="button"
      className={cn(
        "flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left transition-colors",
        active
          ? "bg-[var(--color-accent-soft)] ring-1 ring-accent/30"
          : updatedThisTurn
            ? "bg-accent/5 ring-1 ring-accent/20"
            : "hover:bg-line/40",
      )}
      style={{ paddingLeft: `${10 + indent * 16}px` }}
      onClick={onSelect}
    >
      <FileIcon kind={item.kind} ext={ext} />
      <div className="min-w-0 flex-1">
        <p className="truncate text-[0.8rem] font-medium text-ink">
          {name}
          {updatedThisTurn && !active && (
            <span className="ml-1.5 inline-flex h-1.5 w-1.5 rounded-full bg-accent" />
          )}
        </p>
        <p className="text-[0.6rem] text-faint">
          {kindLabel(item.kind)}
          {item.size ? ` · ${formatBytes(item.size)}` : ""}
          {(item.createdAt || item.modifiedAt) ? ` · ${formatArtifactTime(item.createdAt || item.modifiedAt)}` : ""}
        </p>
      </div>
    </button>
  );
}

interface ArtifactTreeNode {
  name: string;
  path: string;
  folder: boolean;
  children: ArtifactTreeNode[];
  item?: CanvasItem;
}

function buildArtifactTree(items: CanvasItem[]): ArtifactTreeNode {
  const root: ArtifactTreeNode = { name: "Artifacts", path: "", folder: true, children: [] };
  const folders = new Map<string, ArtifactTreeNode>();
  folders.set("", root);

  for (const item of items) {
    const parts = item.path.split("/").filter(Boolean);
    let parent = root;
    let currentPath = "";
    parts.forEach((part, index) => {
      currentPath = currentPath ? `${currentPath}/${part}` : part;
      const isFile = index === parts.length - 1;
      let node = parent.children.find((child) => child.name === part);
      if (!node) {
        node = {
          name: part,
          path: currentPath,
          folder: !isFile,
          children: [],
          item: isFile ? item : undefined,
        };
        parent.children.push(node);
      }
      if (!isFile) {
        node.folder = true;
        folders.set(currentPath, node);
        parent = node;
      }
    });
  }

  const sort = (node: ArtifactTreeNode) => {
    node.children.sort((a, b) => {
      if (a.folder !== b.folder) return a.folder ? -1 : 1;
      return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
    });
    node.children.forEach(sort);
  };
  sort(root);
  return root;
}

function countTreeFiles(node: ArtifactTreeNode): number {
  if (!node.folder) return 1;
  return node.children.reduce((total, child) => total + countTreeFiles(child), 0);
}

function ArtifactTree({
  node,
  depth,
  expanded,
  onToggle,
  onSelect,
  selectedPath,
  updatedPaths,
}: {
  node: ArtifactTreeNode;
  depth: number;
  expanded: Set<string>;
  onToggle: (path: string) => void;
  onSelect: (path: string) => void;
  selectedPath: string | null;
  updatedPaths: Set<string>;
}) {
  if (!node.folder && node.item) {
    return (
      <FileListItem
        item={node.item}
        active={node.item.path === selectedPath}
        updatedThisTurn={updatedPaths.has(node.item.path)}
        indent={Math.max(0, depth - 1)}
        onSelect={() => onSelect(node.item!.path)}
      />
    );
  }
  const open = depth === 0 || expanded.has(node.path);
  return (
    <div>
      {depth > 0 && (
        <button
          type="button"
          className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-left text-[0.7rem] font-medium text-muted hover:bg-line/40 hover:text-ink"
          style={{ paddingLeft: `${10 + (depth - 1) * 16}px` }}
          onClick={() => onToggle(node.path)}
          aria-expanded={open}
        >
          <span className="w-3 text-center text-[0.6rem]">{open ? "⌄" : "›"}</span>
          <span className="text-accent/80">▰</span>
          <span className="min-w-0 flex-1 truncate">{node.name}</span>
          <span className="text-[0.58rem] text-faint">{countTreeFiles(node)}</span>
        </button>
      )}
      {open && node.children.map((child) => (
        <ArtifactTree
          key={child.path}
          node={child}
          depth={depth + (depth === 0 ? 1 : 1)}
          expanded={expanded}
          onToggle={onToggle}
          onSelect={onSelect}
          selectedPath={selectedPath}
          updatedPaths={updatedPaths}
        />
      ))}
    </div>
  );
}

// ── Main Panel ────────────────────────────────────────────────────────

export function ArtifactPanel({ filter, onOpenLightbox, onCollapse }: ArtifactPanelProps) {
  const canvasItems = useAppStore((s) => s.canvasItems);
  const completedMessages = useAppStore(
    useShallow((s) => s.messages.filter((m) => m.role === "assistant" && !m.streaming)),
  );
  const [collection, setCollection] = useState<"working" | "delivered">("working");
  const canvasTurnPaths = useAppStore((s) => s.canvasTurnPaths);
  const canvasSelectedPath = useAppStore((s) => s.canvasSelectedPath);
  const previewRequest = useAppStore((s) => s.canvasPreviewRequest);
  const selectCanvasItem = useAppStore((s) => s.selectCanvasItem);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [view, setView] = useState<"list" | "grid" | "board">("board");
  const [boardFocus, setBoardFocus] = useState(false);
  useEffect(() => {
    if (previewRequest) setBoardFocus(true);
  }, [previewRequest]);
  const [expandedFolders, setExpandedFolders] = useState<Set<string>>(new Set());
  const splitRef = useRef<HTMLDivElement>(null);
  const [listHeight, setListHeight] = useState(storedListHeight);
  const [resizingSplit, setResizingSplit] = useState(false);

  useEffect(() => {
    if (!resizingSplit) return;
    const onMove = (event: PointerEvent) => {
      const bounds = splitRef.current?.getBoundingClientRect();
      if (!bounds) return;
      const max = Math.max(100, bounds.height - 180);
      setListHeight(Math.min(max, Math.max(100, event.clientY - bounds.top)));
    };
    const onUp = () => setResizingSplit(false);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [resizingSplit]);

  useEffect(() => {
    try {
      localStorage.setItem("kageha.artifactListHeight", String(Math.round(listHeight)));
    } catch {
      // Unavailable browser storage must not prevent files from opening.
    }
  }, [listHeight]);

  const delivered = useMemo(() => {
    const paths = canvasItems.filter(item => item.available !== false).map(item => item.path);
    return new Set(completedMessages.flatMap(message => deliveredPaths(message, paths)));
  }, [canvasItems, completedMessages]);
  const availableDeliveredCount = canvasItems.filter(item => item.available !== false && delivered.has(item.path)).length;
  const selectedCollection = canvasSelectedPath
    ? delivered.has(canvasSelectedPath) ? "delivered" : "working"
    : collection;
  const filtered = useMemo(() => canvasItems.filter(item =>
    delivered.has(item.path) === (selectedCollection === "delivered") &&
    (selectedCollection === "working" || item.available !== false) && matchesFilter(item, filter)),
  [canvasItems, delivered, selectedCollection, filter]);
  const collections = (
    <div className="flex shrink-0 gap-1 border-b border-line px-3 py-2" role="group" aria-label="File collection">
      {(["working", "delivered"] as const).map(id => <button key={id} type="button"
        aria-pressed={selectedCollection === id}
        className={cn("rounded-md px-2 py-1 text-xs", selectedCollection === id ? "bg-line text-ink" : "text-muted hover:text-ink")}
        onClick={() => {
          setCollection(id);
          if (selectedCollection !== id) {
            setBoardFocus(false);
            setSelectedPath(null);
            selectCanvasItem(null);
          }
        }}>
        {id === "working" ? "Working files" : `Deliverables (${availableDeliveredCount})`}
      </button>)}
    </div>
  );

  const artifactTree = useMemo(() => buildArtifactTree(filtered), [filtered]);

  useEffect(() => {
    const topLevelFolders = artifactTree.children
      .filter((node) => node.folder)
      .map((node) => node.path);
    setExpandedFolders((previous) => {
      const next = new Set([...previous, ...topLevelFolders]);
      return next.size === previous.size ? previous : next;
    });
  }, [artifactTree]);

  const selected = useMemo(
    () => filtered.find((i) => i.path === selectedPath) || null,
    [filtered, selectedPath],
  );

  useEffect(() => {
    const next = resolveArtifactSelection(filtered, selectedPath, canvasSelectedPath, canvasTurnPaths);
    if (next.path !== selectedPath) setSelectedPath(next.path);
    if (next.syncStore && next.path !== canvasSelectedPath) selectCanvasItem(next.path);
  }, [filtered, selectedPath, canvasSelectedPath, canvasTurnPaths, selectCanvasItem]);

  const handleSelect = useCallback((path: string) => {
    setSelectedPath((prev) => {
      const next = prev === path ? null : path;
      selectCanvasItem(next);
      return next;
    });
  }, [selectCanvasItem]);

  const toggleFolder = useCallback((path: string) => {
    setExpandedFolders((previous) => {
      const next = new Set(previous);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }, []);

  if (filtered.length === 0) {
    return (
      <div ref={splitRef} className="ka-artifact-panel flex min-h-0 flex-1 flex-col">
        {collections}
        {onCollapse && (
          <div className="flex h-9 shrink-0 items-center justify-between border-b border-line px-3">
            <span className="text-[0.65rem] font-semibold text-muted uppercase tracking-wide">
              Files
            </span>
            <button
              type="button"
              className="rounded-md px-2 py-1 text-xs text-muted hover:bg-line/70 hover:text-ink"
              onClick={onCollapse}
              title="Minimize artifact panel"
              aria-label="Minimize artifact panel"
            >
              <span aria-hidden="true">−</span>
            </button>
          </div>
        )}
        <div className="flex flex-1 flex-col items-center justify-center px-6 py-12 text-center">
          <div className="flex h-14 w-14 items-center justify-center rounded-2xl bg-line/30">
            <Icon.Projects size={26} className="text-accent" />
          </div>
          <p className="mt-3 text-sm font-medium text-muted">{selectedCollection === "delivered" ? "No deliverables yet" : "Your files will appear here"}</p>
          <p className="mt-1 max-w-[200px] text-xs text-faint leading-relaxed">
            {selectedCollection === "delivered" ? "Files included in the agent’s finished answers appear here." : "Working files appear here with live preview."}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div ref={splitRef} className="ka-artifact-panel flex min-h-0 flex-1 flex-col">
      {collections}
      {/* Header */}
      <div className="flex h-9 shrink-0 items-center justify-between border-b border-line px-3">
        <div className="flex items-center gap-2">
          <span className="text-[0.65rem] font-semibold text-muted uppercase tracking-wide">
            Files
          </span>
          <span className="rounded-full bg-accent/15 px-1.5 py-0.5 text-[0.55rem] font-medium text-accent">
            {filtered.length}
          </span>
        </div>
        <div className="flex items-center gap-1">
          <div className="flex rounded-md border border-line p-0.5" role="group" aria-label="Artifact view">
            {(["board", "list", "grid"] as const).map((mode) => (
              <button
                key={mode}
                type="button"
                className={cn(
                  "rounded px-1.5 py-1 text-[0.62rem] font-medium uppercase tracking-wide",
                  view === mode ? "bg-accent-soft text-accent" : "text-muted hover:text-ink",
                )}
                aria-pressed={view === mode}
                onClick={() => setView(mode)}
                title={`${mode[0].toUpperCase() + mode.slice(1)} view`}
              >
                {mode[0].toUpperCase() + mode.slice(1)}
              </button>
            ))}
          </div>
          {onCollapse && (
            <button
              type="button"
              className="rounded-md px-2 py-1 text-xs text-muted hover:bg-line/70 hover:text-ink"
              onClick={onCollapse}
              title="Minimize artifact panel"
              aria-label="Minimize artifact panel"
            >
              <span aria-hidden="true">−</span>
            </button>
          )}
        </div>
      </div>

      {view === "board" ? (
        <div className="ka-artifact-board">
          {boardFocus && selected ? (
            <div className="ka-board-preview">
              <button type="button" className="ka-board-back" onClick={() => setBoardFocus(false)}>
                <span aria-hidden="true">←</span> All files
              </button>
              <InlinePreview item={selected} onOpenFull={() => onOpenLightbox(selected.path)} />
            </div>
          ) : (
            <div className="ka-board-grid" aria-label="Artifact board">
              {filtered.map((item) => (
                <ArtifactThumb key={item.path} item={item} onClick={() => {
                  setSelectedPath(item.path);
                  selectCanvasItem(item.path);
                  setBoardFocus(true);
                }} />
              ))}
            </div>
          )}
        </div>
      ) : (
      /* Split: file list + preview */
      <div className="flex min-h-0 flex-1 flex-col">
        {/* File list */}
        <div
          className={cn(
            "overflow-y-auto overscroll-contain px-2 py-1.5 shrink-0",
            view === "list" ? "space-y-0.5" : "grid grid-cols-2 gap-2",
            !selected && "flex-1",
          )}
          style={selected ? { height: `${listHeight}px` } : undefined}
        >
          {view === "grid"
            ? filtered.map((item) => (
                <ArtifactThumb
                  key={item.path}
                  item={item}
                  onClick={() => handleSelect(item.path)}
                />
              ))
            : (
              <ArtifactTree
                node={artifactTree}
                depth={0}
                expanded={expandedFolders}
                onToggle={toggleFolder}
                onSelect={handleSelect}
                selectedPath={selectedPath}
                updatedPaths={canvasTurnPaths}
              />
            )}
        </div>

        {selected && (
          <div
            className="group relative z-20 h-2 shrink-0 cursor-row-resize border-y border-line bg-line/70 hover:bg-accent/60"
            role="separator"
            aria-label="Resize file list and artifact preview"
            aria-orientation="horizontal"
            onPointerDown={(event) => {
              event.preventDefault();
              event.currentTarget.setPointerCapture?.(event.pointerId);
              setResizingSplit(true);
            }}
          >
            <span className="absolute left-1/2 top-1/2 h-0.5 w-14 -translate-x-1/2 -translate-y-1/2 rounded bg-faint group-hover:bg-accent" />
          </div>
        )}

        {/* Inline preview */}
        {selected && (
          <div className="min-h-0 flex-1 overflow-auto overscroll-contain">
            <InlinePreview
              item={selected}
              onOpenFull={() => onOpenLightbox(selected.path)}
            />
          </div>
        )}
      </div>
      )}
    </div>
  );
}
