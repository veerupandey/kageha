import type { ActivityStep } from "../api/types";
import { isShellTool } from "./transcript";

/** Match CLI TransientProgress glyphs. */
export function activityGlyph(label: string): string {
  const low = (label || "").toLowerCase();
  if (low.includes("waiting") || low.includes("approval") || low.includes("awaiting")) {
    return "⏸";
  }
  if (
    low.includes("think") ||
    low.includes("reason") ||
    low.includes("planning") ||
    low.includes("model:")
  ) {
    return "✦";
  }
  if (
    low.includes("step") ||
    low.includes("tool") ||
    low.includes("running") ||
    low.includes("working") ||
    low.includes("action")
  ) {
    return "▸";
  }
  if (low.includes("error") || low.includes("fail") || low.includes("denied")) {
    return "!";
  }
  if (low.includes("done") || low.includes("complete") || low.includes("success")) {
    return "✓";
  }
  return "·";
}

export function activityGlyphClass(label: string): string {
  const g = activityGlyph(label);
  if (g === "⏸") return "text-warn";
  if (g === "✦") return "text-sky-600";
  if (g === "▸") return "text-teal-700";
  if (g === "!") return "text-danger";
  if (g === "✓") return "text-accent";
  return "text-cyan-700";
}

/** Shorten noisy labels toward CLI-friendly status. */
export function friendlyActivityLabel(label: string): string {
  const compact = (label || "").replace(/\s+/g, " ").trim();
  if (!compact) return "";
  const low = compact.toLowerCase();
  if (low === "tool_card") return "Working…";
  if (low.includes("ask_human")) return "Waiting for your answer…";
  if (/^(still\s+)?(working|connecting|starting)[.…]*$/i.test(compact)) return compact;
  if (low.includes("reasoning:")) return "Thinking…";
  if (low.includes("plan ready")) return compact.startsWith("Plan ready") ? compact : "Starting…";
  if (low.includes("planning")) return "Planning…";
  if (low.includes("preparing task")) return "Preparing…";
  if (low.startsWith("accepted")) return "Starting…";
  if (low.includes("checking") || low.includes("verify") || low.includes("progress=")) {
    return "Checking the result…";
  }
  const action = /action:\s*([a-z0-9_]+)/i.exec(compact);
  if (action) return `Running ${action[1]}…`;
  const tools = /tools:\s*([^(\n]+)/i.exec(compact);
  if (tools) {
    const first = tools[1].split(",")[0]?.trim();
    if (first && !first.includes("ask_human")) return `Running ${first}…`;
    return "Working…";
  }
  if (low.includes("model:") && compact.includes("→")) {
    return compact.length > 72 ? `${compact.slice(0, 69)}…` : compact;
  }
  if (low.includes("thinking") || low.includes("model=")) return "Thinking…";
  return compact.length > 96 ? `${compact.slice(0, 93)}…` : compact;
}

/** Hide executor guardrails from plan cards while preserving the user's task. */
export function friendlyPlanStepText(text: string): string {
  const compact = String(text || "").replace(/\s+/g, " ").trim();
  if (!compact) return "";
  const directivePrefixes = [
    /^Do exactly this follow-up with the minimum tools, answer in chat \(no \.md file unless the user asked to save one\), then stop:\s*/i,
    /^Answer this directly in chat\. Use a tool only if you need to look something up first\. Do NOT create files, plans, or todo lists:\s*/i,
    /^Execute this single action with the minimum tools needed, then report the result in chat:\s*/i,
  ];
  for (const prefix of directivePrefixes) {
    const friendly = compact.replace(prefix, "").trim();
    if (friendly !== compact) return friendly || compact;
  }
  return compact;
}

export function activityLineText(step: ActivityStep): string {
  const label = friendlyActivityLabel(step.label);
  const detail = step.detail?.[0]?.replace(/\s+/g, " ").trim();
  if (!detail) return label;
  // Keep one short trailing hint, CLI-style.
  const hint = detail.length > 64 ? `${detail.slice(0, 61)}…` : detail;
  if (label.toLowerCase().includes(hint.toLowerCase().slice(0, 24))) return label;
  return `${label}  ${hint}`;
}

const TIMELINE_NOISE_LABEL =
  /^(thinking|reasoning|reasoned|accepted|turn accepted|starting|continuing|working|planning\b|using |model selected|loaded skill|fast (lane|setup)|execution plan|preparing |checkpoint|thinking with|route selected|environment ready)/i;

const TIMELINE_NOISE_KIND = new Set([
  "thinking_started",
  "reasoning",
  "model_selected",
  "accepted",
  "planning_started",
  "approval_resolved",
  "execution_profile",
  "mcp_skipped",
  "skill_autoload",
]);

const TIMELINE_TOOL_KIND = new Set(["tool_started", "tool_completed", "tool_card", "tool_result"]);

const TRACE_NOISE_KIND = new Set([
  ...TIMELINE_NOISE_KIND,
  ...TIMELINE_TOOL_KIND,
  "heartbeat",
  "running",
  "starting",
  "progress",
  "approval_resolved",
  "contract_compile",
  "checkpoint",
  "budget_review",
]);

const TRACE_NOISE_LABEL =
  /^(thinking|reasoning|reasoned|still working|working|connecting|starting|continuing|planning\b|accepted|turn accepted|using |model selected|fast (lane|setup)|execution plan|preparing |checkpoint|thinking with|route selected|environment ready)/i;

/** Right-panel steps: milestones only (plan ready, approvals, errors) — not thinking/model noise. */
export function isTimelineMilestone(step: ActivityStep): boolean {
  const label = String(step.label || "").trim();
  if (!label) return false;
  const low = label.toLowerCase();
  if (low.startsWith("todos") || low.includes("todo_board")) return false;
  if (TIMELINE_NOISE_LABEL.test(label)) return false;
  const kind = String(step.kind || "").trim();
  if (TIMELINE_NOISE_KIND.has(kind) || TIMELINE_TOOL_KIND.has(kind)) return false;
  return true;
}

/** Monitor trace rows: prompt cache, todos, verification, plan milestones — not live thinking noise. */
export function isTraceStep(step: ActivityStep): boolean {
  const label = String(step.label || "").trim();
  if (!label) return false;
  if (TRACE_NOISE_LABEL.test(label)) return false;
  const kind = String(step.kind || "").trim();
  if (TRACE_NOISE_KIND.has(kind)) return false;
  return true;
}

export function collectTraceSteps(
  messages: Array<{ role?: string; steps?: ActivityStep[] }>,
): ActivityStep[] {
  const out: ActivityStep[] = [];
  for (const message of messages) {
    if (message.role !== "assistant" || !message.steps?.length) continue;
    for (const step of message.steps) {
      if (isTraceStep(step)) out.push(step);
    }
  }
  return out;
}

export type ActivityTarget = "browser" | "computer" | "machine" | null;

export function isBrowserToolName(name: string): boolean {
  const n = String(name || "").trim().toLowerCase();
  return n === "browse" || /(^|\s)browser_[a-z0-9_]+/.test(n);
}

export function isComputerToolName(name: string): boolean {
  const n = String(name || "").trim().toLowerCase();
  return /(^|\s)computer_[a-z0-9_]+/.test(n) || /cua[-_ ]driver/.test(n);
}

export function isMachineShellToolName(name: string): boolean {
  const n = String(name || "").trim().toLowerCase();
  return isShellTool(n) || n === "install_python_packages";
}

/** Composer text that should reveal the owned-browser panel immediately. */
export function wantsLiveBrowser(text: string): boolean {
  const t = String(text || "").toLowerCase();
  if (/(?:^|\s)\/(?:browser|comet)\b/.test(t)) return true;
  if (/\bbrowse\s+to\b|\bnavigate\s+to\b/.test(t)) return true;
  if (/\bopen\s+(https?:\/\/|the\s+(site|page|website|browser))/.test(t)) return true;
  if (/\bhttps?:\/\//.test(t) && /\b(click|fill|type|login|form)\b/.test(t)) return true;
  return false;
}

const MACHINE_ADMIN = new Set([
  "status",
  "doctor",
  "pack",
  "stop",
  "snapshot",
  "resume",
  "help",
  "-h",
  "--help",
  "?",
]);

const MACHINE_PHRASES = [
  "use a machine",
  "use the machine",
  "on a machine",
  "in a machine",
  "isolated computer",
  "isolated linux computer",
  "spin up a sandbox computer",
  "spin up a machine",
  "give this task a computer",
  "give this task a machine",
  "run this in an isolated computer",
  "run this in a machine",
  "use an isolated machine",
];

/** Composer text that should open the isolated desktop in the Live panel. */
export function wantsLiveMachine(text: string): boolean {
  const raw = String(text || "").trim();
  if (!raw) return false;
  if (/(?:^|\s)\/machine\b/i.test(raw)) {
    const parts = raw.split(/\s+/).filter(Boolean);
    const verb = (parts[0] || "").toLowerCase() === "/machine"
      ? (parts[1] || "").toLowerCase()
      : "";
    if (!verb || MACHINE_ADMIN.has(verb)) return false;
    return true;
  }
  const low = raw.toLowerCase();
  if (MACHINE_PHRASES.some((phrase) => low.includes(phrase))) return true;
  if (
    /\b(?:use|start|open|work|operate|run|do|perform|switch(?:\s+this\s+task)?\s+to|move(?:\s+this\s+task)?\s+to)\b[^.\n]{0,80}\b(?:isolated|sandbox(?:ed)?|virtual|linux)\b[^.\n]{0,40}\b(?:desktop|computer|workstation|machine)\b/i.test(
      raw,
    )
  ) {
    return true;
  }
  if (
    /\b(?:isolated|sandbox(?:ed)?|virtual|linux)\b[^.\n]{0,40}\b(?:desktop|computer|workstation|machine)\b[^.\n]{0,50}\b(?:main|primary|default)\b/i.test(
      raw,
    )
  ) {
    return true;
  }
  return false;
}

export function activityTarget(label: string): ActivityTarget {
  const low = String(label || "").toLowerCase();
  if (
    isBrowserToolName(low.split(/\s+/)[0] || "") ||
    low.includes("browser_") ||
    /\bbrowser\b/.test(low) ||
    low.includes("kageha browser") ||
    low.includes("opening page") ||
    low.includes("opening browser") ||
    low.includes("clicking in browser")
  ) {
    return "browser";
  }
  if (
    isMachineShellToolName(low.split(/\s+/)[0] || "") ||
    low.includes("machine_ready") ||
    low.includes("machine_starting") ||
    low.includes("task machine") ||
    /(?:^|\s)\/machine\b/.test(low) ||
    (/\bmachine\b/.test(low) && !low.includes("computer_"))
  ) {
    return "machine";
  }
  if (/loaded skill/.test(low)) {
    return null;
  }
  if (
    isComputerToolName(low.split(/\s+/)[0] || "") ||
    /\bcomputer_[a-z0-9]+\b/.test(low) ||
    low.includes("computer-use") ||
    low.includes("computer use") ||
    low.includes("desktop") ||
    low.includes("opening app") ||
    low.includes("capturing")
  ) {
    return "computer";
  }
  if (
    low.includes("clicking") ||
    low.includes("typing") ||
    low.includes("scrolling")
  ) {
    return "computer";
  }
  return null;
}

/** Extract the first navigable page URL from compact tool arguments/details. */
export function activityUrl(...values: Array<string | undefined>): string | null {
  for (const value of values) {
    const match = String(value || "").match(/https?:\/\/[^\s"'<>\\}]+/i);
    if (match) return match[0].replace(/[),.;]+$/, "");
  }
  return null;
}

/** Turn provider request details into short labels for live surfaces. */
export function modelActivityLabel(detail?: string): string | undefined {
  if (!detail) return undefined;
  if (/^Waiting for \S+ to respond$/i.test(detail)) return "Waiting for model…";
  if (/^Trying fallback model \S+$/i.test(detail)) return "Trying another model…";
  return undefined;
}
