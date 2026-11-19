import { useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";

interface SidebarHeaderProps {
  onNewThread: () => void;
  onCollapse?: () => void;
  onImported?: () => void;
}

export function SidebarHeader({ onNewThread, onCollapse, onImported }: SidebarHeaderProps) {
  const importSession = useAppStore((s) => s.importSession);
  const inputRef = useRef<HTMLInputElement>(null);
  const [importing, setImporting] = useState(false);
  return (
    <div className="ka-sidebar-brand flex flex-col gap-2.5 px-3 pt-3.5 pb-2">
      <div className="flex items-center justify-between">
        <span className="flex items-center gap-2 text-base font-semibold tracking-tight text-ink">
          <span className="flex h-6 w-6 items-center justify-center rounded-md bg-accent/10 text-accent">
            <Icon.Logo size={15} />
          </span>
          <span>Kageha</span>
        </span>
        <button
          type="button"
          className="ka-icon-btn h-8 w-8 rounded-lg text-faint hover:text-ink hover:bg-line/40 lg:h-7 lg:w-7"
          aria-label="Collapse sidebar"
          title="Collapse sidebar"
          onClick={onCollapse}
        >
          <Icon.Collapse size={16} />
        </button>
      </div>

      <button
        type="button"
        className="ka-new-thread group flex w-full items-center gap-2.5 rounded-lg border border-line/70 bg-surface/80 px-3 py-2 text-xs font-medium text-ink shadow-2xs transition-all hover:border-accent/40 hover:bg-surface hover:shadow-xs active:scale-[0.99]"
        onClick={onNewThread}
      >
        <Icon.NewThread size={15} className="text-accent group-hover:scale-105 transition-transform" />
        <span className="font-medium text-ink">New thread</span>
        <kbd className="ml-auto rounded-md border border-line/60 bg-canvas/60 px-1.5 py-0.5 text-[0.65rem] font-sans font-medium text-muted/80 group-hover:text-ink">
          ⌘N
        </kbd>
      </button>

      <input
        ref={inputRef} type="file" accept=".zip,application/zip" className="hidden"
        aria-label="Session archive"
        onChange={async (event) => {
          const file = event.target.files?.[0];
          event.target.value = "";
          if (!file || importing) return;
          setImporting(true);
          try {
            await importSession(file);
            onImported?.();
          } catch {
            // The store displays the server's validation error.
          } finally {
            setImporting(false);
          }
        }}
      />
      <button
        type="button" disabled={importing} onClick={() => inputRef.current?.click()}
        aria-label={importing ? "Importing session…" : "Import session"}
        aria-busy={importing}
        className="group flex w-full items-center justify-between rounded-lg border border-line/40 bg-surface/30 px-2.5 py-1.5 text-xs text-muted transition-all hover:border-line/70 hover:bg-surface/70 hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-accent disabled:cursor-wait disabled:opacity-70"
        title="Import messages, uploaded files, artifacts, and settings from a Kageha session ZIP"
      >
        <span className="flex items-center gap-2 min-w-0">
          {importing ? (
            <Icon.Loader size={13} className="animate-spin text-accent shrink-0 motion-reduce:animate-none" />
          ) : (
            <Icon.Import size={13} className="shrink-0 text-faint group-hover:text-accent transition-colors" />
          )}
          <span className="truncate text-[0.72rem] font-medium">
            {importing ? "Importing session…" : "Import session"}
          </span>
        </span>
        {!importing && (
          <span className="rounded-md border border-line/60 bg-canvas/60 px-1.5 py-0.5 text-[0.62rem] font-medium tracking-wide text-faint group-hover:text-muted">
            ZIP
          </span>
        )}
      </button>
    </div>
  );
}
