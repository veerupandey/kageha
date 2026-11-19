import { useEffect, useMemo, useRef, useState } from "react";
import type { DurableJob } from "./DurableJobs";
import { agentElapsed, agentOutline, elapsedLabel } from "../lib/agentTree";
import {
  WORKER_TERMINAL,
  workerActivity,
  workerName,
  workerStatus,
} from "../lib/workerPresentation";
import { cn } from "../lib/cn";

export function AgentTree({
  jobs,
  selectedId,
  onSelect,
}: {
  jobs: DurableJob[];
  selectedId?: string;
  onSelect: (job: DurableJob) => void;
}) {
  const outline = useMemo(() => agentOutline(jobs), [jobs]);
  const byId = useMemo(
    () => new Map(outline.map((node) => [node.job.id, node])),
    [outline],
  );
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());
  const [now, setNow] = useState(() => Date.now() / 1000);
  const nodes = useRef(new Map<string, HTMLButtonElement>());
  const active = jobs.some((job) => !WORKER_TERMINAL.has(job.state));
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(timer);
  }, [active]);
  const selectedPath: string[] = [];
  let ancestor = selectedId ? byId.get(selectedId) : undefined;
  while (ancestor?.parentId) {
    selectedPath.push(ancestor.parentId);
    ancestor = byId.get(ancestor.parentId);
  }
  const pathKey = JSON.stringify(selectedPath);
  useEffect(() => {
    const path = new Set<string>(JSON.parse(pathKey));
    setCollapsed((old) => new Set([...old].filter((id) => !path.has(id))));
  }, [selectedId, pathKey]);
  const visible = outline.filter((node) => {
    let parent = node.parentId;
    while (parent) {
      if (collapsed.has(parent)) return false;
      parent = byId.get(parent)?.parentId || null;
    }
    return true;
  });
  const [focusedId, setFocusedId] = useState(selectedId);
  useEffect(() => setFocusedId(selectedId), [selectedId]);
  const focusableId = visible.some((node) => node.job.id === focusedId)
    ? focusedId
    : visible[0]?.job.id;
  const toggle = (id: string, close: boolean) =>
    setCollapsed((old) => {
      const next = new Set(old);
      if (close) next.add(id);
      else next.delete(id);
      return next;
    });
  return (
    <nav
      aria-label="Agent tree"
      className="ka-agent-tree border-b border-line/60 px-3 py-2"
    >
      <div className="mb-1.5 flex items-center justify-between px-1 text-xs text-muted">
        <span>Agents</span>
        <span>{jobs.length} in this task</span>
      </div>
      <ul
        role="tree"
        aria-label="Task agents"
        className="max-h-44 space-y-0.5 overflow-y-auto overscroll-contain"
      >
        {visible.map((node, index) => (
          <li key={node.job.id} role="none">
            <button
              ref={(element) => {
                if (element) nodes.current.set(node.job.id, element);
                else nodes.current.delete(node.job.id);
              }}
              type="button"
              role="treeitem"
              aria-level={node.depth + 1}
              aria-setsize={
                node.parentId
                  ? byId.get(node.parentId)?.children.length
                  : outline.filter((item) => !item.parentId).length
              }
              aria-posinset={
                (node.parentId
                  ? byId.get(node.parentId)!.children
                  : outline
                      .filter((item) => !item.parentId)
                      .map((item) => item.job.id)
                ).indexOf(node.job.id) + 1
              }
              aria-selected={selectedId === node.job.id}
              aria-expanded={
                node.children.length ? !collapsed.has(node.job.id) : undefined
              }
              tabIndex={focusableId === node.job.id ? 0 : -1}
              onFocus={() => setFocusedId(node.job.id)}
              onClick={() => onSelect(node.job)}
              onKeyDown={(event) => {
                let target: string | undefined;
                if (event.key === "ArrowDown")
                  target = visible[index + 1]?.job.id;
                else if (event.key === "ArrowUp")
                  target = visible[index - 1]?.job.id;
                else if (event.key === "Home") target = visible[0]?.job.id;
                else if (event.key === "End") target = visible.at(-1)?.job.id;
                else if (event.key === "ArrowLeft") {
                  if (node.children.length && !collapsed.has(node.job.id))
                    toggle(node.job.id, true);
                  else target = node.parentId || undefined;
                } else if (event.key === "ArrowRight") {
                  if (collapsed.has(node.job.id)) toggle(node.job.id, false);
                  else target = node.children[0];
                } else return;
                event.preventDefault();
                if (target) nodes.current.get(target)?.focus();
              }}
              style={{
                paddingInlineStart: `${8 + Math.min(node.depth, 6) * 14}px`,
              }}
              className={cn(
                "ka-agent-tree-item flex w-full items-center gap-2 rounded-md px-2 py-2 text-left",
                selectedId === node.job.id
                  ? "bg-accent/10 text-ink"
                  : "text-muted hover:bg-accent/5 hover:text-ink",
              )}
            >
              <span aria-hidden="true" className="w-2 shrink-0 text-[10px]">
                {node.children.length
                  ? collapsed.has(node.job.id)
                    ? "▸"
                    : "▾"
                  : node.depth
                    ? "└"
                    : ""}
              </span>
              <span
                data-state={node.job.state}
                className="ka-agent-status min-w-0 flex-1"
              >
                <span className="min-w-0">
                  <span
                    className="block truncate text-xs font-medium"
                    title={workerName(node.job)}
                  >
                    {workerName(node.job)}
                  </span>
                  {node.job.progress?.current_tool?.status === "running" && (
                    <span
                      className="mt-0.5 block truncate text-[10px] font-normal text-faint"
                      title={node.job.progress.current_tool.label || node.job.progress.current_tool.name}
                    >
                      {node.job.progress.current_tool.name}
                    </span>
                  )}
                </span>
              </span>
              {node.job.error.approval_id && (
                <span className="shrink-0 text-[10px] font-medium text-warn">
                  Approval
                </span>
              )}
              <span
                className="shrink-0 text-[11px] text-muted"
                title={workerActivity(node.job)}
              >
                {workerStatus(node.job)}
              </span>
              <span className="w-12 shrink-0 text-right text-[10px] tabular-nums text-faint">
                {elapsedLabel(agentElapsed(node.job, now))}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </nav>
  );
}
