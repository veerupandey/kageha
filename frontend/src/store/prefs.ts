import type { AgentMode, Density, UserPrefs } from "../api/types";
import { isTheme, themeScheme } from "../lib/themes";

export const PREFS_KEY = "kageha.prefs";
/** Legacy ask-mode flag — migrated into prefs.defaultAskMode. */
export const ASK_MODE_KEY = "kageha.askMode";

export const DEFAULT_PREFS: UserPrefs = {
  density: "comfortable",
  defaultAskMode: false,
  defaultAgentMode: "normal",
  reduceMotion: false,
  theme: "dark",
  voiceReply: false,
  voiceConversation: false,
  mainModel: "",
  plannerModel: "",
  workerModel: "",
  reasoningEffort: "",
  mediaModel: "auto",
  vision: "auto",
  webSearch: "auto",
  imageGeneration: "auto",
  tts: "auto",
  stt: "auto",
  sidebarCollapsed: false,
  rightPanelCollapsed: true,
};

const IMAGE_PROVIDERS = ["auto", "codex", "antigravity", "nano-banana", "fal", "cloudflare"];
const isProviderId = (value: unknown): value is string => typeof value === "string" && /^[a-z0-9][a-z0-9_-]{0,79}$/.test(value);
const STT_PROVIDERS = ["auto", "gemini-stt", "openai", "cloudflare"];

const AGENT_MODES: AgentMode[] = ["normal", "plan", "goal"];
const EFFORTS = new Set(["", "low", "medium", "high"]);

function isDensity(v: unknown): v is Density {
  return v === "comfortable" || v === "compact";
}

function isAgentMode(v: unknown): v is AgentMode {
  return typeof v === "string" && AGENT_MODES.includes(v as AgentMode);
}

function readLegacyAskMode(): boolean | null {
  try {
    const raw = localStorage.getItem(ASK_MODE_KEY);
    if (raw === "1") return true;
    if (raw === "0") return false;
    return null;
  } catch {
    return null;
  }
}

/** Load prefs from localStorage (with legacy ask-mode migration). */
export function loadPrefs(): UserPrefs {
  const base = { ...DEFAULT_PREFS };
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<UserPrefs>;
      if (isDensity(parsed.density)) base.density = parsed.density;
      if (typeof parsed.defaultAskMode === "boolean") {
        base.defaultAskMode = parsed.defaultAskMode;
      }
      if (isAgentMode(parsed.defaultAgentMode)) {
        base.defaultAgentMode = parsed.defaultAgentMode;
      }
      if (typeof parsed.reduceMotion === "boolean") {
        base.reduceMotion = parsed.reduceMotion;
      }
      if (isTheme(parsed.theme)) base.theme = parsed.theme;
      if (typeof parsed.voiceReply === "boolean") {
        base.voiceReply = parsed.voiceReply;
      }
      if (typeof parsed.voiceConversation === "boolean") {
        base.voiceConversation = parsed.voiceConversation;
      }
      if (typeof parsed.mainModel === "string") base.mainModel = parsed.mainModel;
      if (typeof parsed.plannerModel === "string") base.plannerModel = parsed.plannerModel;
      if (typeof parsed.workerModel === "string") base.workerModel = parsed.workerModel;
      if (typeof parsed.reasoningEffort === "string" && EFFORTS.has(parsed.reasoningEffort)) {
        base.reasoningEffort = parsed.reasoningEffort as UserPrefs["reasoningEffort"];
      }
      if (typeof parsed.mediaModel === "string") base.mediaModel = parsed.mediaModel;
      if (["auto", "native", "gemini_analyze"].includes(String(parsed.vision))) base.vision = parsed.vision as UserPrefs["vision"];
      if (["auto", "ddg", "zai", "tavily", "brave", "exa", "perplexity", "gemini", "codex", "antigravity"].includes(String(parsed.webSearch))) base.webSearch = parsed.webSearch as UserPrefs["webSearch"];
      if (["auto", "codex", "antigravity", "nano-banana", "fal", "cloudflare"].includes(String(parsed.imageGeneration))) base.imageGeneration = parsed.imageGeneration as UserPrefs["imageGeneration"];
      // Existing users' media choice becomes the new image default.
      if (base.imageGeneration === "auto" && typeof parsed.mediaModel === "string" && parsed.mediaModel !== "gpt-image") base.imageGeneration = parsed.mediaModel as UserPrefs["imageGeneration"];
      if (isProviderId(parsed.tts)) base.tts = parsed.tts;
      if (typeof parsed.ttsVoice === "string") base.ttsVoice = parsed.ttsVoice;
      if (STT_PROVIDERS.includes(String(parsed.stt))) base.stt = parsed.stt as UserPrefs["stt"];
      if (typeof parsed.sidebarCollapsed === "boolean") {
        base.sidebarCollapsed = parsed.sidebarCollapsed;
      }
      if (typeof parsed.rightPanelCollapsed === "boolean") {
        base.rightPanelCollapsed = parsed.rightPanelCollapsed;
      }
    } else {
      const legacy = readLegacyAskMode();
      if (legacy != null) base.defaultAskMode = legacy;
    }
  } catch {
    const legacy = readLegacyAskMode();
    if (legacy != null) base.defaultAskMode = legacy;
  }
  if (base.voiceConversation) base.voiceReply = true;
  return base;
}

/** Persist prefs and keep legacy ask-mode key in sync. */
export function savePrefs(prefs: UserPrefs): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
    localStorage.setItem(ASK_MODE_KEY, prefs.defaultAskMode ? "1" : "0");
  } catch {
    /* ignore quota / private mode */
  }
}

/** Apply density / reduce-motion / theme attributes on the shell. */
export function applyPrefsToDocument(prefs: UserPrefs): void {
  if (typeof document === "undefined") return;
  const root = document.getElementById("app") || document.documentElement;
  root.setAttribute("data-density", prefs.density);
  root.setAttribute("data-theme", prefs.theme);
  root.setAttribute("data-color-scheme", themeScheme(prefs.theme));
  root.setAttribute("data-ui", "kageha");
  document.documentElement.setAttribute("data-ui", "kageha");
  if (prefs.reduceMotion) {
    root.setAttribute("data-reduce-motion", "true");
  } else {
    root.removeAttribute("data-reduce-motion");
  }
  document.documentElement.setAttribute("data-density", prefs.density);
  document.documentElement.setAttribute("data-theme", prefs.theme);
  document.documentElement.setAttribute("data-color-scheme", themeScheme(prefs.theme));
  if (prefs.reduceMotion) {
    document.documentElement.setAttribute("data-reduce-motion", "true");
  } else {
    document.documentElement.removeAttribute("data-reduce-motion");
  }
}

export function mergePrefs(
  current: UserPrefs,
  patch: Partial<UserPrefs>,
): UserPrefs {
  const next: UserPrefs = { ...current };
  if (isDensity(patch.density)) next.density = patch.density;
  if (typeof patch.defaultAskMode === "boolean") {
    next.defaultAskMode = patch.defaultAskMode;
  }
  if (isAgentMode(patch.defaultAgentMode)) {
    next.defaultAgentMode = patch.defaultAgentMode;
  }
  if (typeof patch.reduceMotion === "boolean") {
    next.reduceMotion = patch.reduceMotion;
  }
  if (isTheme(patch.theme)) next.theme = patch.theme;
  if (typeof patch.voiceReply === "boolean") {
    next.voiceReply = patch.voiceReply;
  }
  if (typeof patch.voiceConversation === "boolean") {
    next.voiceConversation = patch.voiceConversation;
  }
  if (typeof patch.mainModel === "string") next.mainModel = patch.mainModel;
  if (typeof patch.plannerModel === "string") next.plannerModel = patch.plannerModel;
  if (typeof patch.workerModel === "string") next.workerModel = patch.workerModel;
  if (typeof patch.reasoningEffort === "string" && EFFORTS.has(patch.reasoningEffort)) {
    next.reasoningEffort = patch.reasoningEffort as UserPrefs["reasoningEffort"];
  }
  if (typeof patch.mediaModel === "string") {
    next.mediaModel = patch.mediaModel;
  }
  if (["auto", "native", "gemini_analyze"].includes(String(patch.vision))) {
    next.vision = patch.vision as UserPrefs["vision"];
  }
  if (["auto", "ddg", "zai", "tavily", "brave", "exa", "perplexity", "gemini", "codex", "antigravity"].includes(String(patch.webSearch))) {
    next.webSearch = patch.webSearch as UserPrefs["webSearch"];
  }
  if (IMAGE_PROVIDERS.includes(String(patch.imageGeneration))) {
    next.imageGeneration = patch.imageGeneration as UserPrefs["imageGeneration"];
    next.mediaModel = next.imageGeneration;
  }
  if (typeof patch.ttsVoice === "string") next.ttsVoice = patch.ttsVoice;
  if (isProviderId(patch.tts)) {
    next.tts = patch.tts as UserPrefs["tts"];
  }
  if (STT_PROVIDERS.includes(String(patch.stt))) {
    next.stt = patch.stt as UserPrefs["stt"];
  }
  if (typeof patch.sidebarCollapsed === "boolean") {
    next.sidebarCollapsed = patch.sidebarCollapsed;
  }
  if (typeof patch.rightPanelCollapsed === "boolean") {
    next.rightPanelCollapsed = patch.rightPanelCollapsed;
  }
  if (patch.voiceReply === false && patch.voiceConversation !== true) next.voiceConversation = false;
  if (next.voiceConversation) next.voiceReply = true;
  return next;
}
