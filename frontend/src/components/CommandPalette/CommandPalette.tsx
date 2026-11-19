import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import { cn } from "../../lib/cn";
import { withViewTransition } from "../../lib/viewTransition";
import { useAppStore } from "../../store";

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  onOpenShortcuts?: () => void;
}

interface PaletteAction {
  id: string;
  title: string;
  subtitle?: string;
  category: "Actions" | "Surfaces" | "Agents" | "Threads";
  icon: React.ReactNode;
  shortcut?: string;
  run: () => void;
}

export function CommandPalette({ open, onClose, onOpenShortcuts }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  const sessions = useAppStore((s) => s.sessions);
  const newChat = useAppStore((s) => s.newChat);
  const openSession = useAppStore((s) => s.openSession);
  const prefetchSession = useAppStore((s) => s.prefetchSession);
  const prefs = useAppStore((s) => s.prefs);
  const setPrefs = useAppStore((s) => s.setPrefs);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const personas = useAppStore((s) => s.personas);
  const setActivePersonaId = useAppStore((s) => s.setActivePersonaId);

  // Focus input when opened
  useEffect(() => {
    if (open) {
      setQuery("");
      setSelectedIndex(0);
      setTimeout(() => inputRef.current?.focus(), 20);
    }
  }, [open]);

  const actions = useMemo<PaletteAction[]>(() => {
    const list: PaletteAction[] = [
      {
        id: "act-new-chat",
        title: "New thread",
        subtitle: "Start a fresh task or conversation",
        category: "Actions",
        icon: <Icon.NewThread size={16} />,
        shortcut: "⌘N",
        run: () => {
          onClose();
          withViewTransition(() => void newChat());
        },
      },
      {
        id: "act-toggle-sidebar",
        title: prefs.sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar",
        subtitle: "Toggle navigation sidebar visibility",
        category: "Actions",
        icon: <Icon.Collapse size={16} />,
        shortcut: "⌘B",
        run: () => {
          onClose();
          setPrefs({ sidebarCollapsed: !prefs.sidebarCollapsed });
        },
      },
      {
        id: "act-toggle-right-panel",
        title: prefs.rightPanelCollapsed ? "Expand right panel" : "Collapse right panel",
        subtitle: "Toggle workspace & monitor panel",
        category: "Actions",
        icon: <Icon.Canvas size={16} />,
        shortcut: "⌘\\",
        run: () => {
          onClose();
          setPrefs({ rightPanelCollapsed: !prefs.rightPanelCollapsed });
        },
      },
      {
        id: "act-toggle-theme",
        title: "Toggle theme",
        subtitle: `Current: ${prefs.theme || "dark"}`,
        category: "Actions",
        icon: <Icon.Moon size={16} />,
        run: () => {
          onClose();
          const themes: Array<"dark" | "midnight" | "teal" | "light"> = ["dark", "midnight", "teal", "light"];
          const currentIdx = themes.indexOf(prefs.theme as "dark" | "midnight" | "teal" | "light");
          const next = themes[(currentIdx + 1) % themes.length];
          setPrefs({ theme: next });
        },
      },
      {
        id: "act-shortcuts",
        title: "Keyboard shortcuts",
        subtitle: "View complete cheatsheet of hotkeys",
        category: "Actions",
        icon: <Icon.Keyboard size={16} />,
        shortcut: "?",
        run: () => {
          onClose();
          onOpenShortcuts?.();
        },
      },
      {
        id: "surf-monitor",
        title: "Activity Monitor",
        subtitle: "Live tool calls, stats, and DAG execution",
        category: "Surfaces",
        icon: <Icon.Activity size={16} />,
        shortcut: "⌘3",
        run: () => {
          onClose();
          setPrefs({ rightPanelCollapsed: false });
          pinWorkspaceSurface("monitor");
        },
      },
      {
        id: "surf-artifacts",
        title: "Files & Artifacts",
        subtitle: "Generated code, documents, and assets",
        category: "Surfaces",
        icon: <Icon.Canvas size={16} />,
        shortcut: "⌘2",
        run: () => {
          onClose();
          setPrefs({ rightPanelCollapsed: false });
          pinWorkspaceSurface("artifacts");
        },
      },
      {
        id: "surf-computer",
        title: "Computer Screen",
        subtitle: "Watch live desktop / browser execution",
        category: "Surfaces",
        icon: <Icon.Terminal size={16} />,
        shortcut: "⌘1",
        run: () => {
          onClose();
          setPrefs({ rightPanelCollapsed: false });
          pinWorkspaceSurface("live");
        },
      },
    ];

    // Personas
    for (const persona of (personas || []).slice(0, 5)) {
      list.push({
        id: `persona-${persona.id}`,
        title: `Switch Agent: ${persona.name}`,
        subtitle: persona.system_prompt ? persona.system_prompt.slice(0, 60) + "…" : "Agent persona",
        category: "Agents",
        icon: <Icon.Brain size={16} />,
        run: () => {
          onClose();
          setActivePersonaId(persona.id);
        },
      });
    }

    // Recent threads
    const recent = (sessions || []).filter((s) => !s.archived).slice(0, 15);
    for (const session of recent) {
      const title = session.title?.trim() || session.session_id.slice(0, 8);
      list.push({
        id: `thread-${session.session_id}`,
        title,
        subtitle: session.objective || (session.status ? `Status: ${session.status}` : undefined),
        category: "Threads",
        icon: <Icon.NewThread size={16} />,
        run: () => {
          onClose();
          withViewTransition(() => void openSession(session.session_id));
        },
      });
    }

    return list;
  }, [sessions, newChat, openSession, prefs, setPrefs, pinWorkspaceSurface, personas, setActivePersonaId, onClose]);

  // Filter actions by query
  const filteredActions = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return actions;
    return actions.filter(
      (a) =>
        a.title.toLowerCase().includes(q) ||
        (a.subtitle && a.subtitle.toLowerCase().includes(q)) ||
        a.category.toLowerCase().includes(q),
    );
  }, [actions, query]);

  // Clamp selection index
  useEffect(() => {
    setSelectedIndex(0);
  }, [query]);

  // Scroll active item into view
  useEffect(() => {
    const el = listRef.current?.children[selectedIndex] as HTMLElement | undefined;
    el?.scrollIntoView({ block: "nearest" });
  }, [selectedIndex]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIndex((i) => (filteredActions.length ? (i + 1) % filteredActions.length : 0));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIndex((i) =>
          filteredActions.length ? (i - 1 + filteredActions.length) % filteredActions.length : 0,
        );
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        const selected = filteredActions[selectedIndex];
        if (selected) selected.run();
        return;
      }
    },
    [filteredActions, selectedIndex, onClose],
  );

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center p-4 pt-[12vh] bg-black/60 backdrop-blur-md animate-[fadeIn_140ms_ease-out]"
      onClick={onClose}
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
    >
      <div
        className="w-full max-w-[580px] overflow-hidden rounded-2xl border border-[var(--color-line)] bg-surface shadow-2xl transition-all"
        style={{
          boxShadow: "0 24px 64px -12px rgba(0, 0, 0, 0.45), 0 0 0 1px color-mix(in srgb, var(--color-accent) 15%, transparent)",
        }}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        {/* Search header */}
        <div className="flex items-center gap-3 border-b border-[var(--color-line)] px-4 py-3.5">
          <span className="text-muted shrink-0">
            <Icon.Search size={18} />
          </span>
          <input
            ref={inputRef}
            type="text"
            className="flex-1 bg-transparent text-sm text-ink outline-none placeholder:text-faint"
            placeholder="Type a command or search recent threads…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <kbd className="hidden sm:inline-flex items-center rounded border border-[var(--color-line)] px-1.5 py-0.5 text-[0.62rem] font-mono text-faint">
            ESC
          </kbd>
        </div>

        {/* Results list */}
        <ul ref={listRef} className="max-h-[380px] overflow-y-auto p-2">
          {filteredActions.length === 0 ? (
            <li className="py-8 text-center text-xs text-faint">No matching commands or threads</li>
          ) : (
            filteredActions.map((action, i) => {
              const active = i === selectedIndex;
              const isThread = action.id.startsWith("thread-");
              const threadId = isThread ? action.id.replace("thread-", "") : "";
              return (
                <li
                  key={action.id}
                  className={cn(
                    "flex items-center gap-3 rounded-xl px-3 py-2 text-left cursor-pointer transition-colors",
                    active ? "bg-accent-soft text-ink" : "text-muted hover:bg-[var(--color-surface-hover)] hover:text-ink",
                  )}
                  onClick={action.run}
                  onMouseEnter={() => {
                    setSelectedIndex(i);
                    if (threadId) prefetchSession(threadId);
                  }}
                >
                  <span
                    className={cn(
                      "flex h-7 w-7 shrink-0 items-center justify-center rounded-lg border border-[var(--color-line)]/50",
                      active ? "bg-accent/15 text-accent" : "bg-canvas text-faint",
                    )}
                  >
                    {action.icon}
                  </span>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-xs font-medium text-ink">{action.title}</p>
                    {action.subtitle && <p className="truncate text-[0.68rem] text-faint">{action.subtitle}</p>}
                  </div>
                  {action.shortcut ? (
                    <kbd className="shrink-0 rounded border border-[var(--color-line)] bg-canvas/60 px-1.5 py-0.5 font-mono text-[0.6rem] text-muted">
                      {action.shortcut}
                    </kbd>
                  ) : (
                    <span className="text-[0.6rem] uppercase tracking-wider text-faint/60 shrink-0">
                      {action.category}
                    </span>
                  )}
                </li>
              );
            })
          )}
        </ul>

        {/* Footer hints */}
        <div className="flex items-center justify-between border-t border-[var(--color-line)] px-4 py-2 text-[0.68rem] text-faint">
          <div className="flex items-center gap-3">
            <span><kbd className="font-mono">↑↓</kbd> to navigate</span>
            <span><kbd className="font-mono">↵</kbd> to select</span>
          </div>
          <span><kbd className="font-mono">esc</kbd> to dismiss</span>
        </div>
      </div>
    </div>
  );
}
