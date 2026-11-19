export interface SearchOption {
  id: string;
  label: string;
  ready?: boolean;
}

export interface SearchKeyCard {
  id: string;
  label: string;
  env: string;
  ready: boolean;
  hint?: string;
}

export interface VisionSetup {
  backend: string;
  configured: string;
  label: string;
  hint?: string;
  source?: string;
  options: SearchOption[];
}

export interface SearchSetup {
  backend: string;
  configured: string;
  label: string;
  source?: string;
  fallbacks: string[];
  options: SearchOption[];
  keys?: SearchKeyCard[];
  vision?: VisionSetup;
}

export const SEARCH_SHORT: Record<string, string> = {
  auto: "Auto",
  native: "Native grounded search",
  openai: "OpenAI",
  anthropic: "Anthropic",
  antigravity: "Antigravity",
  zai: "Z.AI",
  brave: "Brave",
  tavily: "Tavily",
  exa: "Exa",
  perplexity: "Perplexity",
  gemini: "Gemini",
  gemini_only: "Gemini only",
  ddg: "DuckDuckGo",
};

export const VISION_SHORT: Record<string, string> = {
  auto: "Auto",
  gemini_analyze: "Gemini",
  native: "Model vision",
};

export const DEFAULT_SEARCH_OPTIONS: SearchOption[] = [
  { id: "auto", label: "Auto (native search when supported)", ready: true },
  { id: "antigravity", label: "Antigravity native search", ready: false },
  { id: "zai", label: "Z.AI Web Search Prime", ready: false },
  { id: "brave", label: "Brave Search", ready: false },
  { id: "tavily", label: "Tavily", ready: false },
  { id: "exa", label: "Exa", ready: false },
  { id: "perplexity", label: "Perplexity", ready: false },
  { id: "gemini", label: "Gemini Search", ready: false },
  { id: "ddg", label: "DuckDuckGo", ready: true },
];

export const DEFAULT_VISION_OPTIONS: SearchOption[] = [
  {
    id: "auto",
    label: "Auto (native multimodal if model supports it, else same-provider vision sibling, else Gemini)",
    ready: true,
  },
  { id: "gemini_analyze", label: "Gemini analyze", ready: false },
  { id: "native", label: "Model vision only", ready: true },
];

export const DEFAULT_SEARCH_KEYS: SearchKeyCard[] = [
  {
    id: "brave",
    label: "Brave Search",
    env: "BRAVE_API_KEY",
    ready: false,
    hint: "search.brave.com/search/api",
  },
  {
    id: "tavily",
    label: "Tavily",
    env: "TAVILY_API_KEY",
    ready: false,
    hint: "app.tavily.com",
  },
  {
    id: "exa",
    label: "Exa",
    env: "EXA_API_KEY",
    ready: false,
    hint: "dashboard.exa.ai",
  },
  {
    id: "perplexity",
    label: "Perplexity",
    env: "PERPLEXITY_API_KEY",
    ready: false,
    hint: "perplexity.ai/settings/api",
  },
  {
    id: "zai",
    label: "Z.AI Search",
    env: "ZAI_API_KEY",
    ready: false,
    hint: "Also unlocks Coding Plan chat under Models (separate from OpenCode Go)",
  },
  {
    id: "gemini",
    label: "Google Gemini",
    env: "GEMINI_API_KEY",
    ready: false,
    hint: "Live Google Search + vision hop · also unlocks Gemini chat under Models",
  },
];

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : null;
}

function parseOption(raw: unknown): SearchOption | null {
  const row = asRecord(raw);
  if (!row) return null;
  const id = String(row.id || "").trim();
  if (!id) return null;
  const label = typeof row.label === "string" && row.label.trim() ? row.label.trim() : id;
  return { id, label, ready: row.ready !== false };
}

function parseKeyCard(raw: unknown): SearchKeyCard | null {
  const row = asRecord(raw);
  if (!row) return null;
  const id = String(row.id || "").trim();
  const env = String(row.env || "").trim();
  if (!id || !env) return null;
  return {
    id,
    label: typeof row.label === "string" && row.label.trim() ? row.label.trim() : id,
    env,
    ready: row.ready === true,
    hint: typeof row.hint === "string" ? row.hint : undefined,
  };
}

function parseVision(raw: unknown): VisionSetup | undefined {
  const row = asRecord(raw);
  if (!row) return undefined;
  const options = Array.isArray(row.options)
    ? row.options.map(parseOption).filter((item): item is SearchOption => Boolean(item))
    : DEFAULT_VISION_OPTIONS;
  return {
    backend: String(row.backend || "auto"),
    configured: String(row.configured || "auto"),
    label: typeof row.label === "string" && row.label.trim() ? row.label.trim() : "Auto",
    hint: typeof row.hint === "string" ? row.hint : undefined,
    source: typeof row.source === "string" ? row.source : undefined,
    options: options.length ? options : DEFAULT_VISION_OPTIONS,
  };
}

export function parseSearchSetup(raw: unknown): SearchSetup | null {
  const row = asRecord(raw);
  if (!row) return null;
  const options = Array.isArray(row.options)
    ? row.options.map(parseOption).filter((item): item is SearchOption => Boolean(item))
    : DEFAULT_SEARCH_OPTIONS;
  const keys = Array.isArray(row.keys)
    ? row.keys.map(parseKeyCard).filter((item): item is SearchKeyCard => Boolean(item))
    : [];
  const fallbacks = Array.isArray(row.fallbacks)
    ? row.fallbacks.filter((item): item is string => typeof item === "string")
    : [];
  return {
    backend: String(row.backend || ""),
    configured: String(row.configured || "auto"),
    label: typeof row.label === "string" && row.label.trim() ? row.label.trim() : "Search",
    source: typeof row.source === "string" ? row.source : undefined,
    fallbacks,
    options: options.length ? options : DEFAULT_SEARCH_OPTIONS,
    keys: keys.length ? keys : undefined,
    vision: parseVision(row.vision),
  };
}

export function parseSearchFromProviderSetup(data: unknown): SearchSetup | null {
  const row = asRecord(data);
  if (!row) return null;
  if (row.search) return parseSearchSetup(row.search);
  if (row.backend || row.options) return parseSearchSetup(row);
  return null;
}

export function searchShortLabel(id: string, fallback = ""): string {
  return SEARCH_SHORT[id] || fallback || id;
}

export function visionShortLabel(id: string, fallback = ""): string {
  return VISION_SHORT[id] || fallback || id;
}

export function searchPillLabel(setup: SearchSetup | null): string {
  if (!setup) return "Search";
  const configured = (setup.configured || "auto").trim();
  if (configured && configured !== "auto") {
    return searchShortLabel(configured, setup.label);
  }
  if (setup.backend) {
    return searchShortLabel(setup.backend, setup.label);
  }
  return "Auto";
}

export function searchPillTitle(setup: SearchSetup | null): string {
  if (!setup) return "Web search";
  const configured = setup.configured || "auto";
  if (configured === "auto" && setup.label) {
    return `Web search: Auto (${setup.label})`;
  }
  return `Web search: ${setup.label}`;
}

export function visionPillLabel(setup: SearchSetup | null): string {
  const vision = setup?.vision;
  if (!vision) return "Auto";
  const configured = (vision.configured || "auto").trim();
  if (configured && configured !== "auto") {
    return visionShortLabel(configured, vision.label);
  }
  if (vision.backend) {
    return visionShortLabel(vision.backend, vision.label);
  }
  return "Auto";
}

/** Friendly copy when a keyed search/vision option is missing credentials. */
export function searchNeedsKeyHint(option: SearchOption): string {
  const short = searchShortLabel(option.id, option.label);
  if (option.id === "antigravity") {
    return `${short} needs the signed-in agy CLI — connect Antigravity on this machine`;
  }
  return `${short} needs an API key — add it in Settings → Search`;
}

export function visionNeedsKeyHint(option: SearchOption): string {
  const short = visionShortLabel(option.id, option.label);
  return `${short} needs an API key — add it in Settings → Search`;
}
