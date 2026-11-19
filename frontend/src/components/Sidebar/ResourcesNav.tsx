import { useState, type ComponentType } from "react";
import { Icon, type IconProps } from "../../lib/icons";
import { useAppStore } from "../../store";
import { SidebarSection } from "./SidebarSection";

const RESOURCES: { id: string; label: string; Icon: ComponentType<IconProps> }[] = [
  { id: "knowledge", label: "Knowledge", Icon: Icon.Memories },
  { id: "agent_network", label: "Agent Network", Icon: Icon.Network },
];

interface ResourcesNavProps {
  onNavigate?: (id: string) => void;
  knowledgeEnabled?: boolean;
}

export function ResourcesNav({ onNavigate, knowledgeEnabled = true }: ResourcesNavProps) {
  const resources = knowledgeEnabled ? RESOURCES : RESOURCES.filter((item) => item.id !== "knowledge");
  const peerPendingCount = useAppStore((s) => s.peerPendingCount);
  const [collapsed, setCollapsed] = useState(() => {
    try { return localStorage.getItem("kageha.sidebar.resources") !== "0"; } catch { return true; }
  });
  const toggle = () => {
    const next = !collapsed;
    setCollapsed(next);
    try { localStorage.setItem("kageha.sidebar.resources", next ? "1" : "0"); } catch { /* private storage */ }
  };
  return (
    <div className="px-3 py-2">
      <SidebarSection label="Resources" count={resources.length} collapsed={collapsed} onToggle={toggle}>
        <ul className="space-y-0.5">
        {resources.map((r) => {
          const ItemIcon = r.Icon;
          return (
            <li key={r.id}>
              <button
                type="button"
                className="ka-sidebar-item w-full"
                onClick={() => onNavigate?.(r.id)}
              >
                <ItemIcon size={16} />
                <span className="flex-1 text-left">{r.label}</span>
                {r.id === "agent_network" && peerPendingCount > 0 ? (
                  <span
                    className="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-accent px-1.5 text-[0.65rem] font-semibold text-white"
                    aria-label={`${peerPendingCount} pending peer request${peerPendingCount === 1 ? "" : "s"}`}
                  >
                    {peerPendingCount}
                  </span>
                ) : null}
              </button>
            </li>
          );
        })}
        </ul>
      </SidebarSection>
    </div>
  );
}
