import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api/client";
import { parseSearchSetup } from "../lib/searchSetup";
import { useAppStore } from "./index";

const { SAMPLE, searchState } = vi.hoisted(() => {
  const SAMPLE = {
    backend: "zai",
    configured: "auto",
    label: "Z.AI Web Search Prime",
    source: "yaml",
    fallbacks: ["DuckDuckGo"],
    options: [
      { id: "auto", label: "Auto", ready: true },
      { id: "tavily", label: "Tavily", ready: true },
    ],
    vision: {
      backend: "gemini_analyze",
      configured: "auto",
      label: "Gemini analyze",
      options: [{ id: "auto", label: "Auto", ready: true }],
    },
  };
  return { SAMPLE, searchState: { current: SAMPLE } };
});

function cloneSample() {
  return {
    ...SAMPLE,
    options: SAMPLE.options.map((row) => ({ ...row })),
    vision: { ...SAMPLE.vision, options: SAMPLE.vision.options.map((row) => ({ ...row })) },
  };
}

vi.mock("../api/client", () => ({
  api: vi.fn(async (path: string, opts?: RequestInit) => {
    if (path === "/api/provider-setup/search") {
      const body = JSON.parse(String(opts?.body || "{}")) as {
        backend?: string;
        vision?: string;
      };
      const prev = searchState.current;
      const configured = body.backend ?? prev.configured;
      const visionConfigured = body.vision ?? prev.vision.configured;
      searchState.current = {
        ...prev,
        configured,
        backend: configured !== "auto" ? configured : "zai",
        label: configured === "tavily" ? "Tavily" : prev.label,
        vision: {
          ...prev.vision,
          configured: visionConfigured,
          backend: visionConfigured !== "auto" ? visionConfigured : prev.vision.backend,
          label: visionConfigured === "native" ? "Selected model vision only" : prev.vision.label,
        },
      };
      return { ok: true, search: searchState.current };
    }
    return { ok: true };
  }),
  ApiError: class ApiError extends Error {},
}));

describe("search setup store", () => {
  beforeEach(() => {
    searchState.current = cloneSample();
    useAppStore.setState({ searchSetup: null });
    vi.stubGlobal(
      "fetch",
      vi.fn(async (path: string) => {
        if (String(path).includes("/api/provider-setup")) {
          return {
            ok: true,
            status: 200,
            json: async () => ({ search: SAMPLE }),
          };
        }
        return { ok: false, status: 404, json: async () => ({}) };
      }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("loads search status from provider-setup", async () => {
    await useAppStore.getState().loadSearch();
    expect(useAppStore.getState().searchSetup?.backend).toBe("zai");
    expect(useAppStore.getState().searchSetup?.label).toBe("Z.AI Web Search Prime");
  });

  it("saves a pinned backend and refreshes status", async () => {
    useAppStore.setState({ searchSetup: parseSearchSetup(SAMPLE) });
    await useAppStore.getState().setSearch({ backend: "tavily" });
    expect(useAppStore.getState().searchSetup?.configured).toBe("tavily");
    expect(useAppStore.getState().searchSetup?.label).toBe("Tavily");
  });

  it("vision-only patch does not reset a pinned search backend", async () => {
    useAppStore.setState({ searchSetup: parseSearchSetup(SAMPLE) });
    await useAppStore.getState().setSearch({ backend: "tavily" });
    await useAppStore.getState().setSearch({ vision: "native" });
    expect(vi.mocked(api).mock.calls.at(-1)?.[1]).toEqual(
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ vision: "native" }),
      }),
    );
    expect(useAppStore.getState().searchSetup?.configured).toBe("tavily");
    expect(useAppStore.getState().searchSetup?.vision?.configured).toBe("native");
  });
});
