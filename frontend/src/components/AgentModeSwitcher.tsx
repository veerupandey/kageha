import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { cn } from "../lib/cn";
import { useAppStore } from "../store";
import { AGENT_MODES } from "../store/helpers";

export function AgentModeSwitcher({ compact = false }: { compact?: boolean } = {}) {
  const agentMode = useAppStore((s) => s.agentMode);
  const setAgentMode = useAppStore((s) => s.setAgentMode);
  const modeLabel = agentMode[0].toUpperCase() + agentMode.slice(1);

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          className={cn(
            "rounded-md px-2 py-1 text-xs font-medium transition-colors",
            agentMode === "normal"
              ? compact
                ? "bg-[var(--color-accent-soft)] px-2.5 py-1 text-accent"
                : "text-muted hover:bg-line/70 hover:text-ink"
              : "bg-accent-soft text-accent",
          )}
          aria-label="Agent mode"
          aria-pressed={agentMode !== "normal"}
        >
          <span className="inline-flex items-center gap-1.5">
            {agentMode !== "normal" && (
              <span className="h-1.5 w-1.5 rounded-full bg-accent" aria-hidden="true" />
            )}
            {modeLabel}
            {!compact && agentMode !== "normal" && (
              <span className="rounded-full bg-accent/15 px-1.5 text-[0.6rem] uppercase tracking-wide">
                Active
              </span>
            )}
          </span>
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className="z-50 min-w-40 rounded-lg border border-line bg-surface p-1 shadow-lg"
          sideOffset={4}
        >
          {AGENT_MODES.map((mode) => (
            <DropdownMenu.Item
              key={mode}
              className={cn(
                "cursor-pointer rounded-md px-2.5 py-1.5 text-sm outline-none data-[highlighted]:bg-accent-soft",
                agentMode === mode && "font-medium text-accent",
              )}
              onSelect={() => setAgentMode(mode)}
            >
              {mode[0].toUpperCase() + mode.slice(1)}
            </DropdownMenu.Item>
          ))}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
