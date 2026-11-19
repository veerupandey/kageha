import type { ModelInfo } from "../api/types";

const FAMILY_ORDER = ["GLM", "Gemini", "Kimi", "DeepSeek", "Qwen", "OpenAI", "Codex", "Anthropic"];

const PROVIDER_ORDER = [
  "opencode-go",
  "commandcode",
  "commandcode-messages",
  "kiro",
  "cloudflare",
  "zai",
  "siliconflow",
  "gemini",
  "openai",
  "openai-codex-app",
  "anthropic",
  "antigravity",
];

export const EFFORT_OPTIONS: { value: "" | "low" | "medium" | "high"; label: string }[] = [
  { value: "", label: "Auto" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
];

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : null;
}

export function parseModelInfo(raw: unknown): ModelInfo | null {
  if (typeof raw === "string") {
    const id = raw.trim();
    return id ? { id, label: id, ready: true } : null;
  }
  const row = asRecord(raw);
  if (!row) return null;
  const id = String(row.id || row.name || "").trim();
  if (!id) return null;
  return {
    id,
    free: row.free === true,
    pricing_note: typeof row.pricing_note === "string" ? row.pricing_note : undefined,
    provider: typeof row.provider === "string" ? row.provider : undefined,
    model: typeof row.model === "string" ? row.model : undefined,
    label: typeof row.label === "string" && row.label.trim() ? row.label : id,
    family: typeof row.family === "string" && row.family.trim() ? row.family : undefined,
    ready: row.ready !== false,
    latest: Boolean(row.latest),
    vision:
      Boolean(row.vision) ||
      (Array.isArray(row.capabilities) &&
        row.capabilities.some((item) => item === "vision" || item === "video")),
    video:
      Boolean(row.video) ||
      (Array.isArray(row.capabilities) && row.capabilities.some((item) => item === "video")),
    reasoning:
      Boolean(row.reasoning) ||
      (Array.isArray(row.capabilities) && row.capabilities.some((item) => item === "reasoning")),
    web_search:
      Boolean(row.web_search) ||
      (Array.isArray(row.capabilities) && row.capabilities.some((item) => item === "web_search")),
    capabilities: Array.isArray(row.capabilities)
      ? row.capabilities.filter((item): item is string => typeof item === "string")
      : undefined,
    reasoning_efforts: Array.isArray(row.reasoning_efforts)
      ? row.reasoning_efforts.filter((item): item is string => typeof item === "string")
      : undefined,
  };
}

export function parseModelCatalogPayload(data: unknown): {
  models: string[];
  catalog: ModelInfo[];
  defaultModel: string;
} {
  const payload = asRecord(data) || {};
  const readyRaw = Array.isArray(payload.models)
    ? payload.models
    : Array.isArray(payload.items)
      ? payload.items
      : [];
  const allRaw = Array.isArray(payload.all) ? payload.all : readyRaw;
  const catalog = allRaw.map(parseModelInfo).filter((row): row is ModelInfo => Boolean(row));
  const readyIds = new Set(
    readyRaw
      .map(parseModelInfo)
      .filter((row): row is ModelInfo => Boolean(row && row.ready !== false))
      .map((row) => row.id),
  );
  const models = (readyIds.size ? catalog.filter((row) => readyIds.has(row.id)) : catalog)
    .filter((row) => row.ready !== false)
    .map((row) => row.id);
  return {
    models: models.length ? models : catalog.map((row) => row.id),
    catalog,
    defaultModel: String(payload.default_model || "").trim(),
  };
}

export function groupModelsByFamily(catalog: ModelInfo[]): { family: string; models: ModelInfo[] }[] {
  const groups = new Map<string, ModelInfo[]>();
  for (const row of catalog) {
    const family = row.family || "Other";
    const list = groups.get(family) || [];
    list.push(row);
    groups.set(family, list);
  }
  for (const list of groups.values()) {
    list.sort((a, b) => Number(Boolean(b.latest)) - Number(Boolean(a.latest)) || (a.label || a.id).localeCompare(b.label || b.id));
  }
  const names = [...groups.keys()].sort((a, b) => {
    const ai = FAMILY_ORDER.indexOf(a);
    const bi = FAMILY_ORDER.indexOf(b);
    return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi) || a.localeCompare(b);
  });
  return names.map((family) => ({ family, models: groups.get(family) || [] }));
}

/** Catalog host id: models.yaml provider, else inferred from family. */
export function modelProviderId(row: ModelInfo): string {
  const explicit = String(row.provider || "").trim().toLowerCase();
  if (explicit) return explicit === "commandcode-messages" ? "commandcode" : explicit;
  const family = String(row.family || "").toLowerCase();
  if (family === "glm") return "zai";
  if (family === "gemini") return "gemini";
  if (family === "kimi" || family === "deepseek" || family === "qwen") return "siliconflow";
  if (family === "openai") return "openai";
  if (family === "codex") return "openai-codex-app";
  if (family === "anthropic") return "anthropic";
  return "other";
}

export function groupModelsByProvider(
  catalog: ModelInfo[],
  role: "executor" | "planner" = "executor",
): { id: string; label: string; models: ModelInfo[] }[] {
  const rank = (row: ModelInfo) => {
    if (row.ready === false) return 2;
    return role === "planner" || composerModelSelectable(row) ? 0 : 1;
  };
  const groups = new Map<string, ModelInfo[]>();
  for (const row of catalog) {
    const id = modelProviderId(row);
    const list = groups.get(id) || [];
    list.push(row);
    groups.set(id, list);
  }
  for (const list of groups.values()) {
    list.sort(
      (a, b) =>
        rank(a) - rank(b) ||
        Number(Boolean(b.latest)) - Number(Boolean(a.latest)) ||
        (a.label || a.id).localeCompare(b.label || b.id),
    );
  }
  const ids = [...groups.keys()].sort((a, b) => {
    const readyOrder = rank(groups.get(a)![0]) - rank(groups.get(b)![0]);
    if (readyOrder) return readyOrder;
    const ai = PROVIDER_ORDER.indexOf(a);
    const bi = PROVIDER_ORDER.indexOf(b);
    const ao = ai === -1 ? 99 : ai;
    const bo = bi === -1 ? 99 : bi;
    if (ao !== bo) return ao - bo;
    return providerDisplayName(a).localeCompare(providerDisplayName(b));
  });
  return ids.map((id) => ({
    id,
    label: providerDisplayName(id),
    models: groups.get(id) || [],
  }));
}

export function modelDisplayLabel(catalog: ModelInfo[], modelId: string): string {
  const hit = catalog.find((row) => row.id === modelId);
  return hit?.label || modelId;
}

const PROVIDER_LABELS: Record<string, string> = {
  openai: "OpenAI",
  "openai-codex-app": "Codex Runtime",
  anthropic: "Anthropic",
  gemini: "Google Gemini",
  antigravity: "Antigravity",
  siliconflow: "SiliconFlow",
  zai: "Z.AI",
  "opencode-go": "OpenCode Go",
  commandcode: "Command Code",
  "commandcode-messages": "Command Code",
  kiro: "Kiro",
  cloudflare: "Cloudflare Workers AI",
};

/** Human name for a models.yaml / setup provider id. */
export function providerDisplayName(
  provider: string | undefined,
  extras?: Array<{ id: string; label: string }> | Record<string, string>,
): string {
  const id = String(provider || "").trim();
  if (!id) return "";
  if (PROVIDER_LABELS[id]) return PROVIDER_LABELS[id];
  const extraMap = Array.isArray(extras)
    ? Object.fromEntries(extras.map((row) => [row.id, row.label]))
    : extras || {};
  if (extraMap[id]) return extraMap[id];
  return id
    .replace(/[-_]+/g, " ")
    .replace(/\b\w/g, (char) => char.toUpperCase());
}

/** Role-picker row: `GPT 5.6 Sol · OpenAI` so the same weights from two hosts stay distinct. */
export function modelOptionLabel(
  model: { id: string; provider?: string; model?: string; label?: string },
  catalog: ModelInfo[] = [],
  extras?: Array<{ id: string; label: string }> | Record<string, string>,
): string {
  const hit = catalog.find((row) => row.id === model.id);
  const name = hit?.label || model.label || model.model || model.id;
  const source = providerDisplayName(model.provider || hit?.provider, extras);
  if (!source) return name;
  if (name.toLowerCase().includes(source.toLowerCase())) return name;
  return `${name} · ${source}`;
}

/** User pin only — never the catalog default. */
export function pinnedComposerModel(
  modelOverride: string,
  mainModel: string,
  _defaultModel = "",
): string {
  const pin = modelOverride.trim();
  const saved = mainModel.trim();
  return pin || saved;
}

export function currentModelId(
  modelOverride: string,
  mainModel: string,
  defaultModel: string,
): string {
  return (
    pinnedComposerModel(modelOverride, mainModel, defaultModel) ||
    defaultModel.trim()
  );
}

/** Whether a picker id is served by the authenticated local Codex app-server. */
export function isCodexRuntimeModel(catalog: ModelInfo[], modelId: string): boolean {
  const id = modelId.trim();
  const row = catalog.find((candidate) => candidate.id === id);
  return row?.provider === "openai-codex-app" || id === "gpt-codex-app" || id.startsWith("gpt-codex-");
}

/** Model answers can use built-in grounded/hosted web search (Codex, Gemini, OpenAI). */
export function modelHasNativeSearch(catalog: ModelInfo[], modelId: string): boolean {
  const id = modelId.trim();
  if (!id) return false;
  const row = catalog.find((candidate) => candidate.id === id);
  if (!row) return false;
  return (
    Boolean(row.web_search) ||
    Boolean(row.capabilities?.includes("web_search"))
  );
}

export function modelNativeSearchLabel(catalog: ModelInfo[], modelId: string): string {
  const id = modelId.trim();
  if (isCodexRuntimeModel(catalog, id)) return "Codex hosted search";
  const row = catalog.find((candidate) => candidate.id === id);
  const provider = String(row?.provider || "").toLowerCase();
  if (provider === "gemini") return "Gemini live web search";
  if (provider === "openai") return "OpenAI live web search";
  return "Live web search";
}

/** Short capability summary for model row tooltips. */
export function modelCapabilityTitle(row: ModelInfo): string {
  const bits: string[] = [];
  if (row.web_search || row.capabilities?.includes("web_search")) bits.push("live web search");
  if (row.vision) bits.push(row.video ? "vision + video" : "vision");
  else if (row.reasoning) bits.push("reasoning");
  return bits.length ? bits.join(" · ") : "";
}

export function composerModelSelectable(row: ModelInfo): boolean {
  if (row.ready === false) return false;
  const provider = String(row.provider || "").trim().toLowerCase();
  const appServer = provider === "openai-codex-app" || row.id === "gpt-codex-app";
  if (appServer) return true;
  if ((row.family || "").toLowerCase() === "codex") return false;
  if (row.capabilities && !row.capabilities.includes("tool_calling")) return false;
  return true;
}

export function composerModelHint(row: ModelInfo): string {
  if (row.ready === false) return row.provider === "kiro" || row.provider === "openai-codex-app" ? "Needs connection" : "Needs key";
  const provider = String(row.provider || "").trim().toLowerCase();
  if (provider === "openai-codex-app" || row.id === "gpt-codex-app") return "Codex · Tools";
  if (!composerModelSelectable(row)) return "Planning only";
  return "";
}

/** Greyed Codex / no-tools rows stay disabled; missing-key rows open Settings. */
export function composerModelDisabled(row: ModelInfo): boolean {
  return !composerModelSelectable(row) && row.ready !== false;
}

export function effortLabel(effort: string): string {
  return EFFORT_OPTIONS.find((row) => row.value === effort)?.label || "Auto";
}
