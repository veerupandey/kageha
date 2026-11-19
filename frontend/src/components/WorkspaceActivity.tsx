/**
 * Task_Workspace views of the Activity panel (design section 16): the live
 * `todo.md`, the current Escalation_Ladder step, and the workspace file tree
 * with open and download actions. The tree and plan come from the read-only
 * `/api/sessions/<id>/workspace/{tree,plan,file}` routes.
 */
import { useEffect, useMemo, useState } from "react";
import { api } from "../api/client";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { LADDER_STEPS, type LadderStep } from "../lib/ladder";
import { renderMarkdownHtml } from "../lib/markdownHtml";
import { WORKSPACE_POLL_MS, workspaceBase as sessionBase, workspaceFileUrl } from "../lib/workspaceFiles";

export interface WorkspaceFileEntry {
  path: string;
  size: number;
  sha256: string;
  artifact_path: string;
}
interface TreeReply { exists?: boolean; files?: WorkspaceFileEntry[]; truncated?: boolean }
interface PlanReply { exists?: boolean; content?: string; sha256?: string | null }


/**
 * Fetches `url` now, whenever `refreshKey` changes, and every
 * {@link WORKSPACE_POLL_MS} while `running`. A failed read keeps the last value.
 */
function useWorkspaceRead<T>(url: string | null, refreshKey: string, running: boolean): T | null {
  const [state, setState] = useState<{ url: string | null; value: T | null }>({ url, value: null });
  useEffect(() => {
    if (!url) return;
    let cancelled = false;
    const read = () => {
      void api<T>(url)
        .then((value) => { if (!cancelled) setState({ url, value }); })
        .catch(() => {});
    };
    read();
    const timer = running ? window.setInterval(read, WORKSPACE_POLL_MS) : undefined;
    return () => { cancelled = true; if (timer !== undefined) window.clearInterval(timer); };
  }, [url, refreshKey, running]);
  // A value read for another chat is never shown.
  return state.url === url ? state.value : null;
}

/** The ladder step, hidden while the turn makes progress (`normal`). */
export function LadderChip({ step }: { step?: string }) {
  if (!step || step === "normal" || !Object.prototype.hasOwnProperty.call(LADDER_STEPS, step)) return null;
  const { label, tooltip } = LADDER_STEPS[step as LadderStep];
  return (
    <span
      className={cn("inline-flex items-center gap-1 rounded-full border px-2 py-1 text-[0.65rem] font-medium",
        step === "conclude" ? "border-danger/25 bg-danger-soft text-danger" : "border-warn/25 bg-warn-soft text-warn")}
      title={tooltip}
      aria-label={`Approach: ${label}. ${tooltip}`}
      data-ladder={step}
    >
      <Icon.Gauge size={11} aria-hidden="true" />
      {label}
    </span>
  );
}

/** `todo.md` rendered as markdown. Hidden when the workspace has no plan. */
export function WorkspacePlanCard({ sessionId, running, refreshKey }: { sessionId: string | null; running: boolean; refreshKey: string }) {
  const plan = useWorkspaceRead<PlanReply>(sessionId ? `${sessionBase(sessionId)}/plan` : null, refreshKey, running);
  const content = plan?.exists ? plan.content ?? "" : "";
  // Re-render the markdown only when the plan's text changes.
  const html = useMemo(() => (content.trim() ? renderMarkdownHtml(content) : ""), [content]);
  if (!html) return null;
  return (
    <section className="rounded-xl border border-line bg-canvas overflow-hidden" aria-label="Plan (todo.md)">
      <div className="flex items-center gap-2 px-3 py-2.5">
        <span className="text-[0.7rem] font-semibold text-ink flex-1">Plan</span>
        <span className="text-[0.55rem] text-faint font-mono">todo.md</span>
      </div>
      <div className="markdown px-3 py-3 text-[0.78rem] border-t border-line/40 overflow-auto max-h-[40vh]"
        dangerouslySetInnerHTML={{ __html: html }} />
    </section>
  );
}

interface TreeNode { name: string; path: string; dirs: Map<string, TreeNode>; files: WorkspaceFileEntry[] }

function buildTree(files: WorkspaceFileEntry[]): TreeNode {
  const root: TreeNode = { name: "", path: "", dirs: new Map(), files: [] };
  for (const file of files) {
    const parts = file.path.split("/");
    let node = root;
    for (const part of parts.slice(0, -1)) {
      const path = node.path ? `${node.path}/${part}` : part;
      let next = node.dirs.get(part);
      if (!next) { next = { name: part, path, dirs: new Map(), files: [] }; node.dirs.set(part, next); }
      node = next;
    }
    node.files.push(file);
  }
  return root;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

const actionClass = "flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-muted hover:bg-line/40 hover:text-ink transition-colors";

function TreeLevel({ node, sessionId, depth }: { node: TreeNode; sessionId: string; depth: number }) {
  const dirs = [...node.dirs.values()].sort((a, b) => a.name.localeCompare(b.name));
  const files = [...node.files].sort((a, b) => a.path.localeCompare(b.path));
  const indent = { paddingLeft: `${depth * 0.75}rem` };
  return (
    <ul className="space-y-px" aria-label={depth === 0 ? "Workspace files" : node.path}>
      {dirs.map((dir) => (
        <li key={dir.path}>
          <details open>
            <summary className="flex cursor-pointer items-center gap-1.5 rounded-md py-1 pr-1 text-[0.72rem] text-ink hover:bg-line/20" style={indent}>
              <Icon.Projects size={12} className="shrink-0 text-muted" aria-hidden="true" />
              <span className="truncate">{dir.name}</span>
            </summary>
            <TreeLevel node={dir} sessionId={sessionId} depth={depth + 1} />
          </details>
        </li>
      ))}
      {files.map((file) => {
        const name = file.path.split("/").pop() || file.path;
        return (
          <li key={file.path} className="flex items-center gap-1.5 rounded-md py-0.5 pr-1 text-[0.72rem] hover:bg-line/20" style={indent}>
            <Icon.File size={12} className="shrink-0 text-muted" aria-hidden="true" />
            <span className="min-w-0 flex-1 truncate text-ink" title={file.path}>{name}</span>
            <span className="shrink-0 tabular-nums text-[0.6rem] text-faint">{formatSize(file.size)}</span>
            <a className={actionClass} href={workspaceFileUrl(sessionId, file.path)} target="_blank" rel="noopener noreferrer"
              aria-label={`Open ${file.path}`} title="Open in a new tab">
              <Icon.External size={12} aria-hidden="true" />
            </a>
            <a className={actionClass} href={workspaceFileUrl(sessionId, file.path, true)} download={name}
              aria-label={`Download ${file.path}`} title="Download">
              <Icon.Export size={12} aria-hidden="true" />
            </a>
          </li>
        );
      })}
    </ul>
  );
}

/** The Task_Workspace file tree, from the workspace manifest. Hidden when it is empty. */
export function WorkspaceFiles({ sessionId, running, refreshKey }: { sessionId: string | null; running: boolean; refreshKey: string }) {
  const tree = useWorkspaceRead<TreeReply>(sessionId ? `${sessionBase(sessionId)}/tree` : null, refreshKey, running);
  const files = useMemo(() => tree?.files ?? [], [tree]);
  const root = useMemo(() => buildTree(files), [files]);
  if (!sessionId || !tree?.exists || files.length === 0) return null;
  return (
    <section className="rounded-xl border border-line bg-canvas overflow-hidden" aria-label="Task workspace files">
      <div className="flex items-center gap-2 px-3 py-2.5">
        <span className="text-[0.7rem] font-semibold text-ink flex-1">Files</span>
        <span className="text-[0.6rem] text-faint tabular-nums">{files.length}{tree.truncated ? "+" : ""}</span>
      </div>
      <div className="border-t border-line/40 px-2 py-2 max-h-[40vh] overflow-auto">
        <TreeLevel node={root} sessionId={sessionId} depth={0} />
        {tree.truncated && <p className="px-1 pt-1.5 text-[0.62rem] text-faint">Showing the first {files.length} files.</p>}
      </div>
    </section>
  );
}
