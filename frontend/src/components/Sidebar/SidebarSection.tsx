import type { ReactNode } from "react";
import { Icon } from "../../lib/icons";

export function SidebarSection({ label, count, collapsed, onToggle, children, action }: {
  label: string;
  count?: number;
  collapsed: boolean;
  onToggle: () => void;
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <section aria-label={label} className="ka-sidebar-section">
      <div className="flex items-center gap-1 px-1">
        <button
          type="button"
          className="group flex min-w-0 flex-1 items-center gap-1.5 rounded-md py-1 text-left transition-colors hover:text-ink"
          aria-expanded={!collapsed}
          onClick={onToggle}
        >
          <Icon.Chevron
            size={12}
            className={`shrink-0 text-faint transition-transform duration-200 group-hover:text-muted ${
              collapsed ? "-rotate-90" : "rotate-0"
            }`}
          />
          <span className="truncate text-[0.62rem] font-bold uppercase tracking-[0.14em] text-faint/80 group-hover:text-ink transition-colors">
            {label}
          </span>
          {typeof count === "number" && count > 0 && (
            <span className="rounded-full bg-line/40 px-1.5 py-0.2 text-[0.58rem] font-medium tabular-nums text-faint group-hover:text-muted transition-colors">
              {count}
            </span>
          )}
        </button>
        {action ? <div className="shrink-0 opacity-75 hover:opacity-100 transition-opacity">{action}</div> : null}
      </div>
      {!collapsed && <div className="ka-sidebar-section-content mt-0.5">{children}</div>}
    </section>
  );
}
