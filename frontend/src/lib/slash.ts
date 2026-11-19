import { api } from "../api/client";
import type { AgentMode, SlashCommand } from "../api/types";
import { isComputerAdminSlash, isMachineAdminSlash } from "../store/helpers";
import { useAppStore } from "../store";

export interface TokenContext {
  start: number;
  end: number;
  query: string;
  token?: string;
}

const AGENT_MODES: AgentMode[] = ["normal", "plan", "goal"];

const SLASH_PRIMARY_IDS = [
  "plan",
  "goal",
  "normal",
  "new",
  "ask",
  "auto",
  "model",
  "browser",
  "research",
  "comet",
  "computer",
  "machine",
];

const SLASH_KIND_ORDER = [
  "mode",
  "multitask",
  "prefs",
  "browser",
  "computer",
  "machine",
  "project",
  "skill",
];

const SLASH_ID_ORDER = Object.fromEntries(
  SLASH_PRIMARY_IDS.map((id, i) => [id, i]),
);

export type SlashApplyResult = "attach" | "focus-model" | void;

function slashCommandTitle(cmd: SlashCommand): string {
  if (cmd.id === "computer") return "Computer";
  if (cmd.title && cmd.id !== "skill-computer_use") return String(cmd.title);
  const id = String(cmd.id || "");
  if (cmd.kind === "mode" && id) {
    return id[0].toUpperCase() + id.slice(1);
  }
  if (id === "multitask" || id === "new" || id === "task") return "New task";
  const label = String(cmd.label || "")
    .replace(/^\//, "")
    .trim();
  if (!label) return id;
  return label
    .split(/[\s./-]+/)
    .filter(Boolean)
    .map((p) => p[0].toUpperCase() + p.slice(1))
    .join(" ");
}

function slashCommandRank(cmd: SlashCommand): number {
  const id = String(cmd?.id || "");
  if (Object.prototype.hasOwnProperty.call(SLASH_ID_ORDER, id)) {
    return SLASH_ID_ORDER[id];
  }
  const ka = SLASH_KIND_ORDER.indexOf(String(cmd?.kind || ""));
  return 1000 + (ka < 0 ? 99 : ka);
}

const SLASH_KIND_TITLES: Record<string, string> = {
  mode: "Modes",
  multitask: "Tasks",
  browser: "Browser & research",
  computer: "Computer use",
  machine: "Task machine",
  skill: "Skills",
  project: "Project",
  prefs: "Settings & actions",
};

export function slashCommandGroup(cmd: SlashCommand): string {
  return SLASH_KIND_TITLES[String(cmd.kind || "")] || "Other";
}

export function slashCommandUsage(cmd: SlashCommand): string {
  if (cmd.usage) return cmd.usage;
  if (cmd.id === "computer") return "/computer <task>";
  if (cmd.id === "browser-diagnose") return "/browser diagnose <url>";
  if (cmd.id.startsWith("research")) return `${cmd.label} <query>`;
  return cmd.label;
}

/** First token shown as the Cursor-style slash badge, e.g. `/plan`. */
export function slashCommandToken(cmd: SlashCommand): string {
  const usage = slashCommandUsage(cmd);
  const token = usage.trim().split(/\s+/)[0];
  if (token.startsWith("/")) return token;
  const label = String(cmd.label || "").trim().split(/\s+/)[0];
  if (label.startsWith("/")) return label;
  return `/${cmd.id}`;
}

/** `/token` at caret — returns start/end/query or null. */
export function getSlashContext(
  text: string,
  caret: number,
  commands?: SlashCommand[],
): TokenContext | null {
  const value = String(text || "");
  const pos = Math.max(0, Math.min(Number(caret) || 0, value.length));
  const before = value.slice(0, pos);
  // Multi-word commands stay open only while completing a catalog label;
  // ordinary arguments (e.g. /research explain gravity) remain plain text.
  if (commands) {
    const multi = before.match(/(^|\s)(\/[a-z0-9_./-]+(?: +[a-z0-9_./-]*)+)$/i);
    if (multi && commands.some((cmd) => cmd.label.toLowerCase().startsWith(multi[2].toLowerCase()))) {
      return { start: before.length - multi[2].length, end: pos, token: multi[2], query: multi[2].slice(1).toLowerCase() };
    }
  }
  const m = before.match(/(^|[\s\n])(\/[a-z0-9_./-]*)$/i);
  if (!m) return null;
  const token = m[2];
  return {
    start: before.length - token.length,
    end: pos,
    token,
    query: token.slice(1).toLowerCase(),
  };
}

/** `@token` at caret — returns start/end/query or null. */
export function getAtContext(
  text: string,
  caret: number,
  sigil: "@" | "&" = "@",
): TokenContext | null {
  const value = String(text || "");
  const pos = Math.max(0, Math.min(Number(caret) || 0, value.length));
  const before = value.slice(0, pos);
  const m = before.match(sigil === "@" ? /(^|\s)(@[^\s]*)$/ : /(^|\s)(&[^\s]*)$/);
  if (!m) return null;
  const token = m[2];
  return {
    start: before.length - token.length,
    end: pos,
    token,
    query: token.slice(1),
  };
}

/** Filter slash catalog by typed query (id, label, title, description). */
export function filterSlashCommands(
  cmds: SlashCommand[],
  query: string,
): SlashCommand[] {
  const list = Array.isArray(cmds) ? cmds : [];
  const q = String(query || "")
    .toLowerCase()
    .trim();
  let matched: SlashCommand[];
  if (!q) {
    matched = list.slice();
  } else {
    matched = list.filter((c) => slashMatchScore(c, q) < 99);
  }
  return matched.slice().sort((a, b) => {
    const sa = slashMatchScore(a, q);
    const sb = slashMatchScore(b, q);
    if (sa !== sb) return sa - sb;
    const ra = slashCommandRank(a);
    const rb = slashCommandRank(b);
    if (ra !== rb) return ra - rb;
    return String(a.label || "").localeCompare(String(b.label || ""));
  });
}

/** Lower is better. 99 = no match. Prefix beats substring so `/p` → Plan. */
export function slashMatchScore(cmd: SlashCommand, query: string): number {
  const q = String(query || "")
    .toLowerCase()
    .trim();
  if (!q) return 0;
  const id = String(cmd.id || "").toLowerCase();
  const label = String(cmd.label || "")
    .replace(/^\//, "")
    .toLowerCase();
  const title = slashCommandTitle(cmd).toLowerCase();
  const desc = String(cmd.description || "").toLowerCase();
  if (id === q || label === q) return 0;
  if (id.startsWith(q) || label.startsWith(q)) return 1;
  if (title.startsWith(q)) return 2;
  if (id.includes(q) || label.includes(q) || title.includes(q)) return 8;
  if (desc.includes(q)) return 12;
  return 99;
}

export function groupSlashItems(
  cmds: SlashCommand[],
): { group: string; items: SlashCommand[] }[] {
  const order: string[] = [];
  const map = new Map<string, SlashCommand[]>();
  for (const cmd of cmds) {
    const group = slashCommandGroup(cmd);
    const list = map.get(group);
    if (!list) {
      map.set(group, [cmd]);
      order.push(group);
    } else {
      list.push(cmd);
    }
  }
  return order.map((group) => ({ group, items: map.get(group) || [] }));
}

function replaceTokenInDraft(start: number, end: number, insert: string) {
  const store = useAppStore.getState();
  const draft = store.draft;
  const head = draft.slice(0, start);
  const after = draft.slice(end);
  store.setDraft(head + insert + after);
}

function toastApiResult(
  store: ReturnType<typeof useAppStore.getState>,
  data: { message?: string; status?: string },
  fallback: string,
) {
  const status = data.status != null ? String(data.status) : "";
  const message = data.message != null ? String(data.message) : "";
  let text = message || status || fallback;
  if (status && message && !message.toLowerCase().includes(status.toLowerCase())) {
    text = `${status} · ${message}`;
  }
  if (text.length > 180) text = `${text.slice(0, 177)}…`;
  store.showToast(text);
}

async function postBrowser(command: string) {
  const store = useAppStore.getState();
  const cmd = String(command || "/browser").trim();
  // Don't auto-send (avoids minting a session + recurse via sendMessage).
  if (store.capabilities.browserApi === false) {
    store.setDraft(cmd);
    store.showToast("Browser · API unavailable — press Send");
    return;
  }
  store.showToast(cmd.startsWith("/research") ? "Research…" : "Browser…");
  try {
    const data = await api<{ message?: string; status?: string; ok?: boolean }>(
      "/api/browser",
      {
        method: "POST",
        body: JSON.stringify({ command: cmd }),
      },
    );
    const full = String(data.message || data.status || "Done");
    if (store.sessionId) store.appendLocalMessage("assistant", full);
    toastApiResult(store, data, cmd.startsWith("/research") ? "Research" : "Browser");
  } catch (err) {
    const msg = err instanceof Error ? err.message : `Browser failed: ${err}`;
    if (store.sessionId) store.appendLocalMessage("assistant", msg);
    store.showToast(msg.slice(0, 180));
  }
}

async function postComputer(command: string) {
  const store = useAppStore.getState();
  const cmd = String(command || "/computer").trim();
  if (store.capabilities.computerApi === false) {
    store.setDraft(cmd);
    store.showToast("Computer · API unavailable — press Send");
    return;
  }
  store.showToast("Computer…");
  try {
    const data = await api<{ message?: string; status?: string; ok?: boolean }>(
      "/api/computer",
      {
        method: "POST",
        body: JSON.stringify({ command: cmd, session_id: store.sessionId || "" }),
      },
    );
    const full = String(data.message || data.status || "Done");
    // Only append into an existing chat — never mint a session for status/tips.
    if (store.sessionId) {
      store.appendLocalMessage("assistant", full);
    }
    toastApiResult(store, data, "Computer");
  } catch (err) {
    const msg = err instanceof Error ? err.message : `Computer failed: ${err}`;
    if (store.sessionId) store.appendLocalMessage("assistant", msg);
    store.showToast(msg.slice(0, 180));
  }
}

async function postMachine(cmd: string) {
  const store = useAppStore.getState();
  store.showToast("Machine…");
  try {
    const data = await api<{ message?: string; status?: string; ok?: boolean }>(
      "/api/machine",
      {
        method: "POST",
        body: JSON.stringify({
          command: cmd,
          session_id: store.sessionId || "",
        }),
      },
    );
    const full = String(data.message || data.status || "Done");
    if (store.sessionId) {
      store.appendLocalMessage("assistant", full);
    }
    toastApiResult(store, data, "Machine");
    const low = cmd.toLowerCase();
    if (low === "/machine stop" && store.sessionId === useAppStore.getState().sessionId) {
      void store.setMachineActive(false, { synced: true });
    }
  } catch (err) {
    const msg = err instanceof Error ? err.message : `Machine failed: ${err}`;
    if (store.sessionId) store.appendLocalMessage("assistant", msg);
    store.showToast(msg.slice(0, 180));
  }
}

/** Handle native admin slash without creating a chat session. */
export function dispatchNativeAdminSlash(command: string): boolean {
  const cmd = String(command || "").trim();
  const low = cmd.toLowerCase();
  // Admin only — `/computer <task>` and `/machine <task>` belong in chat.
  if (isComputerAdminSlash(cmd)) {
    void postComputer(cmd);
    return true;
  }
  if (isMachineAdminSlash(cmd)) {
    void postMachine(cmd);
    return true;
  }
  if (
    low === "/browser" ||
    low.startsWith("/browser ") ||
    low === "/comet" ||
    low.startsWith("/comet ")
  ) {
    const browserCmd = low.startsWith("/comet")
      ? `/browser ${cmd.replace(/^\//, "")}`
      : cmd;
    void postBrowser(browserCmd);
    return true;
  }
  return false;
}

type StoreSlice = ReturnType<typeof useAppStore.getState>;

/**
 * Dispatch a slash command against the zustand store.
 * Returns 'attach' | 'focus-model' for UI side-effects the caller should handle.
 */
export function applySlashCommand(
  cmd: SlashCommand,
  storeOrOpts?: StoreSlice | { start?: number; end?: number },
  maybeOpts?: { start?: number; end?: number },
): SlashApplyResult {
  if (!cmd) return;
  const looksLikeStore =
    storeOrOpts != null &&
    typeof storeOrOpts === "object" &&
    "setAgentMode" in storeOrOpts;
  const store = looksLikeStore
    ? (storeOrOpts as StoreSlice)
    : useAppStore.getState();
  const opts = looksLikeStore
    ? maybeOpts
    : (storeOrOpts as { start?: number; end?: number } | undefined);
  const start = opts?.start ?? 0;
  const end = opts?.end ?? store.draft.length;
  const id = cmd.id;
  const clearToken = () => {
    const draft = store.draft;
    const head = draft.slice(0, start);
    const after = draft.slice(end);
    store.setDraft(head + after);
  };

  if (cmd.kind === "mode" && AGENT_MODES.includes(id as AgentMode)) {
    clearToken();
    store.setAgentMode(id as AgentMode);
    if (id === "normal") store.showToast("Normal mode");
    else store.showToast(`${id[0].toUpperCase()}${id.slice(1)} mode`);
    return;
  }

  if (id === "new" || id === "task") {
    clearToken();
    store.clearComposerChip({ resetMode: false });
    if (store.sessionId) {
      void store
        .newSession({ parallel: true })
        .then(() => store.showToast("New task opened · previous work continues"))
        .catch((err: Error) =>
          store.showToast(err.message || String(err)),
        );
    } else {
      store.setComposerChip("multitask", "multitask");
      store.showToast("Send your first task, then use /new to start another");
    }
    return;
  }

  if (id === "ask" || id === "permissions-ask") {
    clearToken();
    void store.setPermissionsMode("ask");
    return;
  }

  if (id === "auto" || id === "permissions-auto") {
    clearToken();
    void store.setPermissionsMode("auto");
    return;
  }

  if (id === "permissions-full") {
    clearToken();
    void store.setPermissionsMode("full");
    return;
  }

  if (id === "permissions") {
    clearToken();
    const scope = store.permissionScope || (store.autoApprove ? "session" : "ask");
    store.showToast(
      scope === "full"
        ? "Permissions · full (ask|auto|full)"
        : scope === "session"
          ? "Permissions · auto (ask|auto|full)"
          : "Permissions · ask (ask|auto|full)",
    );
    return;
  }

  if (id === "artifacts") {
    clearToken();
    store.setCanvasOpen(true);
    void store.refreshArtifacts();
    store.showToast("Canvas · session files");
    return;
  }

  if (
    id === "labs" ||
    id === "best-of-n" ||
    id === "review" ||
    id === "memory" ||
    id === "spec"
  ) {
    clearToken();
    store.showToast("Not available in lean WebUI");
    return;
  }

  if (id === "attach" || id === "files") {
    clearToken();
    store.showToast("Choose files · or drop / paste into the composer");
    return "attach";
  }

  if (id === "model") {
    clearToken();
    store.showToast(
      store.capabilities.modelApi !== false
        ? "Model override · API available"
        : "Model override",
    );
    return "focus-model";
  }

  if (id === "comet") {
    clearToken();
    void postBrowser("/browser comet start");
    return;
  }

  if (cmd.kind === "browser" || id === "browser" || id.startsWith("browser-") || id.startsWith("research")) {
    clearToken();
    const label = String(cmd.label || "").trim();
    if (label === "/research" || label.startsWith("/research")) {
      const parts = label.split(/\s+/).filter(Boolean);
      if (
        parts.length === 1 ||
        (parts.length === 2 &&
          ["flash", "standard", "deep"].includes(parts[1].toLowerCase()))
      ) {
        const prefix =
          parts.length === 1 ? "/research flash " : `${label} `;
        store.setDraft(prefix + store.draft.replace(/^\s*/, ""));
        store.showToast("Research · type your query, then send");
        return;
      }
    }
    void postBrowser(label || "/browser");
    return;
  }

  // Admin pack commands only (kind: computer). Primary /computer is kind: skill.
  if (cmd.kind === "computer" || id.startsWith("computer-")) {
    clearToken();
    const label = String(cmd.label || "/computer").trim();
    void postComputer(label);
    return;
  }

  if (cmd.kind === "machine" || id.startsWith("machine-")) {
    clearToken();
    const label = String(cmd.label || "/machine").trim();
    if (id === "machine") {
      replaceTokenInDraft(start, end, "/machine ");
      store.showToast("Task machine · type a task after /machine, then send");
      return;
    }
    void postMachine(label);
    return;
  }

  if (cmd.kind === "project" && cmd.label) {
    replaceTokenInDraft(start, end, `${cmd.label} `);
    store.showToast("Project recipe · send to expand");
    return;
  }

  if (cmd.kind === "skill" && cmd.label) {
    replaceTokenInDraft(start, end, `${cmd.label} `);
    const skillName = cmd.title || cmd.id.replace(/^skill-/, "");
    store.showToast(
      id === "computer" || skillName === "computer_use"
        ? "Computer-use · type a task after /computer, then send"
        : `Skill · ${skillName} (explicit)`,
    );
    return;
  }

  // Fallback: insert the slash label into the composer.
  if (cmd.label) {
    replaceTokenInDraft(start, end, `${cmd.label} `);
  }
}

export { slashCommandTitle, slashCommandRank };
