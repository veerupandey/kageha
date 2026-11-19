import { describe, expect, it } from "vitest";
import {
  composerModelDisabled,
  composerModelHint,
  composerModelSelectable,
  currentModelId,
  pinnedComposerModel,
  effortLabel,
  groupModelsByFamily,
  groupModelsByProvider,
  isCodexRuntimeModel,
  modelHasNativeSearch,
  modelNativeSearchLabel,
  modelOptionLabel,
  parseModelCatalogPayload,
  parseModelInfo,
  providerDisplayName,
} from "./modelCatalog";
import type { ModelInfo } from "../api/types";

const CATALOG: ModelInfo[] = [
  { id: "gemini-flash", family: "Gemini", label: "3.7 Flash", latest: true, ready: true },
  { id: "gemini-pro", family: "Gemini", label: "3.1 Pro", latest: false, ready: true },
  { id: "glm-5.3", family: "GLM", label: "GLM 5.3", latest: true, ready: true },
  {
    id: "gpt-codex-app",
    provider: "openai-codex-app",
    family: "Codex",
    label: "GPT 5.6 Sol",
    latest: true,
    ready: true,
    capabilities: ["tool_calling", "vision"],
  },
  {
    id: "gpt-codex-terra",
    provider: "openai-codex-app",
    family: "Codex",
    label: "GPT 5.6 Terra",
    latest: false,
    ready: true,
    capabilities: ["tool_calling", "vision"],
  },
];

describe("modelCatalog helpers", () => {
  it("prioritizes ready tool models over latest-but-unverified and disconnected models", () => {
    const groups = groupModelsByProvider([
      { id: "unconnected", provider: "kiro", ready: false, capabilities: ["tool_calling"] },
      { id: "old", provider: "openai", ready: true, latest: true, capabilities: [] },
      { id: "frontier", provider: "openai", ready: true, capabilities: ["tool_calling"] },
    ]);
    expect(groups.map((group) => group.id)).toEqual(["openai", "kiro"]);
    expect(groups[0].models.map((model) => model.id)).toEqual(["frontier", "old"]);
  });
  it("merges Command Code protocols while preserving model routing ids", () => {
    const rows: ModelInfo[] = [
      { id: "beam", provider: "commandcode", free: true },
      { id: "claude", provider: "commandcode-messages" },
    ];
    const groups = groupModelsByProvider(rows);
    expect(groups).toHaveLength(1);
    expect(groups[0].label).toBe("Command Code");
    expect(groups[0].models.map((m) => m.id)).toEqual(["beam", "claude"]);
    expect(rows[1].provider).toBe("commandcode-messages");
    expect(parseModelInfo({ id: "beam", free: true, pricing_note: "Free preview" })).toMatchObject({ free: true, pricing_note: "Free preview" });
    expect(parseModelInfo({ id: "unknown" })?.free).toBe(false);
  });
  it("groups SiliconFlow Kimi, DeepSeek, and Qwen under one host", () => {
    const groups = groupModelsByProvider([
      { id: "kimi-k3", family: "Kimi", provider: "siliconflow", label: "Kimi K3", latest: true, ready: true },
      { id: "deepseek-v4-pro", family: "DeepSeek", provider: "siliconflow", label: "DeepSeek V4 Pro", latest: true, ready: true },
      { id: "qwen-3.8", family: "Qwen", provider: "siliconflow", label: "Qwen 3.8", latest: true, ready: true },
      { id: "glm-5.3", family: "GLM", provider: "zai", label: "GLM 5.3", latest: true, ready: true },
    ]);
    expect(groups.map((g) => g.label)).toEqual(["Z.AI", "SiliconFlow"]);
    expect(groups[1].models.map((m) => m.label)).toEqual([
      "DeepSeek V4 Pro",
      "Kimi K3",
      "Qwen 3.8",
    ]);
  });

  it("keeps SiliconFlow Kimi, DeepSeek, and Qwen as separate families", () => {
    const groups = groupModelsByFamily([
      { id: "kimi-k3", family: "Kimi", label: "Kimi K3", latest: true, ready: true },
      { id: "deepseek-v4-pro", family: "DeepSeek", label: "DeepSeek V4 Pro", latest: true, ready: true },
      { id: "qwen-3.8", family: "Qwen", label: "Qwen 3.8", latest: true, ready: true },
      { id: "glm-5.3", family: "GLM", label: "GLM 5.3", latest: true, ready: true },
    ]);
    expect(groups.map((g) => g.family)).toEqual(["GLM", "Kimi", "DeepSeek", "Qwen"]);
  });

  it("groups Gemini Flash and Pro together with Flash as latest", () => {
    const groups = groupModelsByFamily(CATALOG);
    expect(groups.map((g) => g.family)).toEqual(["GLM", "Gemini", "Codex"]);
    const gemini = groups.find((g) => g.family === "Gemini");
    expect(gemini?.models.map((m) => m.label)).toEqual(["3.7 Flash", "3.1 Pro"]);
    expect(gemini?.models[0].latest).toBe(true);
  });

  it("parses /api/models payload including not-ready siblings", () => {
    const parsed = parseModelCatalogPayload({
      default_model: "gemini-flash",
      models: [CATALOG[0], CATALOG[1], CATALOG[2]],
      all: CATALOG,
    });
    expect(parsed.defaultModel).toBe("gemini-flash");
    expect(parsed.models).toEqual(["gemini-flash", "gemini-pro", "glm-5.3"]);
    expect(parsed.catalog.map((m) => m.id)).toEqual([
      "gemini-flash",
      "gemini-pro",
      "glm-5.3",
      "gpt-codex-app",
      "gpt-codex-terra",
    ]);
  });

  it("falls back to the configured default for the pill id", () => {
    expect(currentModelId("", "", "gemini-flash")).toBe("gemini-flash");
    expect(currentModelId("gemini-pro", "", "gemini-flash")).toBe("gemini-pro");
    expect(effortLabel("high")).toBe("High");
    expect(effortLabel("")).toBe("Auto");
  });

  it("always trusts the live pin, even when it matches the catalog default", () => {
    expect(pinnedComposerModel("glm-5.3", "gpt-codex-app", "glm-5.3")).toBe(
      "glm-5.3",
    );
    expect(pinnedComposerModel("", "gpt-codex-app", "glm-5.3")).toBe("gpt-codex-app");
    expect(pinnedComposerModel("", "", "glm-5.3")).toBe("");
    expect(currentModelId("glm-5.3", "gpt-codex-app", "glm-5.3")).toBe("glm-5.3");
  });

  it("recognizes only the local Codex Runtime picker models", () => {
    expect(isCodexRuntimeModel(CATALOG, "gpt-codex-terra")).toBe(true);
    expect(isCodexRuntimeModel(CATALOG, "gpt-codex-luna")).toBe(true);
    expect(isCodexRuntimeModel(CATALOG, "gpt-5.6-terra")).toBe(false);
    expect(isCodexRuntimeModel(CATALOG, "glm-5.3")).toBe(false);
  });

  it("marks SiliconFlow thinking models as reasoning from capabilities", () => {
    const parsed = parseModelInfo({
      id: "qwen-3.8",
      capabilities: ["tool_calling", "reasoning"],
    });
    expect(parsed?.reasoning).toBe(true);
    expect(parsed?.vision).toBe(false);
    expect(parseModelInfo({ id: "kimi-k3", reasoning: true })?.reasoning).toBe(true);
  });

  it("detects grounded/native web search from catalog flags", () => {
    const grounded: ModelInfo[] = [
      {
        id: "gemini-flash",
        provider: "gemini",
        family: "Gemini",
        label: "3.7 Flash",
        ready: true,
        web_search: true,
        vision: true,
        capabilities: ["tool_calling", "vision", "web_search"],
      },
      {
        id: "gpt-5.6-sol",
        provider: "openai",
        family: "OpenAI",
        label: "GPT 5.6 Sol",
        ready: true,
        web_search: true,
        capabilities: ["tool_calling", "web_search"],
      },
      {
        id: "glm-5.3",
        provider: "opencode-go",
        family: "GLM",
        label: "GLM 5.3",
        ready: true,
        capabilities: ["tool_calling", "reasoning"],
      },
      {
        id: "gpt-codex-terra",
        provider: "openai-codex-app",
        family: "Codex",
        label: "GPT 5.6 Terra",
        ready: true,
        web_search: true,
        capabilities: ["tool_calling", "vision", "web_search"],
      },
      {
        id: "gpt-codex-app",
        provider: "openai-codex-app",
        family: "Codex",
        label: "GPT 5.6 Sol",
        ready: true,
        web_search: true,
        capabilities: ["tool_calling", "vision", "web_search"],
      },
    ];
    expect(modelHasNativeSearch(grounded, "gpt-codex-terra")).toBe(true);
    expect(modelHasNativeSearch(grounded, "gemini-flash")).toBe(true);
    expect(modelHasNativeSearch(grounded, "gpt-5.6-sol")).toBe(true);
    expect(modelHasNativeSearch(grounded, "glm-5.3")).toBe(false);
    expect(modelNativeSearchLabel(grounded, "gemini-flash")).toBe("Gemini live web search");
    expect(modelNativeSearchLabel(grounded, "gpt-5.6-sol")).toBe("OpenAI live web search");
    expect(modelNativeSearchLabel(grounded, "gpt-codex-app")).toBe("Codex hosted search");
    expect(
      composerModelHint({
        id: "gemini-flash",
        provider: "gemini",
        ready: true,
        web_search: true,
        vision: true,
        capabilities: ["tool_calling", "vision", "web_search"],
      }),
    ).toBe("");
    expect(parseModelInfo({ id: "gpt-5.6-sol", capabilities: ["web_search"] })?.web_search).toBe(
      true,
    );
  });

  it("keeps OpenAI without a key and Codex out of the composer agent picker", () => {
    expect(
      composerModelSelectable({
        id: "gpt-5.6-sol",
        family: "OpenAI",
        label: "GPT 5.6 Sol",
        ready: false,
        capabilities: ["tool_calling"],
      }),
    ).toBe(false);
    expect(
      composerModelHint({
        id: "gpt-5.6-sol",
        family: "OpenAI",
        label: "GPT 5.6 Sol",
        ready: false,
      }),
    ).toBe("Needs key");
    expect(
      composerModelSelectable({
        id: "gpt-codex-app",
        provider: "openai-codex-app",
        family: "Codex",
        label: "GPT 5.6 Sol",
        ready: true,
        capabilities: ["tool_calling", "vision"],
      }),
    ).toBe(true);
    expect(
      composerModelHint({
        id: "gpt-codex-app",
        provider: "openai-codex-app",
        family: "Codex",
        ready: true,
        capabilities: ["tool_calling"],
      }),
    ).toBe("Codex · Tools");
    expect(
      composerModelSelectable({
        id: "glm-5.3",
        family: "GLM",
        label: "GLM 5.3",
        ready: true,
        capabilities: ["tool_calling"],
      }),
    ).toBe(true);
    expect(
      composerModelSelectable({
        id: "siliconflow/moonshotai-kimi-k2",
        provider: "siliconflow",
        family: "Kimi",
        ready: true,
        capabilities: ["tool_calling", "structured_output"],
      }),
    ).toBe(true);
    expect(
      composerModelDisabled({
        id: "gpt-5.6-sol",
        family: "OpenAI",
        ready: false,
        capabilities: ["tool_calling"],
      }),
    ).toBe(false);
  });

  it("names the host so the same weights from two providers stay distinct", () => {
    expect(providerDisplayName("zai")).toBe("Z.AI");
    expect(providerDisplayName("openai-codex-app")).toBe("Codex Runtime");
    expect(providerDisplayName("demo-provider", [{ id: "demo-provider", label: "Demo Provider" }])).toBe(
      "Demo Provider",
    );
    const catalog: ModelInfo[] = [
      { id: "gpt-5.6-sol", label: "GPT 5.6 Sol", provider: "openai", family: "OpenAI" },
      { id: "gpt-codex-app", label: "GPT 5.6 Sol", provider: "openai-codex-app", family: "Codex" },
    ];
    expect(modelOptionLabel({ id: "gpt-5.6-sol", provider: "openai" }, catalog)).toBe(
      "GPT 5.6 Sol · OpenAI",
    );
    expect(modelOptionLabel({ id: "gpt-codex-app", provider: "openai-codex-app" }, catalog)).toBe(
      "GPT 5.6 Sol · Codex Runtime",
    );
  });
});
