import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ComposerModelMenu } from "./ComposerModelMenu";
import { SidebarFooter } from "./Sidebar/SidebarFooter";
import { useAppStore } from "../store";
import { DEFAULT_PREFS } from "../store/prefs";
import type { ModelInfo } from "../api/types";

vi.mock("../api/client", () => ({
  api: vi.fn(async (path: string) => {
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
        ],
        models: [{ id: "glm-5.3", provider: "zai", model: "GLM-5.3", ready: true }],
        default_model: "glm-5.3",
        roles: { default: "glm-5.3", planner: "glm-5.3", worker: "glm-5.3" },
      };
    }
    return { ok: true };
  }),
}));

const CATALOG: ModelInfo[] = [
  { id: "gemini-flash", family: "Gemini", label: "3.7 Flash", latest: true, ready: true },
  { id: "gemini-pro", family: "Gemini", label: "3.1 Pro", latest: false, ready: true },
  { id: "glm-5.3", family: "GLM", label: "GLM 5.3", latest: true, ready: true },
  {
    id: "gpt-5.6-sol",
    family: "OpenAI",
    label: "GPT 5.6 Sol",
    ready: false,
    capabilities: ["tool_calling"],
  },
  {
    id: "gpt-codex-app",
    provider: "openai-codex-app",
    family: "Codex",
    label: "GPT 5.6 Sol",
    ready: true,
    capabilities: ["tool_calling", "vision", "reasoning"],
  },
  {
    id: "gpt-codex-terra",
    provider: "openai-codex-app",
    family: "Codex",
    label: "GPT 5.6 Terra",
    ready: true,
    capabilities: ["tool_calling", "vision", "reasoning"],
  },
  {
    id: "claude-fable",
    family: "Anthropic",
    label: "Claude Fable 5",
    ready: false,
    capabilities: ["tool_calling"],
  },
];

function resetStore() {
  useAppStore.setState({
    modelCatalog: CATALOG,
    models: CATALOG.filter((row) => row.ready !== false).map((row) => row.id),
    defaultModel: "gemini-flash",
    modelOverride: "gemini-flash",
    settingsTab: null,
    prefs: { ...DEFAULT_PREFS, mainModel: "gemini-flash", reasoningEffort: "medium" },
  });
}

function openMenu() {
  const trigger = screen.getByRole("button", { name: "Model and reasoning effort" });
  fireEvent.pointerDown(trigger, { button: 0, pointerId: 1, pointerType: "mouse" });
  fireEvent.pointerUp(trigger, { button: 0, pointerId: 1, pointerType: "mouse" });
  fireEvent.click(trigger);
}

describe("ComposerModelMenu", () => {
  it("puts tool-capable models first and hides unverified executor models until requested", () => {
    resetStore();
    useAppStore.setState({
      modelCatalog: [
        { id: "legacy", provider: "openai", label: "GPT Legacy", ready: true, latest: true, capabilities: [] },
        { id: "frontier-alias", model: "gpt-frontier", provider: "openai", label: "Duplicate Alias", ready: true, capabilities: ["tool_calling"] },
        { id: "frontier", model: "gpt-frontier", provider: "openai", label: "GPT Frontier", ready: true, capabilities: ["tool_calling"] },
      ],
      modelOverride: "frontier",
      prefs: { ...DEFAULT_PREFS, mainModel: "frontier" },
    });
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    expect(screen.getByRole("menuitem", { name: "GPT Frontier" })).toBeInTheDocument();
    expect(screen.queryByText("Duplicate Alias")).not.toBeInTheDocument();
    expect(screen.queryByText("GPT Legacy")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("More models (1)"));
    expect(screen.getByText("GPT Legacy").closest('[role="menuitem"]')).toHaveAttribute("data-disabled");
    fireEvent.change(screen.getByLabelText("Search models"), { target: { value: "Legacy" } });
    expect(screen.getByText("GPT Legacy")).toBeInTheDocument();
  });

  it("allows a ready text-only model in Planner", () => {
    resetStore();
    useAppStore.setState({ modelCatalog: [{ id: "text", provider: "openai", label: "Text Model", ready: true, capabilities: [] }] });
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Planner"));
    fireEvent.click(screen.getByText("Text Model"));
    expect(useAppStore.getState().prefs.plannerModel).toBe("text");
    expect(useAppStore.getState().modelOverride).toBe("gemini-flash");
  });
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

  afterEach(() => {
    cleanup();
    resetStore();
  });

  it("shows the current model on the composer pill", () => {
    resetStore();
    render(<ComposerModelMenu />);
    expect(screen.getByRole("button", { name: "Model and reasoning effort" })).toHaveTextContent(
      "3.7 Flash",
    );
  });

  it("opens Model and Reasoning effort rows", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    expect(screen.getByText("Executor")).toBeInTheDocument();
    expect(screen.getByText("Reasoning effort")).toBeInTheDocument();
    expect(screen.getByText("Medium")).toBeInTheDocument();
  });

  it("pins a sibling model from the family submenu", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    fireEvent.click(screen.getByText("3.1 Pro"));
    expect(useAppStore.getState().modelOverride).toBe("gemini-pro");
    expect(useAppStore.getState().prefs.mainModel).toBe("gemini-pro");
  });

  it("shows one Command Code group with a Free badge and selects either protocol", () => {
    resetStore();
    useAppStore.setState({
      modelCatalog: [
        { id: "beam", provider: "commandcode", label: "Beam 1.0", free: true, pricing_note: "Free while capacity lasts", capabilities: ["tool_calling"] },
        { id: "claude", provider: "commandcode-messages", label: "Claude Sonnet", capabilities: ["tool_calling"] },
      ],
      modelOverride: "claude",
      prefs: { ...DEFAULT_PREFS, mainModel: "claude" },
    });
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    expect(screen.getAllByText("Command Code")).toHaveLength(1);
    expect(screen.getByText("Free")).toBeInTheDocument();
    expect(screen.getByText("Free while capacity lasts")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Beam 1.0"));
    expect(useAppStore.getState().modelOverride).toBe("beam");
  });

  it("pins a planner model independently of the executor", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Planner"));
    // Search forces the matching provider group open regardless of seed.
    fireEvent.change(screen.getByLabelText("Search models"), {
      target: { value: "3.1 Pro" },
    });
    fireEvent.click(screen.getByText("3.1 Pro"));
    // Planner slot is set; the executor pin is untouched.
    expect(useAppStore.getState().prefs.plannerModel).toBe("gemini-pro");
    expect(useAppStore.getState().modelOverride).toBe("gemini-flash");
    expect(useAppStore.getState().prefs.mainModel).toBe("gemini-flash");
  });

  it("persists reasoning effort from the submenu", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Reasoning effort"));
    fireEvent.click(screen.getByText("High"));
    expect(useAppStore.getState().prefs.reasoningEffort).toBe("high");
  });

  it("opens Models settings from Manage models & keys", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    fireEvent.click(screen.getByText("OpenAI"));
    expect(screen.getAllByText("Needs key").length).toBeGreaterThan(0);
    expect(screen.getByText("Manage models & keys")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Manage models & keys"));
    expect(useAppStore.getState().settingsTab).toBe("models");
    expect(useAppStore.getState().modelOverride).toBe("gemini-flash");
  });

  it("opens Models settings when a Needs key row is clicked", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    fireEvent.click(screen.getByText("Anthropic"));
    fireEvent.click(screen.getByText("Claude Fable 5"));
    expect(useAppStore.getState().settingsTab).toBe("models");
    expect(useAppStore.getState().modelOverride).toBe("gemini-flash");
  });

  it("selects the Codex app-server runtime from the composer", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    fireEvent.click(screen.getByText("Codex Runtime"));
    expect(screen.getByText("Live web")).toBeInTheDocument();
    expect(screen.getByText("GPT 5.6 Sol")).toBeInTheDocument();
    expect(screen.getByText("GPT 5.6 Terra")).toBeInTheDocument();
    expect(screen.queryByText("OAuth + web search")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("GPT 5.6 Terra"));
    expect(useAppStore.getState().modelOverride).toBe("gpt-codex-terra");
    expect(useAppStore.getState().prefs.mainModel).toBe("gpt-codex-terra");
  });

  it("collapses and expands provider groups", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    expect(screen.getByText("Z.AI")).toBeInTheDocument();
    expect(screen.getByText("Google Gemini")).toBeInTheDocument();
    expect(screen.queryByText("GLM")).not.toBeInTheDocument();
    expect(screen.getByText("3.1 Pro")).toBeInTheDocument();
    fireEvent.click(screen.getByText("Google Gemini"));
    expect(screen.queryByText("3.1 Pro")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("OpenAI"));
    expect(screen.getByText("GPT 5.6 Sol")).toBeInTheDocument();
  });

  it("opens Models settings from Manage models & keys in the model panel", () => {
    resetStore();
    render(<ComposerModelMenu />);
    openMenu();
    fireEvent.click(screen.getByText("Executor"));
    fireEvent.click(screen.getByText("Manage models & keys"));
    expect(useAppStore.getState().settingsTab).toBe("models");
  });

  it("shows the Settings dialog from Models & keys", async () => {
    resetStore();
    render(
      <>
        <SidebarFooter />
        <ComposerModelMenu />
      </>,
    );
    openMenu();
    fireEvent.click(screen.getByText("Models & keys"));
    await waitFor(() => expect(screen.getByLabelText("Settings dialog")).toBeInTheDocument());
    expect(screen.getByRole("heading", { name: "Models" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByLabelText("OpenAI API key")).toBeInTheDocument());
    expect(screen.getByLabelText("Anthropic API key")).toBeInTheDocument();
  });
});
