import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../api/client";
import { Icon } from "../../lib/icons";

interface Worktree {
  path: string;
  branch: string;
  head?: string;
  creation_id?: string | null;
}

interface WorktreeOperation {
  id: string;
  state: string;
  label: string;
  base: string;
}

interface CleanupOperation {
  id: string;
  creation_id: string;
  state: string;
}

interface WorktreesResponse {
  project_root: string;
  worktrees: Worktree[];
  operations?: WorktreeOperation[];
  cleanup_operations?: CleanupOperation[];
}

interface CreateResponse {
  path: string;
  branch: string;
  root: string;
  operation?: WorktreeOperation;
}

function shortSha(sha?: string): string {
  if (!sha) return "";
  return sha.slice(0, 8);
}

function shortPath(path: string): string {
  // Trim to last 2 path segments for readability.
  const parts = path.split("/").filter(Boolean);
  if (parts.length <= 2) return path;
  return "…/" + parts.slice(-2).join("/");
}

export function WorktreesPanel({ onClose }: { onClose: () => void }) {
  const [worktrees, setWorktrees] = useState<Worktree[]>([]);
  const [projectRoot, setProjectRoot] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [label, setLabel] = useState("agent");
  const [base, setBase] = useState("HEAD");
  const [creating, setCreating] = useState(false);
  const [pending, setPending] = useState<WorktreeOperation | null>(null);
  const [cleanupPending, setCleanupPending] = useState<CleanupOperation | null>(
    null,
  );
  const cleanupIds = useRef<Record<string, string>>({});
  const creation = useRef<{ id: string; label: string; base: string } | null>(
    null,
  );

  const fetchWorktrees = useCallback(async () => {
    try {
      const data = await api<WorktreesResponse>("/api/worktrees");
      setWorktrees(data.worktrees || []);
      setProjectRoot(data.project_root || "");
      setPending(data.operations?.[0] || null);
      setCleanupPending(data.cleanup_operations?.[0] || null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load worktrees");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void fetchWorktrees();
  }, [fetchWorktrees]);

  const handleCreate = useCallback(async () => {
    if (!label.trim()) return;
    setCreating(true);
    setError(null);
    try {
      const requested = { label: label.trim(), base: base.trim() || "HEAD" };
      if (
        !creation.current ||
        creation.current.label !== requested.label ||
        creation.current.base !== requested.base
      ) {
        creation.current = { ...requested, id: crypto.randomUUID() };
      }
      await api<CreateResponse>("/api/worktrees", {
        method: "POST",
        body: JSON.stringify({ ...requested, request_id: creation.current.id }),
      });
      creation.current = null;
      setLabel("agent");
      setBase("HEAD");
      setShowCreate(false);
      await fetchWorktrees();
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "Failed to create worktree",
      );
      if (
        err instanceof ApiError &&
        (err.data as { operation?: WorktreeOperation })?.operation
      ) {
        setPending((err.data as { operation: WorktreeOperation }).operation);
      }
      await fetchWorktrees();
    } finally {
      setCreating(false);
    }
  }, [label, base, fetchWorktrees]);

  const recover = useCallback(async () => {
    if (!pending) return;
    setCreating(true);
    try {
      const result =
        pending.state === "prepared"
          ? await api<CreateResponse>("/api/worktrees", {
              method: "POST",
              body: JSON.stringify({
                request_id: pending.id,
                label: pending.label,
                base: pending.base,
              }),
            })
          : await api<CreateResponse>(
              `/api/worktrees/operations/${encodeURIComponent(pending.id)}`,
              { method: "POST" },
            );
      if (result.operation?.state === "completed") {
        setError(null);
        creation.current = null;
      } else
        setError(
          "Worktree creation is still unverified. Review the repository before continuing.",
        );
      await fetchWorktrees();
    } catch (err) {
      setError(
        err instanceof Error
          ? err.message
          : "Could not inspect worktree creation",
      );
    } finally {
      setCreating(false);
    }
  }, [pending, fetchWorktrees]);

  const removeManaged = useCallback(
    async (creationId: string, requestId?: string) => {
      setCreating(true);
      setError(null);
      if (requestId) cleanupIds.current[creationId] = requestId;
      else cleanupIds.current[creationId] ||= crypto.randomUUID();
      try {
        await api("/api/worktrees/cleanup", {
          method: "POST",
          body: JSON.stringify({
            creation_id: creationId,
            request_id: cleanupIds.current[creationId],
          }),
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : "Cleanup needs review");
      } finally {
        await fetchWorktrees();
        setCreating(false);
      }
    },
    [fetchWorktrees],
  );
  const recoverCleanup = useCallback(async () => {
    if (!cleanupPending) return;
    if (["prepared", "worktree_removed"].includes(cleanupPending.state)) {
      await removeManaged(cleanupPending.creation_id, cleanupPending.id);
      return;
    }
    setCreating(true);
    setError(null);
    try {
      await api(
        `/api/worktrees/cleanup/${encodeURIComponent(cleanupPending.id)}`,
        { method: "POST" },
      );
    } catch (err) {
      setError(
        err instanceof Error ? err.message : "Could not inspect cleanup",
      );
    } finally {
      await fetchWorktrees();
      setCreating(false);
    }
  }, [cleanupPending, removeManaged, fetchWorktrees]);

  return (
    <div className="flex h-full flex-col bg-surface">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-line px-4 py-3">
        <h2 className="text-sm font-semibold text-ink">Worktrees</h2>
        <div className="flex items-center gap-2">
          <button
            type="button"
            className="inline-flex items-center gap-1 rounded-md bg-accent/15 px-2.5 py-1 text-xs font-medium text-accent hover:bg-accent/25 transition-colors"
            onClick={() => setShowCreate(!showCreate)}
          >
            <Icon.NewThread size={13} />
            New
          </button>
          <button
            type="button"
            className="ka-icon-btn h-7 w-7"
            onClick={onClose}
            aria-label="Close"
          >
            <Icon.Delete size={15} />
          </button>
        </div>
      </div>

      {/* Create form */}
      {showCreate && (
        <div className="space-y-2 border-b border-line px-4 py-3">
          <label className="block">
            <span className="mb-1 block text-[0.7rem] font-medium text-muted">
              Label
            </span>
            <input
              type="text"
              className="w-full rounded-md border border-line bg-canvas px-3 py-1.5 text-sm text-ink placeholder:text-faint focus:border-accent focus:outline-none"
              placeholder="agent"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
            />
          </label>
          <label className="block">
            <span className="mb-1 block text-[0.7rem] font-medium text-muted">
              Base ref
            </span>
            <input
              type="text"
              className="w-full rounded-md border border-line bg-canvas px-3 py-1.5 text-sm text-ink placeholder:text-faint focus:border-accent focus:outline-none"
              placeholder="HEAD"
              value={base}
              onChange={(e) => setBase(e.target.value)}
            />
          </label>
          <button
            type="button"
            className="w-full rounded-md bg-accent px-3 py-1.5 text-xs font-medium text-white hover:bg-accent/80 disabled:opacity-50"
            disabled={!label.trim() || creating}
            onClick={() => void handleCreate()}
          >
            {creating ? "Creating…" : "Create Worktree"}
          </button>
        </div>
      )}

      {/* Error */}
      {error && (
        <div className="border-b border-[var(--color-danger)]/20 bg-[var(--color-danger-soft)] px-4 py-2 text-xs text-[var(--color-danger)]">
          {error}
        </div>
      )}

      {pending && (
        <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-2 text-xs text-muted">
          <span>
            {pending.state === "prepared"
              ? "Worktree creation is ready to resume."
              : "Worktree creation needs review."}
          </span>
          <button
            type="button"
            disabled={creating}
            onClick={() => void recover()}
            className="shrink-0 rounded border border-line px-2 py-1 hover:bg-line/30 disabled:opacity-50"
          >
            {pending.state === "prepared"
              ? "Resume creation"
              : "Check worktree"}
          </button>
        </div>
      )}

      {cleanupPending && (
        <div className="flex items-center justify-between gap-3 border-b border-line px-4 py-2 text-xs text-muted">
          <span>Worktree cleanup needs review. Changed work is preserved.</span>
          <button
            type="button"
            disabled={creating}
            onClick={() => void recoverCleanup()}
            className="shrink-0 rounded border border-line px-2 py-1 hover:bg-line/30 disabled:opacity-50"
          >
            {["prepared", "worktree_removed"].includes(cleanupPending.state)
              ? "Resume cleanup"
              : "Check cleanup"}
          </button>
        </div>
      )}
      {/* Worktree list */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {loading ? (
          <div className="flex items-center justify-center py-8 text-sm text-muted">
            Loading…
          </div>
        ) : worktrees.length === 0 ? (
          <div className="flex flex-col items-center justify-center px-4 py-8 text-center">
            <Icon.Worktrees size={28} className="mb-2 text-faint" />
            <p className="text-sm text-muted">No worktrees</p>
            <p className="mt-1 text-xs text-faint">
              Is this a git repository? Worktrees create isolated branches for
              parallel agent work.
            </p>
          </div>
        ) : (
          <ul className="divide-y divide-line">
            {worktrees.map((wt) => (
              <li
                key={wt.path}
                className="px-4 py-3 hover:bg-line/30 transition-colors"
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <Icon.Worktrees
                        size={14}
                        className="shrink-0 text-accent"
                      />
                      <p className="truncate text-sm font-medium text-ink">
                        {wt.branch || "(detached)"}
                      </p>
                    </div>
                    <p
                      className="mt-1 truncate font-mono text-[0.68rem] text-faint"
                      title={wt.path}
                    >
                      {shortPath(wt.path)}
                    </p>
                    {wt.head && (
                      <p className="mt-0.5 font-mono text-[0.62rem] text-faint">
                        {shortSha(wt.head)}
                      </p>
                    )}
                  </div>
                  {wt.creation_id && (
                    <button
                      type="button"
                      disabled={creating || !!pending || !!cleanupPending}
                      onClick={() => void removeManaged(wt.creation_id!)}
                      title="Remove only if this managed checkout is unchanged, including ignored files"
                      className="shrink-0 rounded border border-line px-2 py-1 text-xs text-muted hover:bg-line/30 disabled:opacity-50"
                    >
                      Remove unchanged
                    </button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>

      {/* Footer */}
      {projectRoot && (
        <div className="border-t border-line px-4 py-2">
          <p className="truncate text-[0.62rem] text-faint" title={projectRoot}>
            {projectRoot}
          </p>
        </div>
      )}
    </div>
  );
}
