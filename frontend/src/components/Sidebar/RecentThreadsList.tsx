import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
} from "react";
import { createPortal } from "react-dom";
import * as Dialog from "@radix-ui/react-dialog";
import { ApiError } from "../../api/client";
import type { SessionSummary } from "../../api/types";
import { cn } from "../../lib/cn";
import { Icon, type IconProps } from "../../lib/icons";
import { useAppStore } from "../../store";
import { effectiveSessionTurnStatus } from "../../store/sessions";
import { StatusDot } from "../shared/StatusDot";
import type { DotStatus } from "../shared/StatusDot";
import { getProjectColor, ProjectSelectorPopover } from "../shared/ProjectSelectorPopover";
import { SidebarSection } from "./SidebarSection";
import { SessionLifecycleDialog } from "./SessionLifecycleDialog";
import {
  THREAD_PAGE,
  topLevelThreads,
  clampVisible,
  growVisible,
} from "./threadPaging";

interface RecentThreadsListProps {
  sessions: SessionSummary[];
  activeSessionId: string | null;
  onOpen: (sessionId: string) => void;
  onForked?: () => void;
}

function sessionDotStatus(turn: string, session: SessionSummary): DotStatus {
  if (turn === "running") return "active";
  if (turn === "waiting" || turn === "waiting_approval" || turn === "connecting") return "waiting";
  if (turn === "error" || session.status === "error") return "error";
  return "idle";
}

function shortTitle(session: SessionSummary): string {
  if (session.title && session.title.trim()) {
    const trimmed = session.title.trim();
    if (/^[a-f0-9]{8,}$/i.test(trimmed) && session.objective && session.objective.trim() && !/^resume\s+task$/i.test(session.objective.trim())) {
      return session.objective.trim();
    }
    return trimmed;
  }
  if (session.objective && session.objective.trim() && !/^resume\s+task$/i.test(session.objective.trim())) {
    return session.objective.trim();
  }
  return session.session_id.slice(0, 8);
}

function timeAgo(dateStr?: string | number | null): string {
  if (!dateStr && dateStr !== 0) return "";
  let ts: number;
  if (typeof dateStr === "number") {
    // Unix timestamp (seconds or milliseconds)
    ts = dateStr > 1e12 ? dateStr : dateStr * 1000;
  } else {
    const parsed = Number(dateStr);
    if (!isNaN(parsed) && parsed > 0) {
      // Numeric string — treat as Unix timestamp
      ts = parsed > 1e12 ? parsed : parsed * 1000;
    } else {
      ts = new Date(dateStr).getTime();
    }
  }
  if (!ts || isNaN(ts)) return "";
  const diff = Date.now() - ts;
  if (diff < 0) return "now";
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months}mo ago`;
  const years = Math.floor(months / 12);
  return `${years}y ago`;
}

function statusLabel(turn: string): string | null {
  if (turn === "running") return "Running";
  if (turn === "connecting") return "Connecting…";
  if (turn === "waiting_approval") return "Waiting";
  if (turn === "error") return "Error";
  return null;
}

// ── Context Menu (portaled to body) ───────────────────────────────────

interface ContextMenuProps {
  x: number;
  y: number;
  session: SessionSummary;
  onClose: () => void;
  onForked?: () => void;
  onManageLifecycle: (session: SessionSummary) => void;
  onEdit: (action: "rename" | "delete", session: SessionSummary) => void;
}

function SessionContextMenu({ x, y, session, onClose, onForked, onManageLifecycle, onEdit }: ContextMenuProps) {
  const pinSession = useAppStore((s) => s.pinSession);
  const archiveSession = useAppStore((s) => s.archiveSession);
  const forkSession = useAppStore((s) => s.forkSession);
  const exportSession = useAppStore((s) => s.exportSession);
  const setSessionProject = useAppStore((s) => s.setSessionProject);
  const [projectPickerOpen, setProjectPickerOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ top: y, left: x });

  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!menu) return;
    const bounds = menu.getBoundingClientRect();
    setPosition({
      top: Math.max(8, Math.min(y, window.innerHeight - bounds.height - 8)),
      left: Math.max(8, Math.min(x, window.innerWidth - bounds.width - 8)),
    });
    const previous = document.activeElement;
    menu.querySelector<HTMLButtonElement>("[role=menuitem]")?.focus();
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); };
  }, [x, y]);

  useEffect(() => {
    const dismiss = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented && !projectPickerOpen) {
        event.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", dismiss);
    window.addEventListener("resize", onClose);
    return () => {
      document.removeEventListener("keydown", dismiss);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose, projectPickerOpen]);

  const handleArchive = useCallback(async () => {
    onClose();
    try {
      const updated = await archiveSession(session.session_id, !session.archived);
      if (updated && !session.archived && ["closing", "failed", "interrupted"].includes(updated.lifecycle_status || "")) {
        onManageLifecycle({ ...session, ...updated, title: updated.title ?? session.title });
      }
    } catch { /* The store reports the failure. */ }
  }, [session, onClose, archiveSession, onManageLifecycle]);

  return createPortal(
    <>
      <div
        className="fixed inset-0 z-[9998]"
        onClick={onClose}
        onContextMenu={(e) => {
          e.preventDefault();
          onClose();
        }}
      />
      <div
        ref={menuRef}
        className="fixed z-[9999] w-72 max-w-[calc(100vw-16px)] max-h-[calc(100dvh-16px)] overflow-y-auto rounded-2xl border border-line-strong bg-surface p-1.5 shadow-xl"
        style={position}
        role="menu"
        id={`session-menu-${session.session_id}`}
        aria-label={`Options for ${shortTitle(session)}`}
        onKeyDown={(event) => {
          if (!(event.target instanceof Node) || !event.currentTarget.contains(event.target)) return;
          if (event.key === "Tab") { onClose(); return; }
          if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
          event.preventDefault();
          const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>("[role=menuitem]:not(:disabled)"));
          const current = items.indexOf(document.activeElement as HTMLButtonElement);
          const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
            : (current + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
          items[next]?.focus();
        }}
      >
        <div className="px-2.5 pt-1.5 pb-2.5" role="presentation">
          <p className="text-[0.6rem] font-semibold uppercase tracking-[0.14em] text-faint">Session actions</p>
          <p className="mt-1 truncate text-xs font-medium text-ink">{shortTitle(session)}</p>
        </div>
        <div className="mx-2 mb-1 border-t border-line" />
        <MenuItem label="Fork session" description="Explore a new direction from here" icon={Icon.Fork} featured onClick={() => {
          onClose();
          void forkSession(session.session_id).then(() => onForked?.()).catch(() => {});
        }} />
        <MenuItem label="Export session" description="Take this conversation to another Kageha" icon={Icon.Export} featured onClick={() => {
          onClose();
          void exportSession(session.session_id).catch(() => {});
        }} />
        <div className="mx-2 my-1 border-t border-line" />
        <MenuItem label="Rename" icon={Icon.Rename} onClick={() => { onClose(); onEdit("rename", session); }} />
        <MenuItem
          label={session.pinned ? "Unpin" : "Pin"}
          icon={session.pinned ? Icon.Unpin : Icon.Pin}
          onClick={() => {
            onClose();
            void pinSession(session.session_id, !session.pinned).catch(() => {});
          }}
        />
        <MenuItem
          label={session.archived ? "Unarchive" : "Archive"}
          icon={session.archived ? Icon.ArchiveRestore : Icon.Archive}
          onClick={handleArchive}
        />
        <MenuItem label="Session lifecycle" description="Close, reopen, or review session hooks" icon={Icon.Close} onClick={() => { onManageLifecycle(session); onClose(); }} />
        {/* Move to Project */}
        <ProjectSelectorPopover
          contentZIndex={10000}
          currentProjectId={session.project_id ?? null}
          open={projectPickerOpen}
          onOpenChange={setProjectPickerOpen}
          onSelectProject={(project) => {
            onClose();
            void setSessionProject(session.session_id, project?.id ?? null);
          }}
          side="right"
          align="start"
          trigger={
            <button
              type="button"
              role="menuitem"
              className="flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-left text-[0.82rem] transition-colors text-ink hover:bg-accent-soft focus-visible:bg-accent-soft focus-visible:outline-none max-lg:min-h-11"
            >
              <Icon.MoveToProject
                size={14}
                className="shrink-0 text-muted"
              />
              {session.project_id ? "Change project" : "Add to project"}
            </button>
          }
        />
        {session.project_id && (
          <MenuItem
            label="Remove from project"
            icon={Icon.ProjectOpen}
            onClick={() => {
              onClose();
              void setSessionProject(session.session_id, null);
            }}
          />
        )}
        <div className="my-1 mx-2 border-t border-[var(--color-line)]" />
        <MenuItem label="Delete" icon={Icon.Delete} onClick={() => { onClose(); onEdit("delete", session); }} danger />
      </div>
    </>,
    document.body,
  );
}

function ThreadActionDialog({ action, session, onDismiss, onManageLifecycle }: {
  action: "rename" | "delete";
  session: SessionSummary;
  onDismiss: () => void;
  onManageLifecycle: (session: SessionSummary) => void;
}) {
  const renameSession = useAppStore((s) => s.renameSession);
  const deleteSession = useAppStore((s) => s.deleteSession);
  const title = shortTitle(session);
  const [draft, setDraft] = useState(title);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const renaming = action === "rename";
  const needsClose = error instanceof ApiError
    && (error.data as { code?: string } | undefined)?.code === "session_close_required";

  return (
    <Dialog.Root open onOpenChange={(open) => { if (!open && !busy) onDismiss(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-[10000] bg-black/50" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-[10001] w-[calc(100%-2rem)] max-w-sm -translate-x-1/2 -translate-y-1/2 rounded-2xl border border-line bg-surface p-5 text-sm text-ink shadow-xl">
          <Dialog.Title className="font-semibold">{renaming ? "Rename thread" : "Delete thread"}</Dialog.Title>
          <Dialog.Description className="mt-1 break-words text-muted">
            {renaming ? title : `Delete “${title}”? This cannot be undone.`}
          </Dialog.Description>
          <form className="mt-4 space-y-3" onSubmit={async (event) => {
            event.preventDefault();
            if (busy || (renaming && (!draft.trim() || draft.trim() === title))) return;
            setBusy(true);
            setError(null);
            try {
              if (renaming) await renameSession(draft.trim(), session.session_id);
              else await deleteSession(session.session_id);
              onDismiss();
            } catch (cause) {
              setError(cause instanceof Error ? cause : new Error(String(cause)));
            } finally {
              setBusy(false);
            }
          }}>
            {renaming && (
              <input aria-label="Thread name" className="w-full rounded-lg border border-line bg-canvas px-3 py-2"
                value={draft} maxLength={200} disabled={busy} onChange={(event) => setDraft(event.target.value)} />
            )}
            {error && <p role="alert" className="text-danger">{error.message}</p>}
            <div className="flex justify-end gap-2">
              <Dialog.Close asChild><button type="button" className="rounded-lg px-3 py-2" disabled={busy}>Cancel</button></Dialog.Close>
              {needsClose ? (
                <button type="button" className="rounded-lg px-3 py-2 text-accent" onClick={() => {
                  onDismiss();
                  onManageLifecycle(session);
                }}>Session lifecycle</button>
              ) : (
                <button type="submit" disabled={busy || (renaming && (!draft.trim() || draft.trim() === title))}
                  className={cn("rounded-lg px-3 py-2 font-medium disabled:opacity-50", renaming ? "bg-accent-soft text-accent" : "bg-danger/15 text-danger")}>
                  {renaming ? "Save name" : "Delete thread"}
                </button>
              )}
            </div>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function MenuItem({
  label,
  icon: ItemIcon,
  onClick,
  danger,
  description,
  featured,
}: {
  label: string;
  icon: ComponentType<IconProps>;
  onClick: () => void;
  danger?: boolean;
  description?: string;
  featured?: boolean;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      aria-label={label}
      className={cn(
        "group flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-left text-[0.82rem] transition-colors focus-visible:outline-none max-lg:min-h-11",
        danger
          ? "text-[var(--color-danger)] hover:bg-[var(--color-danger-soft)] focus-visible:bg-[var(--color-danger-soft)]"
          : "text-ink hover:bg-accent-soft focus-visible:bg-accent-soft",
      )}
      onClick={onClick}
    >
      <span className={cn("flex shrink-0 items-center justify-center", featured && "h-8 w-8 rounded-lg border border-line bg-canvas text-accent group-hover:border-accent/30")}>
        <ItemIcon size={featured ? 16 : 14} className={cn("shrink-0", danger ? "text-[var(--color-danger)]" : featured ? "text-accent" : "text-muted")} />
      </span>
      <span className="min-w-0">
        <span className={cn("block", featured && "text-xs font-medium")}>{label}</span>
        {description && <span className="mt-0.5 block text-[0.65rem] leading-snug text-faint">{description}</span>}
      </span>
    </button>
  );
}

// ── Thread Item ───────────────────────────────────────────────────────

interface ThreadItemProps {
  session: SessionSummary;
  active: boolean;
  onOpen: () => void;
  onContextMenu: (e: React.MouseEvent) => void;
  menuOpen: boolean;
}

const ThreadItem = memo(function ThreadItem({
  session,
  active,
  onOpen,
  onContextMenu,
  menuOpen,
}: ThreadItemProps) {
  const prefetchSession = useAppStore((s) => s.prefetchSession);
  const personaMeta = useAppStore(
    useCallback((s) => {
      if (!session.persona_id) return null;
      const p = s.personas?.find((entry) => entry.id === session.persona_id);
      return p ? `${p.name || ""}\0${p.avatar || ""}\0${p.builtin ? 1 : 0}` : null;
    }, [session.persona_id]),
  );
  const runKey = useAppStore(
    useCallback((s) => {
      const r = s.runs[session.session_id];
      return r ? `${r.status}:${r.sending ? 1 : 0}:${r.waitingApproval ? 1 : 0}:${r.startedAt || 0}:${r.transport?.turnId || (!r.sending ? r.messages?.findLast(m => m.role === "assistant" && m.turnId)?.turnId : "") || ""}` : "";
    }, [session.session_id]),
  );
  const run = useMemo(() => {
    if (!runKey) return null;
    const [status, sending, waitingApproval, startedAt, turnId] = runKey.split(":");
    return { status, sending: sending === "1", waitingApproval: waitingApproval === "1",
      startedAt: Number(startedAt) || undefined, turnId: turnId || undefined };
  }, [runKey]);
  const server = useAppStore(useCallback((s) => s.serverTurns?.[session.session_id] ?? null, [session.session_id]));

  const [personaName, personaAvatar] = useMemo(() => {
    if (!personaMeta) return ["", ""];
    const [name, avatar, builtin] = personaMeta.split("\0");
    const isCustom = builtin !== "1";
    const resolvedAvatar = isCustom
      ? (avatar || "").trim() || name.trim().slice(0, 1).toUpperCase() || "A"
      : "";
    return [name, resolvedAvatar];
  }, [personaMeta]);

  const turn = effectiveSessionTurnStatus(session, run, server);
  const dot = sessionDotStatus(turn, session);
  const status = statusLabel(turn);
  const title = shortTitle(session);
  const ago = timeAgo(session.updated_at);

  return (
    <li className="group relative">
      <button
        type="button"
        className={cn(
          "relative flex w-full items-start gap-2 rounded-lg py-2 pl-2.5 pr-10 text-left transition-all duration-150 focus-visible:outline-2 focus-visible:outline-accent",
          active
            ? "bg-[var(--color-accent-soft)] text-ink shadow-xs before:absolute before:left-0 before:top-2 before:bottom-2 before:w-0.5 before:rounded-r-full before:bg-accent"
            : "hover:bg-line/40 text-muted hover:text-ink",
        )}
        onClick={onOpen}
        onMouseEnter={() => prefetchSession(session.session_id)}
        onContextMenu={onContextMenu}
      >
        {/* Status dot */}
        <div className="mt-1 shrink-0">
          <StatusDot status={dot} />
        </div>

        {/* Content */}
        <div className="min-w-0 flex-1 overflow-hidden">
          <div className="flex items-center gap-1">
            {session.pinned && (
              <span className="shrink-0 text-accent"><Icon.Unpin size={11} /></span>
            )}
            {personaAvatar && (
              <span
                className={cn(
                  "shrink-0 leading-none",
                  // emoji: render at natural size; initial: small badge
                  personaAvatar.length === 1 && !/\p{Emoji}/u.test(personaAvatar)
                    ? "inline-flex h-4 w-4 items-center justify-center rounded bg-accent/20 text-[0.6rem] font-semibold text-accent"
                    : "text-[0.85rem]",
                )}
                title={personaName}
              >
                {personaAvatar}
              </span>
            )}
            <p className="truncate text-[0.82rem] font-medium leading-snug text-ink">
              {title}
            </p>
          </div>
          {session.objective && (() => {
            const trimmedObj = session.objective.trim();
            if (trimmedObj === title || /^resume\s+task$/i.test(trimmedObj)) return null;
            return (
              <p className="mt-0.5 truncate text-[0.72rem] leading-snug text-muted">
                {trimmedObj}
              </p>
            );
          })()}
          <div className="mt-0.5 flex items-center gap-1.5">
            {status && (
              <span
                className={cn(
                  "rounded px-1 py-px text-[0.6rem] font-medium leading-none",
                  dot === "active" && "bg-accent/15 text-accent",
                  dot === "waiting" &&
                    "bg-[var(--color-warn-soft)] text-[var(--color-warn)]",
                  dot === "error" &&
                    "bg-[var(--color-danger-soft)] text-[var(--color-danger)]",
                )}
                title={turn === "running" && (server?.status || session.active_turn?.status) === "waiting_for_unlock"
                  ? "Waiting for credential unlock" : undefined}
              >
                {status}
              </span>
            )}
            {session.project && (
              <span
                className="rounded px-1.5 py-px text-[0.6rem] font-medium leading-none truncate max-w-[80px]"
                style={{
                  backgroundColor: `${getProjectColor(session.project.color)}20`,
                  color: getProjectColor(session.project.color),
                }}
              >
                {session.project.name}
              </span>
            )}
            <span className="text-[0.6rem] text-faint">{ago}</span>
          </div>
        </div>
      </button>
      <button
        type="button"
        className="ka-icon-btn absolute right-1 top-1 h-8 w-8 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100 max-lg:opacity-100 focus-visible:outline-2 focus-visible:outline-accent"
        onClick={(e) => {
          e.stopPropagation();
          onContextMenu(e);
        }}
        aria-label={`Options for ${title}`}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        aria-controls={menuOpen ? `session-menu-${session.session_id}` : undefined}
        title="Thread options"
      >
        <Icon.More size={16} />
      </button>
    </li>
  );
});

// ── Main Component ────────────────────────────────────────────────────

export function RecentThreadsList({
  sessions,
  activeSessionId,
  onOpen,
  onForked,
}: RecentThreadsListProps) {
  const [collapsed, setCollapsed] = useState(() => {
    try { return localStorage.getItem("kageha.sidebar.threads") === "1"; } catch { return false; }
  });
  const [contextMenu, setContextMenu] = useState<{
    x: number;
    y: number;
    sessionId: string;
  } | null>(null);
  const [lifecycleSession, setLifecycleSession] = useState<SessionSummary | null>(null);
  const [edit, setEdit] = useState<{ action: "rename" | "delete"; session: SessionSummary } | null>(null);
  const [showArchived, setShowArchived] = useState(false);
  const closeMenu = useCallback(() => setContextMenu(null), []);
  const refreshSessions = useAppStore((s) => s.refreshSessions);
  const refreshLifecycleSessions = useCallback(async () => { await refreshSessions(); }, [refreshSessions]);
  const sessionLoading = useAppStore((s) => s.sessionLoading);
  const [visibleCount, setVisibleCount] = useState(THREAD_PAGE);
  const sentinelRef = useRef<HTMLLIElement>(null);

  const all = useMemo(() => topLevelThreads(sessions).filter((session) => Boolean(session.archived) === showArchived), [sessions, showArchived]);
  const menuSession = contextMenu && all.find((session) => session.session_id === contextMenu.sessionId);
  const recent = all.slice(0, visibleCount);
  const hasMore = visibleCount < all.length;

  useEffect(() => {
    setVisibleCount((n) => clampVisible(n, all.length));
  }, [all.length]);

  useEffect(() => {
    const node = sentinelRef.current;
    if (!node || !hasMore) return;
    const root = node.closest(".ka-sidebar-threads");
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setVisibleCount((n) => growVisible(n, all.length));
        }
      },
      { root: root instanceof Element ? root : null, rootMargin: "120px 0px" },
    );
    io.observe(node);
    return () => io.disconnect();
  }, [all.length, hasMore, visibleCount]);

  const handleContextMenu = useCallback(
    (e: React.MouseEvent, session: SessionSummary) => {
      e.preventDefault();
      e.stopPropagation();
      const bounds = e.currentTarget.getBoundingClientRect();
      setContextMenu({
        x: e.clientX || bounds.right, y: e.clientY || bounds.top, sessionId: session.session_id,
      });
    },
    [],
  );

  const toggle = () => {
    const next = !collapsed;
    setCollapsed(next);
    try { localStorage.setItem("kageha.sidebar.threads", next ? "1" : "0"); } catch { /* private storage */ }
  };

  return (
    <div className="px-3 py-2">
      <SidebarSection label={showArchived ? "Archived threads" : "Threads"} count={all.length} collapsed={collapsed} onToggle={toggle}
        action={<button type="button" className="ka-icon-btn h-7 w-7" aria-label="Show archived threads"
          title={showArchived ? "Show active threads" : "Show archived threads"} aria-pressed={showArchived}
          onClick={() => {
            setShowArchived(!showArchived);
            setVisibleCount(THREAD_PAGE);
            if (collapsed) toggle();
          }}><Icon.Archive size={14} /></button>}>
        <ul className="space-y-0.5">
          {recent.map((session) => (
            <ThreadItem
              key={session.session_id}
              session={session}
              active={session.session_id === activeSessionId}
              onOpen={() => onOpen(session.session_id)}
              onContextMenu={(e: React.MouseEvent) => handleContextMenu(e, session)}
              menuOpen={contextMenu?.sessionId === session.session_id}
            />
          ))}
          {hasMore && (
            <li ref={sentinelRef} className="h-1 list-none" aria-hidden />
          )}
          {sessionLoading && all.length === 0 ? (
            Array.from({ length: 4 }).map((_, i) => (
              <li key={`skeleton-${i}`} className="flex flex-col gap-1.5 rounded-lg py-2 pl-2.5 pr-4 animate-pulse">
                <div className="h-3.5 w-3/4 rounded bg-line/80" />
                <div className="h-2.5 w-1/2 rounded bg-line/50" />
              </li>
            ))
          ) : recent.length === 0 ? (
            <li className="rounded-lg border border-dashed border-[var(--color-line)] px-3 py-4 text-center text-xs text-faint">
              {showArchived ? "No archived threads" : "No threads yet"}
            </li>
          ) : null}
        </ul>
      </SidebarSection>

      {contextMenu && menuSession && (
        <SessionContextMenu
          key={menuSession.session_id}
          x={contextMenu.x}
          y={contextMenu.y}
          session={menuSession}
          onClose={closeMenu}
          onForked={onForked}
          onManageLifecycle={setLifecycleSession}
          onEdit={(action, session) => setEdit({ action, session })}
        />
      )}
      {edit && <ThreadActionDialog {...edit} onDismiss={() => setEdit(null)} onManageLifecycle={setLifecycleSession} />}
      {lifecycleSession && <SessionLifecycleDialog sessionId={lifecycleSession.session_id} title={shortTitle(lifecycleSession)} onDismiss={() => setLifecycleSession(null)} onChanged={refreshLifecycleSessions} />}
    </div>
  );
}
