import { useEffect, useRef } from "react";
import { Icon } from "../../lib/icons";

interface ShortcutsModalProps {
  open: boolean;
  onClose: () => void;
}

interface ShortcutCategory {
  title: string;
  items: Array<{ key: string; description: string }>;
}

const SHORTCUT_CATEGORIES: ShortcutCategory[] = [
  {
    title: "Navigation & Palette",
    items: [
      { key: "⌘ K", description: "Open Command Palette" },
      { key: "⌘ N", description: "Create new thread" },
      { key: "⌥ ↑ / ⌥ ↓", description: "Switch between threads" },
      { key: "⌘ B", description: "Toggle sidebar" },
      { key: "⌘ \\", description: "Toggle right workspace" },
      { key: "⌘ ⇧ S", description: "Toggle subagents / jobs panel" },
      { key: "?", description: "Show keyboard shortcuts" },
    ],
  },
  {
    title: "Workspace Surfaces",
    items: [
      { key: "⌘ 1", description: "Switch to Computer screen" },
      { key: "⌘ 2", description: "Switch to Files & outputs" },
      { key: "⌘ 3", description: "Switch to Activity monitor" },
    ],
  },
  {
    title: "Composer & Chat",
    items: [
      { key: "/", description: "Slash command autocomplete" },
      { key: "@", description: "Direct message an agent persona" },
      { key: "↵", description: "Send message" },
      { key: "⇧ ↵", description: "Insert newline" },
      { key: "Esc", description: "Dismiss modal / Cancel focus" },
    ],
  },
];

export function ShortcutsModal({ open, onClose }: ShortcutsModalProps) {
  const modalRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/60 backdrop-blur-md animate-[fadeIn_140ms_ease-out]"
      onClick={onClose}
      role="dialog"
      aria-modal="true"
      aria-label="Keyboard shortcuts"
    >
      <div
        ref={modalRef}
        className="w-full max-w-[560px] overflow-hidden rounded-2xl border border-[var(--color-line)] bg-surface shadow-2xl transition-all"
        style={{
          boxShadow: "0 24px 64px -12px rgba(0, 0, 0, 0.45), 0 0 0 1px color-mix(in srgb, var(--color-accent) 15%, transparent)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b border-[var(--color-line)] px-5 py-4">
          <div className="flex items-center gap-2">
            <span className="text-accent"><Icon.Gauge size={18} /></span>
            <h2 className="text-sm font-semibold text-ink">Keyboard Shortcuts</h2>
          </div>
          <button
            type="button"
            className="rounded-lg p-1 text-faint hover:bg-canvas hover:text-ink transition-colors"
            onClick={onClose}
            aria-label="Close"
          >
            <Icon.Close size={16} />
          </button>
        </div>

        {/* Content */}
        <div className="max-h-[70vh] overflow-y-auto p-5 space-y-6">
          {SHORTCUT_CATEGORIES.map((cat) => (
            <div key={cat.title}>
              <h3 className="mb-2 text-[0.68rem] font-semibold uppercase tracking-wider text-muted">
                {cat.title}
              </h3>
              <div className="divide-y divide-line/40 rounded-xl border border-line/50 bg-canvas/40 px-3">
                {cat.items.map((item) => (
                  <div key={item.key} className="flex items-center justify-between py-2.5">
                    <span className="text-xs text-ink/90">{item.description}</span>
                    <kbd className="inline-flex items-center gap-1 rounded-md border border-[var(--color-line)] bg-surface px-2 py-0.5 font-mono text-[0.68rem] font-medium text-muted shadow-sm">
                      {item.key}
                    </kbd>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>

        {/* Footer */}
        <div className="border-t border-[var(--color-line)] bg-canvas/20 px-5 py-2.5 text-right text-[0.68rem] text-faint">
          Press <kbd className="font-mono text-muted">Esc</kbd> to close
        </div>
      </div>
    </div>
  );
}
