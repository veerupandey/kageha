import { Icon } from "../../lib/icons";

interface SidebarSearchProps {
  query: string;
  onChange: (value: string) => void;
}

export function SidebarSearch({ query, onChange }: SidebarSearchProps) {
  return (
    <div className="px-3 pb-2.5">
      <label className="sr-only" htmlFor="ka-sidebar-search">
        Search
      </label>
      <div className="relative">
        <span className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-faint">
          <Icon.Search size={14} />
        </span>
        <input
          id="ka-sidebar-search"
          type="search"
          className="w-full rounded-lg border border-line/60 bg-surface/50 py-1.5 pl-8 pr-8 text-xs text-ink outline-none placeholder:text-faint/80 transition-all hover:border-line hover:bg-surface/80 focus:border-accent/60 focus:bg-surface focus:ring-2 focus:ring-accent/15"
          placeholder="Search threads & agents…"
          autoComplete="off"
          value={query}
          onChange={(e) => onChange(e.target.value)}
        />
        {query ? (
          <button
            type="button"
            className="absolute right-2 top-1/2 -translate-y-1/2 flex h-4 w-4 items-center justify-center rounded-full text-faint hover:text-ink hover:bg-line/40 transition-colors"
            onClick={() => onChange("")}
            aria-label="Clear search"
          >
            <Icon.Close size={10} />
          </button>
        ) : (
          <kbd className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 rounded border border-line/60 bg-canvas/60 px-1 py-0.5 text-[0.58rem] font-mono text-faint">
            ⌘K
          </kbd>
        )}
      </div>
    </div>
  );
}
