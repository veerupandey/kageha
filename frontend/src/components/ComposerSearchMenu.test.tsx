import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ComposerSearchMenu } from "./ComposerSearchMenu";
import { parseSearchSetup, type SearchSetup } from "../lib/searchSetup";
import { useAppStore } from "../store";
import { DEFAULT_PREFS } from "../store/prefs";

const SAMPLE = vi.hoisted(() => ({
  backend: "zai",
  configured: "auto",
  label: "Z.AI Web Search Prime",
  source: "yaml",
  fallbacks: ["Brave Search"],
  options: [
    { id: "auto", label: "Auto (free first, paid fallback)", ready: true },
    { id: "zai", label: "Z.AI Web Search Prime", ready: true },
    { id: "tavily", label: "Tavily", ready: true },
    { id: "brave", label: "Brave Search", ready: false },
  ],
  vision: {
    backend: "gemini_analyze",
    configured: "auto",
    label: "Gemini analyze",
    source: "yaml",
    options: [
      { id: "auto", label: "Auto (same-provider sibling, else Gemini, else the model)", ready: true },
      { id: "gemini_analyze", label: "Gemini analyze", ready: true },
    ],
  },
}));

vi.mock("../api/client", () => ({
  api: vi.fn(async (path: string, opts?: RequestInit) => {
    if (path === "/api/provider-setup/search") {
      const body = JSON.parse(String(opts?.body || "{}")) as {
        backend?: string;
        vision?: string;
      };
      return {
        ok: true,
        search: {
          ...SAMPLE,
          configured: body.backend || SAMPLE.configured,
          backend: body.backend && body.backend !== "auto" ? body.backend : "zai",
          label:
            body.backend === "tavily"
              ? "Tavily"
              : body.backend && body.backend !== "auto"
                ? body.backend
                : SAMPLE.label,
          vision: {
            ...SAMPLE.vision,
            configured: body.vision || SAMPLE.vision.configured,
            backend:
              body.vision && body.vision !== "auto" ? body.vision : SAMPLE.vision.backend,
            label: body.vision === "gemini_analyze" ? "Gemini analyze" : SAMPLE.vision.label,
          },
        },
      };
    }
    return { ok: true };
  }),
}));

function resetStore(patch: Partial<SearchSetup> = {}) {
  useAppStore.setState({
    searchSetup: { ...parseSearchSetup(SAMPLE)!, ...patch },
    settingsTab: null,
    prefs: { ...DEFAULT_PREFS },
    defaultModel: "glm-5.3",
    modelOverride: "",
    modelCatalog: [{ id: "glm-5.3", label: "GLM 5.3", family: "GLM", ready: true }],
    loadSearch: async () => true,
  });
}

function openMenu() {
  const trigger = screen.getByRole("button", { name: "Web search and vision hop" });
  fireEvent.pointerDown(trigger, { button: 0, pointerId: 1, pointerType: "mouse" });
  fireEvent.pointerUp(trigger, { button: 0, pointerId: 1, pointerType: "mouse" });
  fireEvent.click(trigger);
}

describe("ComposerSearchMenu", () => {
  beforeAll(() => {
    class FakePointerEvent extends Event {
      button: number;
      ctrlKey: boolean;
      pointerType: string;
      constructor(type: string, props: Record<string, unknown> = {}) {
        super(type, props);
        this.button = Number(props.button || 0);
        this.ctrlKey = Boolean(props.ctrlKey);
        this.pointerType = String(props.pointerType || "mouse");
      }
    }
    window.PointerEvent = FakePointerEvent as unknown as typeof PointerEvent;
    HTMLElement.prototype.hasPointerCapture = () => false;
    HTMLElement.prototype.releasePointerCapture = () => {};
    HTMLElement.prototype.setPointerCapture = () => {};
    HTMLElement.prototype.scrollIntoView = () => {};
  });

  beforeEach(() => {
    vi.clearAllMocks();
    resetStore();
  });

  afterEach(() => {
    cleanup();
    resetStore();
  });

  it("shows the resolved search on the chat-bar pill", () => {
    resetStore();
    render(<ComposerSearchMenu />);
    expect(screen.getByRole("button", { name: "Web search and vision hop" })).toHaveTextContent(
      "Z.AI search",
    );
  });

  it("selects Codex hosted search for a Codex Runtime model and restores the saved provider", () => {
    useAppStore.setState({
      modelOverride: "gpt-codex-terra",
      modelCatalog: [
        {
          id: "gpt-codex-terra",
          provider: "openai-codex-app",
          label: "GPT 5.6 Terra",
          family: "Codex",
          capabilities: ["web_search"],
          ready: true,
        },
      ],
    });
    render(<ComposerSearchMenu />);
    const trigger = screen.getByRole("button", { name: "Web search and vision hop" });
    expect(trigger).toHaveTextContent("Codex");
    openMenu();
    fireEvent.click(screen.getByText("Search"));
    expect(screen.getByText("Codex hosted search")).toBeInTheDocument();
    expect(screen.getByText(/Switch models to restore your tool search engine/)).toBeInTheDocument();
    expect(useAppStore.getState().searchSetup?.configured).toBe("auto");

    act(() => {
      useAppStore.setState({ modelOverride: "glm-5.3" });
    });
    expect(trigger).toHaveTextContent("Z.AI search");
  });

  it("pins Tavily from the search panel", async () => {
    resetStore();
    render(<ComposerSearchMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Search"));
    fireEvent.click(screen.getByText("Tavily"));
    await waitFor(() => expect(useAppStore.getState().searchSetup?.configured).toBe("tavily"));
  });

  it("opens Settings when a search needs a key", () => {
    resetStore();
    render(<ComposerSearchMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Search"));
    fireEvent.click(screen.getByText("Brave Search"));
    expect(useAppStore.getState().settingsTab).toBe("search");
    expect(useAppStore.getState().searchSetup?.configured).toBe("auto");
  });

  it("pins a vision hop from the nested panel", async () => {
    resetStore();
    render(<ComposerSearchMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Vision hop"));
    fireEvent.click(screen.getByText("Gemini analyze"));
    await waitFor(() =>
      expect(useAppStore.getState().searchSetup?.vision?.configured).toBe("gemini_analyze"),
    );
  });

  it("shows an env lock instead of letting the user switch", () => {
    resetStore({ source: "env" });
    render(<ComposerSearchMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Search"));
    expect(screen.getByText(/Pinned by KAGEHA_WEB_SEARCH/)).toBeInTheDocument();
    const tavily = screen.getByText("Tavily").closest("[role='menuitem']");
    expect(tavily).toHaveAttribute("aria-disabled", "true");
  });

  it("lets vision change when only search is env-locked", async () => {
    resetStore({ source: "env" });
    render(<ComposerSearchMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Vision hop"));
    fireEvent.click(screen.getByText("Gemini analyze"));
    await waitFor(() =>
      expect(useAppStore.getState().searchSetup?.vision?.configured).toBe("gemini_analyze"),
    );
    expect(useAppStore.getState().searchSetup?.configured).toBe("auto");
  });

  it("opens Search settings from the chat-bar menu", () => {
    resetStore();
    render(<ComposerSearchMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Search settings"));
    expect(useAppStore.getState().settingsTab).toBe("search");
  });
});
