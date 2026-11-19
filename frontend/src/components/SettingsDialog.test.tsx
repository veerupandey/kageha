import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { api } from "../api/client";
import { SettingsDialog } from "./SettingsDialog";
import { SidebarFooter } from "./Sidebar/SidebarFooter";
import { parseSearchSetup } from "../lib/searchSetup";
import { useAppStore } from "../store";
import { DEFAULT_PREFS } from "../store/prefs";
import { THEMES } from "../lib/themes";

const SEARCH = vi.hoisted(() => ({
  backend: "zai",
  configured: "auto",
  label: "Z.AI Web Search Prime",
  source: "yaml",
  fallbacks: ["Brave Search", "Gemini Search", "DuckDuckGo"],
  options: [
    { id: "auto", label: "Auto (free first, paid fallback)", ready: true },
    { id: "zai", label: "Z.AI Web Search Prime", ready: true },
    { id: "tavily", label: "Tavily", ready: true },
    { id: "brave", label: "Brave Search", ready: false },
  ],
  keys: [
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
      ready: true,
      hint: "app.tavily.com",
    },
  ],
  vision: {
    backend: "gemini_analyze",
    configured: "auto",
    label: "Gemini analyze",
    hint: "Call image_analyze with a local file path.",
    source: "yaml",
    options: [
      { id: "auto", label: "Auto (same-provider sibling, else Gemini, else the model)", ready: true },
      { id: "gemini_analyze", label: "Gemini analyze", ready: true },
    ],
  },
}));

const searchState = vi.hoisted(() => ({ current: SEARCH }));

vi.mock("../api/client", () => ({
  api: vi.fn(async (path: string, opts?: RequestInit) => {
    if (path === "/api/models/fallback") {
      return { policy: { enabled: true, providers: {}, models: {} },
        providers: ["zai"], models: [{ id: "glm-5.3", provider: "zai" }] };
    }
    if (path === "/api/provider-setup") {
      return {
        connections: [
          {
            id: "openai",
            label: "OpenAI",
            kind: "api_key",
            ready: false,
            hint: "Paste an API key from platform.openai.com",
          },
          {
            id: "anthropic",
            label: "Anthropic",
            kind: "api_key",
            ready: false,
            hint: "Paste an API key from console.anthropic.com",
          },
          {
            id: "siliconflow",
            label: "SiliconFlow",
            kind: "api_key",
            ready: false,
            hint: "Kimi, DeepSeek, and Qwen on one key",
          },
        ],
        models: [{ id: "glm-5.3", provider: "zai", model: "GLM-5.3", ready: true }],
        default_model: "glm-5.3",
        roles: { default: "glm-5.3", planner: "glm-5.3", worker: "glm-5.3" },
        search: searchState.current,
      };
    }
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
        label:
          configured === "tavily"
            ? "Tavily"
            : configured !== "auto"
              ? configured
              : prev.label,
        vision: {
          ...prev.vision,
          configured: visionConfigured,
          backend:
            visionConfigured !== "auto" ? visionConfigured : prev.vision.backend,
          label:
            visionConfigured === "gemini_analyze"
              ? "Gemini analyze"
              : prev.vision.label,
        },
      };
      return { ok: true, search: searchState.current };
    }
    return { ok: true };
  }),
}));

describe("Settings", () => {
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
    searchState.current = {
      ...SEARCH,
      options: SEARCH.options.map((row) => ({ ...row })),
      vision: {
        ...SEARCH.vision,
        options: SEARCH.vision.options.map((row) => ({ ...row })),
      },
    };
    useAppStore.setState({
      models: ["glm-5.3"],
      defaultModel: "glm-5.3",
      modelOverride: "",
      prefs: { ...DEFAULT_PREFS },
      modelCatalog: [{ id: "glm-5.3", label: "GLM 5.3", family: "GLM", ready: true }],
      searchSetup: parseSearchSetup(SEARCH),
      loadModels: async () => {},
      loadSearch: async () => true,
    });
  });

  afterEach(() => {
    // Reset store state that mounted components subscribe to *before*
    // unmounting, wrapped in act() so the re-render doesn't warn.
    act(() => {
      useAppStore.setState({ settingsTab: null });
    });
    cleanup();
  });

  it("puts default, planner, worker, and compatible API connections in Settings", async () => {
    render(
      <SettingsDialog tab="models" onClose={() => {}} onTabChange={() => {}} />,
    );
    await waitFor(() => expect(screen.getByText("Who does what")).toBeInTheDocument());
    expect(screen.getByText("Planner")).toBeInTheDocument();
    expect(screen.getByText("Worker")).toBeInTheDocument();
    expect(screen.getByText("Add a compatible API endpoint")).toBeInTheDocument();
    expect(screen.getByText("Add a model")).toBeInTheDocument();
    expect(screen.getByText("Ollama")).toBeInTheDocument();
    expect(screen.getByLabelText("Host for new model")).toHaveValue("openai");
    expect(screen.getAllByText("SiliconFlow").length).toBeGreaterThan(0);
    expect(screen.getByLabelText("OpenAI API key")).toBeInTheDocument();
    expect(screen.getByLabelText("Anthropic API key")).toBeInTheDocument();
  });

  it("shows the active web search and vision hop", async () => {
    render(
      <SettingsDialog tab="search" onClose={() => {}} onTabChange={() => {}} />,
    );
    await waitFor(() => expect(screen.getByText("Using Z.AI Web Search Prime")).toBeInTheDocument());
    expect(
      screen.getByRole("radio", { name: "Auto (free first, paid fallback)" }),
    ).toHaveAttribute("aria-checked", "true");
    expect(
      screen.getByRole("radio", { name: "Auto (same-provider sibling, else Gemini, else the model)" }),
    ).toHaveAttribute("aria-checked", "true");
    expect(screen.getByText(/Falls back to Brave Search/)).toBeInTheDocument();
    expect(screen.getAllByText("Gemini analyze").length).toBeGreaterThan(0);
    expect(screen.getByRole("radio", { name: "Brave Search" })).toHaveTextContent("Key");
    expect(screen.getByText("API keys")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Paste BRAVE_API_KEY")).toBeInTheDocument();
  });

  it("explains that Codex Runtime uses its native hosted search", async () => {
    useAppStore.setState({
      modelOverride: "gpt-codex-terra",
      modelCatalog: [
        {
          id: "gpt-codex-terra",
          provider: "openai-codex-app",
          label: "GPT 5.6 Terra",
          family: "Codex",
          ready: true,
          capabilities: ["tool_calling", "vision", "reasoning", "web_search"],
        },
      ],
    });
    render(
      <SettingsDialog tab="search" onClose={() => {}} onTabChange={() => {}} />,
    );
    expect(await screen.findByText("Using Codex hosted search")).toBeInTheDocument();
    expect(
      screen.getByText(/Codex hosted search is active for the selected model/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Switch to another model to restore Z.AI Web Search Prime/)).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Codex hosted search" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("blocks unready search chips until a key is pasted", async () => {
    render(
      <SettingsDialog tab="search" onClose={() => {}} onTabChange={() => {}} />,
    );
    const brave = await screen.findByRole("radio", { name: "Brave Search" });
    fireEvent.click(brave);
    await waitFor(() =>
      expect(screen.getByText(/Brave needs an API key/)).toBeInTheDocument(),
    );
    await waitFor(() =>
      expect(screen.getByPlaceholderText("Paste BRAVE_API_KEY")).toHaveFocus(),
    );
    expect(vi.mocked(api)).not.toHaveBeenCalledWith(
      "/api/provider-setup/search",
      expect.objectContaining({
        body: JSON.stringify({ backend: "brave" }),
      }),
    );
    expect(screen.getByRole("radio", { name: "Auto (free first, paid fallback)" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("pins Tavily from the search chips", async () => {
    render(
      <SettingsDialog tab="search" onClose={() => {}} onTabChange={() => {}} />,
    );
    const tavily = await screen.findByRole("radio", { name: "Tavily" });
    fireEvent.click(tavily);
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/provider-setup/search",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ backend: "tavily" }),
        }),
      ),
    );
    expect(await screen.findByText("Using Tavily")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Tavily" })).toHaveAttribute("aria-checked", "true");
  });

  it("locks the chips when an env var pins search", async () => {
    render(
      <SettingsDialog tab="search" onClose={() => {}} onTabChange={() => {}} />,
    );
    await screen.findByRole("radio", { name: "Tavily" });
    act(() => {
      useAppStore.setState((state) => ({
        searchSetup: state.searchSetup
          ? { ...state.searchSetup, source: "env" }
          : state.searchSetup,
      }));
    });
    expect(screen.getByText(/KAGEHA_WEB_SEARCH/)).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Tavily" })).toBeDisabled();
    expect(
      screen.getByRole("radio", { name: "Gemini analyze" }),
    ).not.toBeDisabled();
  });

  it("pins vision without sending a search backend", async () => {
    render(
      <SettingsDialog tab="search" onClose={() => {}} onTabChange={() => {}} />,
    );
    fireEvent.click(await screen.findByRole("radio", { name: "Tavily" }));
    await screen.findByText("Using Tavily");
    fireEvent.click(screen.getByRole("radio", { name: "Gemini analyze" }));
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenLastCalledWith(
        "/api/provider-setup/search",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ vision: "gemini_analyze" }),
        }),
      ),
    );
    expect(screen.getByRole("radio", { name: "Tavily" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("radio", { name: "Gemini analyze" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("shows the provider next to each role model", async () => {
    render(
      <SettingsDialog tab="models" onClose={() => {}} onTabChange={() => {}} />,
    );
    const planner = await screen.findByLabelText("Planner model");
    expect(planner).toHaveTextContent("GLM 5.3");
    expect(planner).toHaveTextContent("Z.AI");
    fireEvent.pointerDown(planner, { button: 0, pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerUp(planner, { button: 0, pointerId: 1, pointerType: "mouse" });
    fireEvent.click(planner);
    const menu = await screen.findByRole("menu");
    expect(within(menu).getByText("Z.AI")).toBeInTheDocument();
    expect(within(menu).getByRole("menuitem", { name: "GLM 5.3" })).toBeInTheDocument();
  });

  it("posts OpenAI and Anthropic keys without selecting a model", async () => {
    render(
      <SettingsDialog tab="models" onClose={() => {}} onTabChange={() => {}} />,
    );
    await waitFor(() => expect(screen.getByLabelText("OpenAI API key")).toBeInTheDocument());

    fireEvent.change(screen.getByLabelText("OpenAI API key"), {
      target: { value: "sk-openai-test" },
    });
    fireEvent.click(
      within(screen.getByLabelText("OpenAI API key").closest("div") as HTMLElement).getByRole(
        "button",
        { name: "Save" },
      ),
    );
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/provider-setup/key",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ provider: "openai", api_key: "sk-openai-test" }),
        }),
      ),
    );

    fireEvent.change(screen.getByLabelText("Anthropic API key"), {
      target: { value: "sk-ant-test" },
    });
    fireEvent.click(
      within(screen.getByLabelText("Anthropic API key").closest("div") as HTMLElement).getByRole(
        "button",
        { name: "Save" },
      ),
    );
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/provider-setup/key",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ provider: "anthropic", api_key: "sk-ant-test" }),
        }),
      ),
    );
  });

  it("posts a new model onto the chosen host", async () => {
    render(
      <SettingsDialog tab="models" onClose={() => {}} onTabChange={() => {}} />,
    );
    const host = await screen.findByLabelText("Host for new model");
    fireEvent.change(host, { target: { value: "siliconflow" } });
    fireEvent.change(screen.getByLabelText("New model id"), {
      target: { value: "moonshotai/Kimi-K2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/provider-setup/model",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ provider: "siliconflow", model: "moonshotai/Kimi-K2" }),
        }),
      ),
    );
  });

  it("opens settings from the sidebar footer", async () => {
    render(<SidebarFooter />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    });
    await waitFor(() => expect(screen.getByLabelText("Settings dialog")).toBeInTheDocument());
    expect(screen.getByRole("heading", { name: "Models" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Search" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Extensions" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Appearance" })).toBeInTheDocument();
    // The default Models tab lazy-loads SettingsDialog, then ModelsSettings
    // fetches /api/provider-setup on mount. Wait for content that only renders
    // after that response so every trailing state update settles in act().
    await waitFor(() => expect(screen.getByLabelText("OpenAI API key")).toBeInTheDocument());
  });

  it("keeps search off the Models tab", async () => {
    render(
      <SettingsDialog tab="models" onClose={() => {}} onTabChange={() => {}} />,
    );
    await waitFor(() => expect(screen.getByText("Who does what")).toBeInTheDocument());
    expect(screen.queryByLabelText("Web search provider")).not.toBeInTheDocument();
  });

  it.each(THEMES)("selects $label in Appearance and applies it immediately", (theme) => {
    render(<SettingsDialog tab="appearance" onClose={() => {}} onTabChange={() => {}} />);
    fireEvent.click(screen.getByRole("radio", { name: theme.label }));
    expect(useAppStore.getState().prefs.theme).toBe(theme.id);
    expect(document.documentElement).toHaveAttribute("data-theme", theme.id);
    expect(document.documentElement).toHaveAttribute("data-color-scheme", theme.scheme);
    expect(screen.getByRole("radio", { name: theme.label })).toBeChecked();
  });
});

it("exposes decision models in a dedicated Settings section", () => {
  const changeTab = vi.fn();
  render(<SettingsDialog tab="models" onClose={() => {}} onTabChange={changeTab} />);
  fireEvent.click(screen.getByRole("button", { name: "Decisions" }));
  expect(changeTab).toHaveBeenCalledWith("decisions");
  expect(screen.queryByRole("region", { name: "Decision model settings" })).not.toBeInTheDocument();
});
