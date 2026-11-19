import { useState } from "react";
import * as Popover from "@radix-ui/react-popover";
import { api } from "../api/client";
import { useSessionTruth, refreshSessionTruth, type FileWorkspace } from "../lib/sessionTruth";

export function WorkspaceChip({ sessionId }: { sessionId: string | null }) {
  const truth = useSessionTruth(sessionId);
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<FileWorkspace["mode"]>("private");
  const [path, setPath] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  if (!sessionId || !truth.data?.workspace) return null;
  const workspace = truth.data.workspace;
  const remote = truth.data.selected_target === "remote";
  const busy = Boolean(truth.data.turn && ["accepted", "planning", "executing", "verifying", "repairing", "waiting_approval", "waiting_dependencies"].includes(truth.data.turn.phase));
  const locked = truth.status !== "current" || busy || pending || remote;
  const label = remote ? "Remote workspace" : workspace.mode === "private" ? "Chat files" : workspace.mode === "worktree" ? "Isolated checkout" : "Shared folder";
  const save = async () => {
    setPending(true); setError("");
    try {
      await api(`/api/sessions/${encodeURIComponent(sessionId)}/workspace`, { method: "POST",
        body: JSON.stringify({ mode, project_root: mode === "private" ? "" : path }) });
      refreshSessionTruth(sessionId); setOpen(false);
    } catch (err) { setError(err instanceof Error ? err.message : String(err)); }
    finally { setPending(false); }
  };

  const handleOpenChange = (nextOpen: boolean) => {
    if (nextOpen) {
      setMode(workspace.mode);
      setPath(workspace.source_root);
      setError("");
    }
    setOpen(nextOpen);
  };

  return (
    <Popover.Root open={open} onOpenChange={handleOpenChange}>
      <Popover.Trigger asChild>
        <button
          type="button"
          disabled={locked}
          className="rounded-md border border-line px-2 py-0.5 text-xs text-muted hover:text-ink hover:border-line-strong transition-colors disabled:opacity-60 inline-flex items-center gap-1.5"
          title={remote ? "Remote execution uses the remote host workspace" : `${workspace.root}${busy ? " · stop the run before changing folders" : ""}`}
        >
          <span>{label}</span>
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          role="dialog"
          aria-label="Chat file workspace"
          side="bottom"
          align="start"
          sideOffset={6}
          className="z-[9999] w-84 rounded-xl border border-line bg-surface p-4 shadow-xl backdrop-blur-md animate-in fade-in-0 zoom-in-95 text-ink text-sm"
        >
          <span className="mb-3 block text-sm font-semibold">Where this chat works</span>
          {([ ["private", "This chat’s files"], ["shared", "Use a shared project folder"], ["worktree", "Create an isolated Git checkout"] ] as const).map(([value, text]) =>
            <label key={value} className="mb-2 flex items-center gap-2 text-xs text-ink cursor-pointer">
              <input type="radio" name="file-workspace" checked={mode === value} disabled={pending} onChange={() => setMode(value)} className="accent-accent" />
              <span>{text}</span>
            </label>)}
          {mode !== "private" && (
            <label className="mt-3 block text-xs font-medium text-muted">
              Project folder
              <input
                aria-label="Project folder"
                className="mt-1 w-full rounded-md border border-line bg-canvas p-2 text-xs text-ink font-mono outline-none focus:border-accent"
                value={path}
                disabled={pending}
                onChange={e => setPath(e.target.value)}
                placeholder="/absolute/path/to/project"
              />
            </label>
          )}
          <span className="my-3 block text-xs leading-relaxed text-muted">
            Artifacts stay in this chat. Existing files are kept when you switch. An isolated checkout starts from the project’s committed HEAD; uncommitted files are not copied.
          </span>
          {mode === "shared" && <span className="mb-3 block text-xs text-muted">Other chats using this same folder share its files.</span>}
          {error && <span role="alert" className="mb-3 block text-xs text-rose-500 font-medium">{error}</span>}
          <div className="flex justify-end gap-2 pt-1 border-t border-line/40">
            <button
              type="button"
              className="rounded-lg px-2.5 py-1 text-xs text-muted hover:bg-line/40 hover:text-ink transition-colors"
              disabled={pending}
              onClick={() => setOpen(false)}
            >
              Cancel
            </button>
            <button
              type="button"
              className="rounded-lg bg-accent px-3 py-1 text-xs font-semibold text-white shadow-xs hover:opacity-90 active:scale-[0.98] transition-all disabled:opacity-40"
              disabled={locked || (mode !== "private" && !path.startsWith("/"))}
              onClick={() => void save()}
            >
              {pending ? "Saving…" : "Use workspace"}
            </button>
          </div>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
