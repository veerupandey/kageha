import * as Dialog from "@radix-ui/react-dialog";
import { lazy, Suspense, type ComponentType, type ReactNode } from "react";
import { Icon, type IconProps } from "../lib/icons";
import { useAppStore } from "../store";
import { ThemePicker } from "./shared/ThemePicker";
import { ModelsSettings } from "./ProviderSetupPanel";
import { DecisionSettings } from "./DecisionSettings";
import { PersistenceSettings } from "./PersistenceSettings";
import { AgentPerformance } from "./AgentPerformance";
import { SearchSetupPanel } from "./SearchSetupPanel";
import { MediaSettings } from "./MediaSettings";
import { ChannelsSettings } from "./ChannelsSettings";
import type { SettingsTab } from "../store/types";
import { cn } from "../lib/cn";

const IntegrationsSettings = lazy(() => import("./IntegrationsSettings").then((module) => ({ default: module.IntegrationsSettings })));
const ExtensionsPanel = lazy(() => import("./ExtensionsPanel").then((module) => ({ default: module.ExtensionsPanel })));

const NAV: {
  id: SettingsTab;
  label: string;
  icon: ComponentType<IconProps>;
  group: string;
  title: string;
  description: string;
}[] = [
  {
    id: "persistence", label: "Persistence", icon: Icon.Brain, group: "Agent behavior",
    title: "Persistence", description: "Choose how long agents can keep working through recoverable problems.",
  },
  {
    id: "performance", label: "Performance", icon: Icon.Activity, group: "Agent behavior",
    title: "Agent performance", description: "Compare verified task durations over time.",
  },
  {
    id: "decisions", label: "Decisions", icon: Icon.Brain, group: "Agent behavior",
    title: "Decision models", description: "Choose how Kageha makes frequent judgments, independently of your conversation model.",
  },
  {
    id: "models",
    label: "Models",
    icon: Icon.Brain,
    group: "Connect",
    title: "Models",
    description:
      "Connect OpenCode Go, Z.AI, Gemini, OpenAI, and Codex. Models with live web search can answer without a separate search tool.",
  },
  {
    id: "search",
    label: "Search",
    icon: Icon.Globe,
    group: "Connect",
    title: "Search",
    description:
      "Pick a tool search engine and paste keys. Live-web models keep built-in search — the picker still drives research and citations.",
  },
  {
    id: "media",
    label: "Media",
    icon: Icon.File,
    group: "Connect",
    title: "Media",
    description:
      "Choose the default image provider for chat. The composer chip can override it per session.",
  },
  {
    id: "integrations",
    label: "Integrations",
    icon: Icon.Plug,
    group: "Connect",
    title: "Integrations",
    description: "Connect accounts and choose what your agents can access.",
  },
  {
    id: "channels",
    label: "Channels",
    icon: Icon.Message,
    group: "Connect",
    title: "Messaging channels",
    description:
      "Bring your agent to Telegram, WhatsApp, and phone calls (Twilio). Each channel shares the same threads, memory, and approvals as this Web UI.",
  },
  {
    id: "extensions", label: "Extensions", icon: Icon.Plug, group: "Tools",
    title: "Extensions", description: "Manage plugins, skills, MCP connections, hooks, and mods.",
  },
  {
    id: "appearance",
    label: "Appearance",
    icon: Icon.Sun,
    group: "Workspace",
    title: "Appearance",
    description: "Theme, density, and motion for this workspace.",
  },
];

export function SettingsDialog({
  tab,
  onClose,
  onTabChange,
}: {
  tab: SettingsTab;
  onClose: () => void;
  onTabChange: (tab: SettingsTab) => void;
}) {
  const prefs = useAppStore((s) => s.prefs);
  const setPrefs = useAppStore((s) => s.setPrefs);
  const theme = prefs.theme;
  const density = prefs.density;
  const reduceMotion = prefs.reduceMotion;
  const current = NAV.find((row) => row.id === tab) || NAV[0];
  const groups = NAV.reduce<string[]>((acc, row) => {
    if (!acc.includes(row.group)) acc.push(row.group);
    return acc;
  }, []);

  return (
    <Dialog.Root
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="ka-settings-overlay" />
        <Dialog.Content className="ka-settings-panel" aria-label="Settings dialog">
          <div className="ka-settings-shell">
            <nav className="ka-settings-nav" aria-label="Settings sections">
              <div className="ka-settings-nav-kicker">
                <Icon.Settings size={14} className="text-accent" />
                <span>Settings</span>
              </div>
              {groups.map((group) => (
                <div key={group} className="ka-settings-nav-group">
                  <p className="ka-settings-nav-label">{group}</p>
                  {NAV.filter((row) => row.group === group).map((row) => (
                    <button
                      key={row.id}
                      type="button"
                      className="ka-settings-nav-btn"
                      data-active={tab === row.id}
                      aria-current={tab === row.id ? "page" : undefined}
                      onClick={() => onTabChange(row.id)}
                    >
                      <row.icon size={15} className="shrink-0" />
                      <span>{row.label}</span>
                    </button>
                  ))}
                </div>
              ))}
            </nav>

            <div className="ka-settings-main">
              <header className="ka-settings-header">
                <div>
                  <Dialog.Title className="text-lg font-semibold tracking-tight text-ink sm:text-xl">
                    {current.title}
                  </Dialog.Title>
                  <Dialog.Description className="mt-1 text-xs leading-relaxed text-muted">
                    {current.description}
                  </Dialog.Description>
                </div>
                <div className="flex items-center gap-2">
                  <Dialog.Close
                    className="flex h-8 w-8 items-center justify-center rounded-lg border border-line/60 bg-surface text-faint transition-all hover:border-line hover:bg-line/40 hover:text-ink focus-visible:outline-2 focus-visible:outline-accent"
                    aria-label="Close settings"
                  >
                    <Icon.Close size={15} />
                  </Dialog.Close>
                </div>
              </header>

              <div key={tab} className="ka-settings-body">
                {tab === "persistence" ? <PersistenceSettings /> : null}
                {tab === "performance" ? <AgentPerformance /> : null}
                {tab === "decisions" ? <DecisionSettings /> : null}
                {tab === "models" ? <ModelsSettings /> : null}
                {tab === "search" ? <SearchSetupPanel /> : null}
                {tab === "media" ? <MediaSettings /> : null}
                {tab === "integrations" ? <Suspense fallback={<p role="status" className="text-sm text-muted">Loading integrations…</p>}><IntegrationsSettings /></Suspense> : null}
                {tab === "channels" ? <ChannelsSettings /> : null}
                {tab === "extensions" ? <Suspense fallback={<p role="status">Loading extensions…</p>}><ExtensionsPanel /></Suspense> : null}
                {tab === "appearance" ? (
                  <div className="space-y-5">
                    <ThemePicker value={theme} onChange={(value) => setPrefs({ theme: value })} />

                    <SettingRow
                      label="Density"
                      sublabel={density === "comfortable" ? "More space between rows" : "Tighter lists and chrome"}
                    >
                      <div className="inline-flex rounded-lg border border-line bg-canvas p-0.5">
                        {(["comfortable", "compact"] as const).map((value) => (
                          <button
                            key={value}
                            type="button"
                            className={cn(
                              "rounded-md px-2.5 py-1 text-xs font-medium capitalize transition-all",
                              density === value ? "bg-surface text-ink shadow-xs" : "text-muted hover:text-ink",
                            )}
                            onClick={() => setPrefs({ density: value })}
                          >
                            {value}
                          </button>
                        ))}
                      </div>
                    </SettingRow>

                    <SettingRow
                      label="Reduce motion"
                      sublabel="Limit animation for the interface"
                    >
                      <ToggleSwitch checked={reduceMotion} onChange={(v) => setPrefs({ reduceMotion: v })} />
                    </SettingRow>

                    <div className="flex flex-wrap items-center gap-3 pt-2 text-[0.68rem] text-faint">
                      <span className="flex items-center gap-1.5">
                        <kbd className="rounded border border-line/70 bg-canvas/60 px-1.5 py-0.5 font-mono text-[0.6rem] text-faint">⌘K</kbd>
                        <span>Command palette</span>
                      </span>
                      <span className="text-line">·</span>
                      <span className="flex items-center gap-1.5">
                        <kbd className="rounded border border-line/70 bg-canvas/60 px-1.5 py-0.5 font-mono text-[0.6rem] text-faint">⌘N</kbd>
                        <span>New thread</span>
                      </span>
                      <span className="text-line">·</span>
                      <span className="flex items-center gap-1.5">
                        <kbd className="rounded border border-line/70 bg-canvas/60 px-1.5 py-0.5 font-mono text-[0.6rem] text-faint">⇧↵</kbd>
                        <span>New line</span>
                      </span>
                    </div>
                  </div>
                ) : null}
              </div>
            </div>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function SettingRow({
  label,
  sublabel,
  children,
}: {
  label: string;
  sublabel?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 rounded-xl border border-line/60 bg-canvas/30 px-4 py-3.5 transition-colors hover:border-line hover:bg-canvas/50">
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium text-ink">{label}</p>
        {sublabel ? <p className="mt-0.5 text-xs text-muted leading-relaxed">{sublabel}</p> : null}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

function ToggleSwitch({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      className={cn(
        "relative inline-flex h-6 w-11 shrink-0 cursor-pointer items-center rounded-full p-0.5 transition-colors duration-200 focus-visible:outline-2 focus-visible:outline-accent",
        checked ? "bg-accent" : "bg-line-strong",
      )}
      onClick={() => onChange(!checked)}
    >
      <span
        className={cn(
          "inline-block h-5 w-5 rounded-full bg-white shadow-xs transition-transform duration-200",
          checked ? "translate-x-5" : "translate-x-0",
        )}
      />
    </button>
  );
}
