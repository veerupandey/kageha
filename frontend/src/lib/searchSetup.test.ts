import { describe, expect, it } from "vitest";
import {
  DEFAULT_VISION_OPTIONS,
  parseSearchFromProviderSetup,
  parseSearchSetup,
  searchNeedsKeyHint,
  searchPillLabel,
  searchPillTitle,
  searchShortLabel,
  visionPillLabel,
} from "./searchSetup";

const SAMPLE = {
  backend: "zai",
  configured: "auto",
  label: "Z.AI Web Search Prime",
  source: "yaml",
  fallbacks: ["Brave Search", "DuckDuckGo"],
  options: [
    { id: "auto", label: "Auto (free first, paid fallback)", ready: true },
    { id: "zai", label: "Z.AI Web Search Prime", ready: true },
    { id: "tavily", label: "Tavily", ready: false },
  ],
  keys: [
    {
      id: "brave",
      label: "Brave Search",
      env: "BRAVE_API_KEY",
      ready: false,
      hint: "search.brave.com",
    },
  ],
  vision: {
    backend: "gemini_analyze",
    configured: "auto",
    label: "Gemini analyze",
    hint: "Call image_analyze with a local file path.",
    source: "yaml",
    options: [{ id: "auto", label: "Auto", ready: true }],
  },
};

describe("searchSetup", () => {
  it("describes Auto vision as model-first", () => {
    expect(DEFAULT_VISION_OPTIONS[0].label).toBe(
      "Auto (native multimodal if model supports it, else same-provider vision sibling, else Gemini)",
    );
  });

  it("parses the provider-setup search payload", () => {
    const parsed = parseSearchSetup(SAMPLE);
    expect(parsed?.backend).toBe("zai");
    expect(parsed?.configured).toBe("auto");
    expect(parsed?.options).toHaveLength(3);
    expect(parsed?.options[2]).toEqual({ id: "tavily", label: "Tavily", ready: false });
    expect(parsed?.keys?.[0]?.env).toBe("BRAVE_API_KEY");
    expect(parsed?.vision?.backend).toBe("gemini_analyze");
    expect(parsed?.fallbacks).toEqual(["Brave Search", "DuckDuckGo"]);
  });

  it("explains missing search keys for the Settings CTA", () => {
    expect(searchNeedsKeyHint({ id: "brave", label: "Brave Search", ready: false })).toBe(
      "Brave needs an API key — add it in Settings → Search",
    );
  });

  it("unwraps search from a provider-setup or POST body", () => {
    expect(parseSearchFromProviderSetup({ search: SAMPLE })?.backend).toBe("zai");
    expect(parseSearchFromProviderSetup(SAMPLE)?.label).toBe("Z.AI Web Search Prime");
    expect(parseSearchFromProviderSetup({ ok: true })).toBeNull();
  });

  it("shows the resolved provider on Auto, the pin when locked", () => {
    const auto = parseSearchSetup(SAMPLE);
    expect(searchPillLabel(auto)).toBe("Z.AI");
    expect(searchPillTitle(auto)).toBe("Web search: Auto (Z.AI Web Search Prime)");
    expect(searchPillLabel(parseSearchSetup({ ...SAMPLE, configured: "tavily" }))).toBe(
      "Tavily",
    );
    expect(searchPillLabel(null)).toBe("Search");
    expect(searchShortLabel("perplexity")).toBe("Perplexity");
    expect(searchShortLabel("gemini_only")).toBe("Gemini only");
  });

  it("shortens the vision hop the same way", () => {
    const auto = parseSearchSetup(SAMPLE);
    expect(visionPillLabel(auto)).toBe("Gemini");
    expect(
      visionPillLabel(
        parseSearchSetup({
          ...SAMPLE,
          vision: { ...SAMPLE.vision, configured: "native", backend: "native" },
        }),
      ),
    ).toBe("Model vision");
  });
});
