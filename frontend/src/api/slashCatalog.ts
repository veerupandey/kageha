import type { SlashCommand, WebUiCapabilities } from "./types";

/** WebUI-capable slash commands only (no CLI-only stubs). */
export const SLASH_COMMANDS: SlashCommand[] = [
  {
    id: "plan",
    label: "/plan",
    title: "Plan",
    description: "Plan — clarify, research, then Build",
    kind: "mode",
  },
  {
    id: "goal",
    label: "/goal",
    title: "Goal",
    description: "Goal — keep working toward a saved objective",
    kind: "mode",
  },
  {
    id: "normal",
    label: "/normal",
    title: "Normal",
    description: "Normal mode — standard chat",
    kind: "mode",
  },
  {
    id: "new",
    label: "/new",
    title: "New task",
    description: "Start a separate task while current work continues",
    kind: "multitask",
  },
  {
    id: "ask",
    label: "/ask",
    description: "Ask before risky tools",
    kind: "prefs",
  },
  {
    id: "auto",
    label: "/auto",
    description: "Auto-approve risky tools",
    kind: "prefs",
  },
  {
    id: "permissions",
    label: "/permissions",
    description: "Show ask/auto/full tool approval mode",
    kind: "prefs",
  },
  {
    id: "permissions-ask",
    label: "/permissions ask",
    description: "Ask before risky tools",
    kind: "prefs",
  },
  {
    id: "permissions-auto",
    label: "/permissions auto",
    description: "Auto-approve risky tools",
    kind: "prefs",
  },
  {
    id: "permissions-full",
    label: "/permissions full",
    description: "Auto-approve + sandbox network",
    kind: "prefs",
  },
  {
    id: "attach",
    label: "/attach",
    description: "Attach files from disk (or drop / paste in composer)",
    kind: "prefs",
  },
  {
    id: "files",
    label: "/files",
    description: "Same as /attach — pick files for this message",
    kind: "prefs",
  },
  {
    id: "artifacts",
    label: "/artifacts",
    description: "Open canvas for images, video, PDFs, and files",
    kind: "prefs",
  },
  {
    id: "model",
    label: "/model",
    description: "Focus model override",
    kind: "prefs",
  },
  {
    id: "browser",
    label: "/browser",
    description: "Browser backend status / select",
    kind: "browser",
  },
  {
    id: "browser-list",
    label: "/browser list",
    description: "List browser backends",
    kind: "browser",
  },
  {
    id: "browser-comet",
    label: "/browser comet",
    description: "Use logged-in Comet CDP",
    kind: "browser",
  },
  {
    id: "browser-lightpanda",
    label: "/browser lightpanda",
    description: "Private browser session",
    kind: "browser",
  },
  {
    id: "browser-chromium",
    label: "/browser chromium",
    description: "Warm Chromium headless pool",
    kind: "browser",
  },
  {
    id: "browser-headless",
    label: "/browser headless",
    description: "Interactive headless Chromium",
    kind: "browser",
  },
  {
    id: "browser-diagnose",
    label: "/browser diagnose",
    description: "Console, network, and performance diagnostics for a URL",
    kind: "browser",
  },
  {
    id: "research",
    label: "/research",
    description: "Blink research (native, no LLM loop)",
    kind: "browser",
  },
  {
    id: "research-flash",
    label: "/research flash",
    description: "Research · HTTP flash depth",
    kind: "browser",
  },
  {
    id: "research-standard",
    label: "/research standard",
    description: "Research · headless JS enrich",
    kind: "browser",
  },
  {
    id: "research-deep",
    label: "/research deep",
    description: "Research · deep multi-pass",
    kind: "browser",
  },
  {
    id: "computer",
    label: "/computer",
    usage: "/computer <task>",
    title: "computer_use",
    description: "Drive this Mac · type a task after",
    kind: "skill",
  },
  {
    id: "computer-status",
    label: "/computer status",
    description: "Driver, permissions, pack",
    kind: "computer",
  },
  {
    id: "machine",
    label: "/machine",
    description: "Give this session an isolated Linux computer",
    kind: "machine",
  },
  {
    id: "machine-status",
    label: "/machine status",
    description: "Show the chat’s Docker workstation status",
    kind: "machine",
  },
  {
    id: "machine-doctor",
    label: "/machine doctor",
    description: "Probe Docker and the machine image",
    kind: "machine",
  },
  {
    id: "machine-stop",
    label: "/machine stop",
    description: "Return this chat to local execution",
    kind: "machine",
  },
];

function normalizeCommand(raw: unknown): SlashCommand | null {
  if (!raw || typeof raw !== "object") return null;
  const c = raw as Record<string, unknown>;
  const id = String(c.id || "").trim();
  if (!id) return null;
  const label = String(c.label || `/${id}`).trim() || `/${id}`;
  return {
    id,
    label,
    description: String(c.description || ""),
    kind: String(c.kind || "prefs"),
    title: c.title != null ? String(c.title) : undefined,
    usage: c.usage != null ? String(c.usage) : undefined,
  };
}

/** Use the live server catalog as authority; fallback only while offline. */
const LEAN_UI_EXCLUDED_IDS = new Set([
  "tabs",
  "labs",
  "best-of-n",
  "review",
  "memory",
  "spec",
  "workbench",
  // Internal skill name — advertised as /computer only.
  "skill-computer_use",
  "computer_use",
]);

export function mergeServerCatalog(
  serverCommands: unknown[] | null | undefined,
  fallback: SlashCommand[] = SLASH_COMMANDS,
): SlashCommand[] {
  if (!Array.isArray(serverCommands) || !serverCommands.length) {
    return fallback.slice();
  }
  const byId = new Map<string, SlashCommand>();
  for (const raw of serverCommands) {
    const cmd = normalizeCommand(raw);
    if (!cmd || LEAN_UI_EXCLUDED_IDS.has(cmd.id) || cmd.kind === "labs") continue;
    byId.set(cmd.id, cmd);
  }
  for (const id of LEAN_UI_EXCLUDED_IDS) byId.delete(id);
  return [...byId.values()];
}

/** Filter catalog by live capability probes.
 * Unknown/null capabilities keep commands visible; only hide when explicitly false.
 */
export function filterSlashByCapabilities(
  commands: SlashCommand[],
  capabilities: WebUiCapabilities,
): SlashCommand[] {
  let cmds = commands.slice();
  if (capabilities.slashCatalogApi) {
    return cmds.filter((c) => {
      if (!c?.id) return false;
      if (c.kind === "browser") {
        if (c.id === "comet") return capabilities.cometApi !== false;
        return capabilities.browserApi !== false;
      }
      if (c.kind === "computer") return capabilities.computerApi !== false;
      if (c.kind === "machine") return capabilities.machineApi !== false;
      // /computer skill alias + /computer_use follow computer pack capability
      if (
        c.id === "computer" ||
        c.id === "skill-computer_use" ||
        c.title === "computer_use"
      ) {
        return capabilities.computerApi !== false;
      }
      return true;
    });
  }
  if (capabilities.cometApi) {
    const hasComet = cmds.some((c) => c.id === "comet");
    if (!hasComet) {
      cmds.push({
        id: "comet",
        label: "/comet",
        description: "Logged-in browser · start / status",
        kind: "browser",
      });
    }
  }
  if (capabilities.browserApi === false) {
    cmds = cmds.filter((c) => c.kind !== "browser" || c.id === "comet");
  }
  if (capabilities.computerApi === false) {
    cmds = cmds.filter(
      (c) =>
        c.kind !== "computer" &&
        c.id !== "computer" &&
        c.id !== "skill-computer_use" &&
        c.title !== "computer_use",
    );
  }
  if (capabilities.machineApi === false) {
    cmds = cmds.filter((c) => c.kind !== "machine" && c.id !== "machine");
  }
  return cmds;
}
