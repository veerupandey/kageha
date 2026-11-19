import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { BrowserState } from "../api/types";
import { api } from "../api/client";
import { useAppStore } from "../store";
import { BrowserPanel } from "./BrowserPanel";
import { requestBrowserTarget } from "../lib/browserWorkspace";

vi.mock("./BrowserLiveSurface", async () => ({
  BrowserLiveSurface: (await import("./test_helpers/MockBrowserLiveSurface")).MockBrowserLiveSurface,
}));

vi.mock("../api/client", () => ({
  api: vi.fn(),
}));

const browserState: BrowserState = {
  sessionId: "session-1",
  bindingId: "instance:1",
  revision: 1,
  connected: true,
  backend: "chromium",
  url: "https://example.com/products",
  title: "Products",
  tabs: [
    { index: 0, title: "Products", url: "https://example.com/products", active: true },
    { index: 1, title: "Cart", url: "https://example.com/cart", active: false },
  ],
  lockedBy: "user",
  canControl: true,
  frame: { url: "/api/frame.png", bindingId: "instance:1" },
  viewport: { width: 1280, height: 800 },
};

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

describe("BrowserPanel browser stage", () => {
  beforeEach(() => {
    vi.mocked(api).mockResolvedValue(browserState);
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    useAppStore.setState({
      sessionId: "session-1",
      runStatus: "idle",
      machineActive: false,
      browserPanelMode: "expanded",
      workspaceSurface: "live",
      liveSurface: "browser",
      messages: [],
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    delete (window as { kageha?: unknown }).kageha;
    useAppStore.setState({ sessionId: null, messages: [] });
  });

  it("shows unavailable input separately from a connected live browser", async () => {
    vi.mocked(api).mockResolvedValue({...browserState,backend:"rust",inputStatus:{ready:false,reason:"journal_storage_full"}});
    render(<BrowserPanel />);
    expect(await screen.findByText("Browser input unavailable")).toBeVisible();
    expect(screen.getByText(/Page reads and live preview remain available/)).toBeVisible();
  });

  it("opens recorded browser context without navigating, switching tabs or taking control", async () => {
    const sessionId = "recorded-target-session";
    useAppStore.setState({ sessionId });
    vi.mocked(api).mockResolvedValue({
      ...browserState, sessionId, backend: "rust", targetIdentity: "rust:browser:products",
      tabs: browserState.tabs.map((tab, index) => ({ ...tab, tabHandle: index === 0 ? "products" : "cart" })),
    });
    render(<BrowserPanel />);
    const open = (tabId?: string) =>
      act(() => requestBrowserTarget(sessionId, tabId ? { browserId: "browser", tabId } : undefined));
    open("products");
    expect(await screen.findByText("Showing the recorded tab")).toBeVisible();
    open("cart");
    expect(screen.getByText("Recorded tab: Cart")).toBeVisible();
    expect(screen.getByRole("button", { name: "Switch to tab" })).toBeEnabled();
    open("closed");
    expect(screen.getByText("Recorded tab unavailable · Showing the current browser")).toBeVisible();
    open();
    expect(screen.getByText("Showing the current browser")).toBeVisible();
    expect(vi.mocked(api).mock.calls.some(([url]) => /\/browser\/(action|lease|navigate)$/.test(String(url)))).toBe(false);
  });

  it("shows the host-owned decision route in the browser control header", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, decisionRoute: {
      turn_id: "run-1", status: "routed", model: "jev", cutoff: 0.9, score_metric: "provider_confidence",
    } });
    render(<BrowserPanel />);
    const button = await screen.findByRole("button", { name: "Browser execution route" });
    expect(button).toHaveTextContent("Decision route");
    fireEvent.click(button);
    expect(await screen.findByText("Decision model: jev")).toBeVisible();
  });

  it("acknowledges native frames after paint and withdraws when the panel is hidden", async () => {
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    vi.mocked(api).mockResolvedValue({
      ...browserState, backend: "rust", previewAcknowledgment: true,
      screencast: { available: true }, controlGeneration: 1,
    });
    render(<BrowserPanel />);
    const image = await waitFor(() => {
      const node = document.querySelector<HTMLImageElement>('img[src*="/browser/stream"]');
      expect(node).not.toBeNull();
      return node!;
    });
    const reports = () => vi.mocked(api).mock.calls
      .filter(([url]) => String(url).endsWith("/browser/paint"))
      .map(([, options]) => JSON.parse(String(options?.body)));
    expect(reports()).not.toContainEqual({ bindingId: "instance:1", displayed: true });
    fireEvent.load(image);
    await waitFor(() => expect(reports()).toContainEqual({ bindingId: "instance:1", displayed: true }));
    act(() => useAppStore.setState({ workspaceSurface: "artifacts" }));
    await waitFor(() => expect(reports()).toContainEqual({ bindingId: "instance:1", displayed: false }));
  });

  it("offers the exact generated webpage when the chat browser is disconnected", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState, backend: "rust", selectedProfile: "comet", connected: false,
      lockedBy: null, tabs: [], url: "", title: "", frame: undefined,
    });
    const path = "artifacts/lora-lesson.html";
    useAppStore.setState({
      canvasItems: [{ path, kind: "webpage", url: "/api/file", caption: "LoRA" }],
      canvasSelectedPath: null,
    });
    try {
      render(<BrowserPanel />);
      fireEvent.click(await screen.findByRole("button", { name: "Preview generated webpage in Files" }));
      expect(useAppStore.getState().workspaceSurface).toBe("artifacts");
      expect(useAppStore.getState().canvasSelectedPath).toBe(path);
    } finally {
      useAppStore.setState({ canvasItems: [], canvasSelectedPath: null });
    }
  });

  it("keeps the exact retained session selected when two sessions use the same browser", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, browserSessions: [
      { id: "first", backend: "chromium", mode: "private", selected: false, connected: true, title: "Research" },
      { id: "second", backend: "chromium", mode: "private", selected: true, connected: true, title: "Draft" },
    ] });
    render(<BrowserPanel />);
    const selector = await screen.findByRole("combobox", { name: "Browser profile" });
    expect(selector).toHaveValue("session:second");
    expect(screen.getByRole("option", { name: "Private browser · 1 · Research" })).toBeInTheDocument();
    fireEvent.change(selector, { target: { value: "session:first" } });
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/promote",
      expect.objectContaining({ body: JSON.stringify({ browserSession: "first", bindingId: "instance:1" }) })));
  });

  it("claims the exact signed-in tab chosen from the current binding", async () => {
    const current: BrowserState = { ...browserState, backend: "rust", selectedProfile: "comet",
      canClaimUserTabs: true, controlGeneration: 3 };
    const candidate = { tabHandle: "tab-17", title: "Open application", url: "https://example.com/apply" };
    vi.mocked(api).mockImplementation(async (_path, options) => {
      const action = options?.body ? JSON.parse(String(options.body)).action : "";
      return action === "list_user_tabs"
        ? { ...current, revision: 2, userTabCandidates: [candidate] }
        : current;
    });

    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Use open tab" }));
    fireEvent.click(await screen.findByRole("button", { name: candidate.title }));

    expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/action", {
      method: "POST",
      body: JSON.stringify({ action: "claim_user_tab", ...candidate, candidateBindingId: "instance:1",
        bindingId: "instance:1", controlGeneration: 3 }),
    });
  });

  it("keeps the selected tab and frame when a Rust state poll times out", async () => {
    vi.useFakeTimers();
    try {
      const connected = { ...browserState, backend: "rust", selectedProfile: "comet" };
      vi.mocked(api).mockResolvedValue(connected);
      render(<BrowserPanel />);
      await act(async () => { await vi.advanceTimersByTimeAsync(1); });
      expect(screen.getByRole("tab", { name: "Products", selected: true })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Return control to agent" })).toBeEnabled();

      vi.mocked(api).mockResolvedValue({ sessionId: "session-1", backend: "rust", selectedProfile: "private",
        connected: false, stateUnavailable: true, tabs: [] });
      await act(async () => { await vi.advanceTimersByTimeAsync(6500); });
      expect(screen.getByText("Waiting for browser…")).toBeInTheDocument();
      expect(screen.getByRole("tab", { name: "Products", selected: true })).toBeInTheDocument();
      expect(document.querySelector('img[src*="/api/frame.png"]')).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Return control to agent" })).toBeDisabled();

      vi.mocked(api).mockResolvedValue({ ...connected, revision: 2 });
      await act(async () => { await vi.advanceTimersByTimeAsync(6500); });
      expect(screen.getByRole("button", { name: "Return control to agent" })).toBeEnabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("takes control before switching from an agent-owned browser to Comet", async () => {
    vi.mocked(api).mockImplementation(async (path) =>
      String(path).endsWith("/lease") ? browserState
        : { ...browserState, lockedBy: "agent" });
    render(<BrowserPanel />);
    const selector = await screen.findByRole("combobox", { name: "Browser profile" });
    expect(selector).toBeEnabled();
    fireEvent.change(selector, { target: { value: "comet" } });
    await waitFor(() => expect(api).toHaveBeenCalledWith(
      "/api/sessions/session-1/browser/promote", expect.objectContaining({ method: "POST" })));
    const writes = vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST");
    expect(writes.map(([path]) => path)).toEqual([
      "/api/sessions/session-1/browser/lease", "/api/sessions/session-1/browser/promote",
    ]);
    expect(JSON.parse(writes[1][1]!.body as string)).toMatchObject({ target: "comet", consent: true });
  });

  it("takes control before attaching a signed-in profile on the Rust backend", async () => {
    let current: BrowserState = { ...browserState, backend: "rust", selectedProfile: "private", lockedBy: "agent", agentLocked: true };
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (String(path).endsWith("/lease")) current = { ...current, lockedBy: JSON.parse(String(options?.body)).action === "take" ? "user" : null, agentLocked: false, controlGeneration: 8 };
      if (String(path).endsWith("/promote")) current = { ...current, selectedProfile: "comet", bindingId: "comet:1" };
      return current;
    });
    render(<BrowserPanel />);
    const profile = await screen.findByRole("combobox", { name: "Browser profile" });
    expect(profile).toBeEnabled();
    fireEvent.change(profile, { target: { value: "profile:comet" } });
    await waitFor(() => expect(profile).toHaveValue("profile:comet"));
    const writes = vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST");
    expect(writes.map(([path]) => String(path).split("/").at(-1))).toEqual(["lease", "promote", "lease"]);
    expect(JSON.parse(String(writes[2][1]?.body))).toMatchObject({ action: "return", controlGeneration: 8 });
    expect(JSON.parse(String(writes[1][1]?.body))).toMatchObject({ target: "profile:comet", consent: true });
  });

  it("pins the packaged runtime and offers its supported profiles in the existing picker", async () => {
    let current: BrowserState = {
      ...browserState, lockedBy: null, runtimeCanary: { runtime: "python", canTryRust: true },
    };
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (String(path).endsWith("/runtime")) {
        const { runtime } = JSON.parse(String(options?.body));
        current = runtime === "rust"
          ? { ...current, backend: "rust", selectedBackend: "rust", privateCanary: true,
            selectedProfile: "private", bindingId: "canary:empty:0", revision: 0, tabs: [],
            availableProfiles: ["private", "chrome", "comet"],
            runtimeCanary: { runtime: "rust", canTryRust: true } }
          : { ...browserState, lockedBy: null, runtimeCanary: { runtime: "python", canTryRust: true } };
      }
      return current;
    });
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Browser view settings" }));
    fireEvent.click(screen.getByText("Advanced browser options"));
    fireEvent.click(await screen.findByRole("button", { name: "Try Rust browser" }));
    const rollback = await screen.findByRole("button", { name: "Use Python browser" });
    expect(screen.getByRole("option", { name: "Comet profile" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Chrome profile" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "Alternate private browser" })).not.toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Private browser" })).toBeInTheDocument();
    fireEvent.click(rollback);
    await screen.findByRole("button", { name: "Try Rust browser" });
  });

  it("keeps runtime details out of the browser chrome while retaining reconnect", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState, backend: "rust", selectedProfile: "private", connected: false,
      tabs: [], url: "", title: "", frame: undefined, lockedBy: null,
      runtimeCanary: { runtime: "rust", canTryRust: true },
    });
    render(<BrowserPanel />);
    expect(await screen.findByRole("button", { name: "Reconnect" })).toBeEnabled();
    expect(screen.queryByText("Rust browser canary")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Use Python browser" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Browser view settings" }));
    fireEvent.click(screen.getByText("Advanced browser options"));
    expect(screen.getByRole("button", { name: "Use Python browser" })).toBeEnabled();
  });

  it("uses the workspace window controls when embedded", async () => {
    render(<BrowserPanel embeddedInWorkspace onCollapse={vi.fn()} />);
    await screen.findByDisplayValue("https://example.com/products");
    expect(screen.queryByRole("button", { name: "Collapse panel" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Maximize" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Browser view settings" })).toBeEnabled();
  });

  it.each([
    ["alternate", "Alternate private browser", false],
    ["chrome", "Chrome profile", true],
    ["comet", "Comet profile", true],
    ["cdp", "Custom CDP profile", true],
    ["docker", "Docker browser", false],
    ["machine", "Session computer browser", false],
    ["remote", "Remote browser", false],
    ["embedded", "Kageha profile", false],
  ])("switches to Rust %s in one request and drops the previous frame", async (target, label, consent) => {
    let current: BrowserState = {
      ...browserState, backend: "rust", previewOnly: true, selectedProfile: "private", lockedBy: null,
      privateCanary: target === "chrome" || target === "comet",
      availableProfiles: [target],
      frame: { url: "/api/sessions/session-1/browser/frame?binding=instance%3A1", bindingId: "instance:1" },
    };
    vi.mocked(api).mockImplementation(async (path) => {
      const operation = String(path).split("/").at(-1);
      if (operation === "promote") current = {
        ...current, bindingId: "instance:2", revision: 2, selectedProfile: target,
        frame: { url: "/api/sessions/session-1/browser/frame?binding=instance%3A2", bindingId: "instance:2" },
      };
      return current;
    });
    render(<BrowserPanel />);
    const profile = await screen.findByRole("combobox", { name: "Browser profile" });
    expect(profile).toHaveValue("profile:private");
    expect(screen.getByRole("option", { name: label })).toHaveValue(`profile:${target}`);
    expect(screen.queryByRole("button", { name: "Take control" })).not.toBeInTheDocument();
    const old = document.querySelector('img[src*="binding=instance%3A1"]');
    expect(old).toBeInTheDocument();
    fireEvent.load(old!);
    fireEvent.change(profile, { target: { value: `profile:${target}` } });
    await waitFor(() => expect(profile).toHaveValue(`profile:${target}`));
    await waitFor(() => expect(current.lockedBy).toBeNull());
    expect(document.querySelector('img[src*="binding=instance%3A1"]')).not.toBeInTheDocument();
    expect(document.querySelector('img[src*="binding=instance%3A2"]')).toBeInTheDocument();
    const writes = vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST");
    expect(writes.map(([path]) => String(path).split("/").at(-1))).toEqual(["promote"]);
    expect(JSON.parse(String(writes[0][1]?.body))).toMatchObject({ target: `profile:${target}`, consent, bindingId: "instance:1" });
  });

  it("reconnects the selected signed-in Rust profile explicitly", async () => {
    const disconnected: BrowserState = {
      ...browserState, backend: "rust", previewOnly: true, selectedProfile: "comet",
      connected: false, connectionStatus: "disconnected", lockedBy: null, tabs: [], url: "",
    };
    vi.mocked(api).mockResolvedValue(disconnected);
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Reconnect" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith(
      "/api/sessions/session-1/browser/promote",
      expect.objectContaining({ method: "POST" }),
    ));
    const writes = vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST");
    expect(JSON.parse(String(writes[0][1]?.body))).toMatchObject({
      target: "profile:comet", consent: true, bindingId: disconnected.bindingId,
    });
  });

  it("sends Rust input only after the selected frame has painted under user control", async () => {
    let current: BrowserState = {
      ...browserState, backend: "rust", selectedProfile: "chrome", lockedBy: "agent",
      screencast: { available: true }, viewport: { width: 800, height: 400 },
    };
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path.endsWith("/lease")) {
        const take = JSON.parse(String(options?.body)).action === "take";
        current = { ...current, lockedBy: take ? "user" : null, controlGeneration: take ? 7 : undefined };
      }
      return current;
    });
    const { container } = render(<BrowserPanel />);
    await screen.findByRole("button", { name: "Take control" });
    const surface = container.querySelector('img[src*="/browser/stream"]') as HTMLImageElement;
    expect(surface).toBeInTheDocument();
    vi.spyOn(surface, "getBoundingClientRect").mockReturnValue({
      left: 0, top: 0, width: 800, height: 600, x: 0, y: 0, right: 800, bottom: 600, toJSON: () => ({}),
    });
    const click = (y: number) => {
      fireEvent.mouseDown(surface, { clientX: 40, clientY: y, button: 0 });
      fireEvent.mouseUp(surface, { clientX: 40, clientY: y, button: 0 });
    };
    const actions = () => vi.mocked(api).mock.calls.filter(([path]) => path.endsWith("/action"));
    fireEvent.click(screen.getByRole("button", { name: "Take control" }));
    await waitFor(() => expect(current.lockedBy).toBe("user"));
    click(140);
    expect(actions()).toHaveLength(0);
    fireEvent.load(surface);
    click(30);
    expect(actions()).toHaveLength(0);
    click(140);
    await waitFor(() => expect(actions()).toHaveLength(1));
    expect(JSON.parse(String(actions()[0][1]?.body))).toMatchObject({
      action: "click", button: "left", x: 40, y: 40, bindingId: "instance:1", controlGeneration: 7,
    });
  });

  it("explains which selected profile the agent will follow on handback", async () => {
    const selected = { ...browserState, backend: "comet", selection: {
      revision: "selection-two", backend: "comet", source: "user" as const,
    } };
    vi.mocked(api).mockImplementation(async (path) =>
      String(path).endsWith("/lease") ? { ...selected, lockedBy: "agent" } : selected);
    render(<BrowserPanel />);
    await screen.findByText("Agent will follow Comet profile when you return control");
    fireEvent.click(screen.getByRole("button", { name: "Return control to agent" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith(
      "/api/sessions/session-1/browser/lease",
      expect.objectContaining({ body: JSON.stringify({ action: "return" }) })));
    await waitFor(() => expect(screen.queryByText(
      "Agent will follow Comet profile when you return control")).not.toBeInTheDocument());
  });

  it("keeps the current browser when takeover fails", async () => {
    vi.mocked(api).mockImplementation(async (path) => {
      if (String(path).endsWith("/lease")) throw new Error("Takeover unavailable");
      return { ...browserState, lockedBy: "agent" };
    });
    render(<BrowserPanel />);
    fireEvent.change(await screen.findByRole("combobox", { name: "Browser profile" }),
      { target: { value: "comet" } });
    await waitFor(() => expect(api).toHaveBeenCalledWith(
      "/api/sessions/session-1/browser/lease", expect.anything()));
    await screen.findByText("Takeover unavailable");
    expect(vi.mocked(api).mock.calls.some(([path]) => String(path).endsWith("/promote"))).toBe(false);
  });

  it("blocks old-page actions during a pending browser switch", async () => {
    let finish!: (state: BrowserState) => void;
    vi.mocked(api).mockImplementation(async (path) => String(path).endsWith("/promote")
      ? new Promise<BrowserState>(resolve => { finish = resolve; }) : browserState);
    render(<BrowserPanel />);
    const selector = await screen.findByRole("combobox", { name: "Browser profile" });
    fireEvent.change(selector, { target: { value: "comet" } });
    await screen.findByText("Switching browser session…");
    expect(selector).toBeDisabled();
    const calls = vi.mocked(api).mock.calls.length;
    fireEvent.click(screen.getByRole("tab", { name: "Cart" }));
    expect(vi.mocked(api).mock.calls.length).toBe(calls);
    await act(async () => finish(browserState));
    expect(screen.queryByText("Switching browser session…")).not.toBeInTheDocument();
  });

  it("pauses polling through attachment so old reads cannot hide a successful switch", async () => {
    vi.useFakeTimers();
    try {
      let finish!: (state: BrowserState) => void;
      let reads = 0;
      const destination = { ...browserState, backend: "comet", selectedBackend: "comet", bindingId: "instance:2", revision: 2 };
      vi.mocked(api).mockImplementation(async (path) => String(path).endsWith("/promote")
        ? new Promise<BrowserState>(resolve => { finish = resolve; })
        : (++reads, browserState));
      await act(async () => { render(<BrowserPanel />); });
      await act(async () => { fireEvent.change(screen.getByRole("combobox", { name: "Browser profile" }), { target: { value: "comet" } }); });
      const before = reads;
      await act(async () => { await vi.advanceTimersByTimeAsync(4500); });
      expect(reads).toBe(before);
      // New reads now reflect the selected destination.
      vi.mocked(api).mockResolvedValue(destination);
      await act(async () => finish(destination));
      expect(screen.getByRole("combobox", { name: "Browser profile" })).toHaveValue("comet");
    } finally { vi.useRealTimers(); }
  });

  it("retries the unavailable task tab without opening a replacement", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, connectionStatus: "target_unavailable",
      connected: false, url: "", frame: undefined });
    render(<BrowserPanel />);
    await screen.findByText("Task tab unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Retry browser connection" }));
    await waitFor(() => expect(vi.mocked(api).mock.calls.length).toBeGreaterThan(1));
    expect(vi.mocked(api).mock.calls.every(([path, options]) =>
      path === "/api/sessions/session-1/browser" && options?.method !== "POST")).toBe(true);
  });

  it("lets the user inspect an uncertain action without repeating it", async () => {
    vi.mocked(api).mockImplementation(async (path) => String(path).includes("tool-attempts")
      ? { arguments: {}, result: "Original action output", error: "", browser_receipts: [
        { id: "receipt", action: "click", step: null, state: "uncertain", dispatch: "started", verification: "unverified" },
      ] }
      : { ...browserState, pendingBrowserActions: [{ id: "receipt", attemptId: "attempt", action: "click", state: "uncertain" }] });
    render(<BrowserPanel />);
    await screen.findByRole("complementary", { name: "Browser action needs review" });
    expect(screen.queryByText("Browser connection details")).not.toBeInTheDocument();
    expect(screen.queryByText("Open a new tab to continue other browsing.")).not.toBeInTheDocument();
    expect(vi.mocked(api).mock.calls.some(([path]) => String(path).includes("tool-attempts"))).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Review action details" }));
    expect(screen.getByText("Open a new tab to continue other browsing.")).toBeVisible();
    expect(vi.mocked(api).mock.calls.some(([path]) => String(path).includes("tool-attempts"))).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Show technical log" }));
    await screen.findByText("Original action output");
    expect(vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST")).toHaveLength(0);
  });

  it("opens a separate recovery tab without replaying the earlier click", async () => {
    const pending = { ...browserState, pendingBrowserActions: [
      { id: "receipt", attemptId: "attempt", action: "click", state: "uncertain" },
    ] };
    vi.mocked(api).mockImplementation(async (path) =>
      String(path).endsWith("/action") ? { ...pending, bindingId: "instance:2", revision: 2, url: "about:blank" } : pending);
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Review action details" }));
    fireEvent.click(screen.getByRole("button", { name: "Open a new tab" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/action",
      expect.objectContaining({ body: JSON.stringify({ action: "tab_new", recovery: true, bindingId: "instance:1" }) })));
    expect(vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST")).toHaveLength(1);
  });

  it("shows that a receipt on another tab does not block the current tab", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, pendingBrowserActions: [
      { id: "receipt", attemptId: "attempt", action: "click", state: "uncertain", blocksCurrentTab: false },
    ] });
    render(<BrowserPanel />);
    expect(screen.queryByText("Unverified action on another tab")).not.toBeInTheDocument();
    fireEvent.click(await screen.findByRole("button", { name: "Review action details" }));
    expect(screen.getByText("Unverified action on another tab")).toBeVisible();
    expect(screen.getByText("You can continue unrelated browsing on this tab.")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Open a new tab" })).not.toBeInTheDocument();
  });

  it("keeps a same-binding address draft but resets it after a binding change", async () => {
    const destination = { ...browserState, bindingId: "instance:2", revision: 2, url: "https://example.com/cart", title: "Cart" };
    vi.mocked(api).mockImplementation(async (_path, options) => {
      const body = options?.body ? JSON.parse(String(options.body)) : {};
      return body.action === "tab_select" ? destination : browserState;
    });
    render(<BrowserPanel />);
    const address = await screen.findByRole("textbox", { name: "Address" });
    act(() => { address.focus(); });
    fireEvent.change(address, { target: { value: "https://draft.example/unsent" } });
    expect(address).toHaveValue("https://draft.example/unsent");
    fireEvent.click(screen.getByRole("tab", { name: "Cart" }));
    await waitFor(() => expect(address).toHaveValue(destination.url));
  });

  it("keeps reload busy until the request settles instead of a fixed timer", async () => {
    vi.useFakeTimers();
    try {
      let finish!: (value: BrowserState) => void;
      vi.mocked(api).mockImplementation(async (_path, options) => {
        const body = options?.body ? JSON.parse(String(options.body)) : {};
        return body.action === "reload" ? new Promise<BrowserState>(resolve => { finish = resolve; }) : browserState;
      });
      await act(async () => { render(<BrowserPanel />); });
      const reload = screen.getByRole("button", { name: "Reload" });
      await act(async () => { fireEvent.click(reload); });
      await act(async () => { await vi.advanceTimersByTimeAsync(700); });
      expect(reload).toBeDisabled();
      expect(reload).toHaveAttribute("aria-busy", "true");
      await act(async () => finish(browserState));
      expect(reload).not.toBeDisabled();
      expect(reload).toHaveAttribute("aria-busy", "false");
    } finally { vi.useRealTimers(); }
  });

  it("exposes passive reload accessibly without allowing it during agent input", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, lockedBy: "agent" });
    render(<BrowserPanel />);
    expect(await screen.findByRole("button", { name: "Reload" })).toBeDisabled();
  });

  it("selects the observed tab handle without sending a drifting index", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      tabs: browserState.tabs.map((tab) => ({ ...tab, tabHandle: `tab_stable_${tab.index}` })),
    });
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("tab", { name: "Cart" }));
    await waitFor(() => {
      const request = vi.mocked(api).mock.calls.find(([, options]) =>
        options?.body && JSON.parse(String(options.body)).action === "tab_select",
      );
      expect(request).toBeDefined();
      const body = JSON.parse(String(request?.[1]?.body));
      expect(body).toMatchObject({ tabHandle: "tab_stable_1", bindingId: "instance:1" });
      expect(body).not.toHaveProperty("index");
    });
  });

  it("does not acquire control through auto-resize when passively watched", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, lockedBy: null });
    render(<BrowserPanel />);
    await screen.findByRole("combobox", { name: "Browser profile" });
    expect(vi.mocked(api).mock.calls.some(([, options]) =>
      options?.body && JSON.parse(String(options.body)).action === "resize",
    )).toBe(false);
  });

  it("keeps a snapshot visible and releases an MJPEG request that never produces pixels", async () => {
    vi.useFakeTimers();
    try {
      await act(async () => { render(<BrowserPanel />); });
      const stream = document.querySelector('img[src*="/browser/stream"]');
      expect(stream).toHaveClass("opacity-0");
      expect(screen.getByTestId("browser-stream-fallback")).toHaveAttribute("src", expect.stringContaining("/api/frame.png"));
      await act(async () => { await vi.advanceTimersByTimeAsync(8000); });
      expect(document.querySelector('img[src*="/browser/stream"]')).not.toBeInTheDocument();
      expect(document.querySelector('img[src*="/api/frame.png"]')).toBeInTheDocument();
    } finally { vi.useRealTimers(); }
  });

  it("removes the fallback once the live stream paints", async () => {
    render(<BrowserPanel />);
    await screen.findByTestId("browser-stream-fallback");
    const stream = document.querySelector('img[src*="/browser/stream"]')!;
    fireEvent.load(stream);
    expect(stream).not.toHaveClass("opacity-0");
    expect(screen.queryByTestId("browser-stream-fallback")).not.toBeInTheDocument();
  });

  it.each([
    [{ backend: "chrome", url: "about:blank" }, "Chrome profile"],
    [{ backend: "rust", selectedProfile: "comet", tabs: [], url: "" }, "Comet profile"],
  ])("shows an empty page for a connected browser (%j)", async (emptyState, profile) => {
    vi.mocked(api).mockResolvedValue({ ...browserState, ...emptyState, title: "" });
    render(<BrowserPanel />);
    expect(await screen.findAllByText("No page open")).not.toHaveLength(0);
    expect(screen.getByText(new RegExp(`${profile} is connected`))).toBeVisible();
    expect(document.querySelector(".stage-boot-ring")).not.toBeInTheDocument();
    expect(document.querySelector('img[src*="/browser/stream"]')).not.toBeInTheDocument();
    const address = await screen.findByRole("textbox", { name: "Address" });
    fireEvent.change(address, { target: { value: "https://example.test/draft" } });
    expect(screen.getByText(new RegExp(`${profile} is connected`))).toBeVisible();
    expect(document.querySelector('img[src*="/browser/stream"]')).not.toBeInTheDocument();
    expect(vi.mocked(api).mock.calls.some(([path]) => path.endsWith("/action"))).toBe(false);
  });

  it("keeps browser control available on a connected blank page", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, url: "about:blank", title: "", lockedBy: "agent" });
    render(<BrowserPanel />);
    const button = await screen.findByRole("button", { name: "Take control" });
    expect(button).toBeVisible();
    expect(button).toBeEnabled();
    expect(button.closest("header")).toHaveAttribute("aria-label", "Browser control");
  });

  it("retains the recovered stream instead of reconnecting when the failure backoff resets", async () => {
    vi.useFakeTimers();
    try {
      await act(async () => { render(<BrowserPanel />); });
      const first = document.querySelector<HTMLImageElement>('img[src*="/browser/stream"]')!;
      fireEvent.error(first);
      await act(async () => { await vi.advanceTimersByTimeAsync(1600); });
      const recovered = document.querySelector<HTMLImageElement>('img[src*="/browser/stream"]')!;
      expect(recovered).not.toBeNull();
      const source = recovered.src;
      fireEvent.load(recovered);
      await act(async () => { await vi.advanceTimersByTimeAsync(9000); });
      expect(document.querySelector('img[src*="/browser/stream"]')).toBe(recovered);
      expect(recovered.src).toBe(source);
      expect(recovered).not.toHaveClass("opacity-0");
      expect(screen.queryByTestId("browser-stream-fallback")).not.toBeInTheDocument();
    } finally { vi.useRealTimers(); }
  });

  it("releases preview connections behind Files and restores them when Browser is visible", async () => {
    vi.useFakeTimers();
    try {
      await act(async () => { render(<BrowserPanel />); });
      expect(document.querySelector('img[src*="/browser/stream"]')).toBeInTheDocument();
      act(() => { useAppStore.setState({ workspaceSurface: "artifacts" }); });
      expect(document.querySelector('img[src*="/browser/stream"]')).not.toBeInTheDocument();
      const reads = vi.mocked(api).mock.calls.length;
      await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
      expect(api).toHaveBeenCalledTimes(reads);
      await act(async () => { useAppStore.setState({ workspaceSurface: "live" }); });
      expect(document.querySelector('img[src*="/browser/stream"]')).toBeInTheDocument();
    } finally { vi.useRealTimers(); }
  });

  it("requests responsive sizing without taking control and follows panel resizing", async () => {
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(862);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(848);
    const resizes: ResizeObserverCallback[] = [];
    vi.stubGlobal("ResizeObserver", class {
      constructor(callback: ResizeObserverCallback) { resizes.push(callback); }
      observe() {} disconnect() {}
    });
    let state = { ...browserState, backend: "comet", owned: false, lockedBy: "agent", agentLocked: true };
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (String(path).endsWith("/layout")) {
        const body = JSON.parse(String(options?.body));
        state = { ...state, viewport: { width: body.width, height: body.height } };
      }
      return state;
    });
    render(<BrowserPanel />);
    await waitFor(() => expect(screen.getByTestId("browser-page-canvas")).toHaveStyle({ width: "862px", height: "848px" }));
    await waitFor(() => expect(vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST")).toHaveLength(1));
    const writes = vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST");
    expect(writes[0][0]).toBe("/api/sessions/session-1/browser/layout");
    expect(JSON.parse(String(writes[0][1]?.body))).toEqual({ mode: "panel", width: 862, height: 848, bindingId: "instance:1" });
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(1000);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(900);
    act(() => resizes.forEach(resize => resize([{ contentRect: { width: 1000, height: 900 } }] as ResizeObserverEntry[], {} as ResizeObserver)));
    await waitFor(() => expect(vi.mocked(api).mock.calls.some(([path, options]) =>
      path.endsWith("/layout") && options?.body && JSON.parse(String(options.body)).width === 1000,
    )).toBe(true));
    expect(vi.mocked(api).mock.calls.some(([path]) => path.endsWith("/lease") || path.endsWith("/action"))).toBe(false);
  });

  it("keeps a failed layout request visible and retries it explicitly without taking control", async () => {
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(862);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(848);
    vi.mocked(api).mockImplementation(async (path) => {
      if (String(path).endsWith("/layout")) throw new Error("Could not update page layout");
      return { ...browserState, backend: "comet", owned: false, lockedBy: "agent", agentLocked: true };
    });
    render(<BrowserPanel />);
    await screen.findByText("Could not update page layout");
    const count = () => vi.mocked(api).mock.calls.filter(([path]) => path.endsWith("/layout")).length;
    expect(count()).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "Browser view settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Fit page to panel" }));
    await waitFor(() => expect(count()).toBe(2));
    expect(vi.mocked(api).mock.calls.some(([path]) => path.endsWith("/lease") || path.endsWith("/action"))).toBe(false);
  });

  it("restores native layout on a zoom choice without the legacy resize path competing", async () => {
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(700);
    let state: BrowserState = { ...browserState, backend: "comet", owned: false };
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path.endsWith("/layout")) {
        const body = JSON.parse(String(options?.body));
        state = { ...state, layout: {mode: body.mode}, viewport: body.mode === "panel"
          ? {width: body.width, height: body.height} : browserState.viewport };
      }
      return state;
    });
    render(<BrowserPanel />);
    await waitFor(() => expect(screen.getByTestId("browser-page-canvas")).toHaveStyle({width: "800px", height: "700px"}));
    fireEvent.click(screen.getByRole("button", { name: "Browser view settings" }));
    fireEvent.change(screen.getByRole("combobox", {name: "Preview zoom"}), {target: {value: "100"}});
    await waitFor(() => expect(screen.getByTestId("browser-page-canvas")).toHaveStyle({width: "1280px", height: "800px"}));
    expect(vi.mocked(api).mock.calls.filter(([,options]) => options?.method === "POST").every(([path]) => path.endsWith("/layout"))).toBe(true);
  });

  it("changes preview zoom while the agent works without resizing or taking over the page", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, lockedBy: "agent", agentLocked: true });
    render(<BrowserPanel />);
    await screen.findByRole("button", { name: "Copy URL" });
    fireEvent.click(screen.getByRole("button", { name: "Browser view settings" }));
    const zoom = await screen.findByRole("combobox", { name: "Preview zoom" });
    expect(zoom).toHaveValue("responsive");
    await act(async () => fireEvent.change(zoom, { target: { value: "100" } }));
    expect(screen.getByTestId("browser-page-canvas")).toHaveStyle({ width: "1280px", height: "800px" });
    await act(async () => fireEvent.change(zoom, { target: { value: "50" } }));
    expect(screen.getByTestId("browser-page-canvas")).toHaveStyle({ width: "640px", height: "400px" });
    expect(vi.mocked(api).mock.calls.some(([, options]) => options?.method === "POST")).toBe(false);
  });

  it("does not report clipboard success when the write is denied", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("Denied"));
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Copy URL" }));
    expect(await screen.findByText("Could not copy the URL. Copy it from the address bar.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Copied" })).not.toBeInTheDocument();
  });

  it("reports successful URL copies only after the clipboard resolves", async () => {
    let resolve!: () => void;
    const writeText = vi.fn().mockImplementation(() => new Promise<void>(r => { resolve = r; }));
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Copy URL" }));
    expect(screen.queryByRole("button", { name: "Copied" })).not.toBeInTheDocument();
    await act(async () => resolve());
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(writeText).toHaveBeenCalledWith(browserState.url);
  });

  it("does not send visited hostnames to a third-party favicon service", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, url: "https://private.internal/review" });
    const { container } = render(<BrowserPanel />);
    await screen.findByRole("button", { name: "Copy URL" });
    expect(container.innerHTML).not.toContain("google.com/s2/favicons");
    expect(container.querySelector('[style*="background-image"]')).toBeNull();
  });

  it("ignores a delayed state response after a newer binding arrives", async () => {
    vi.useFakeTimers();
    try {
      let resolveOld!: (value: BrowserState) => void;
      const delayed = new Promise<BrowserState>((resolve) => { resolveOld = resolve; });
      let reads = 0;
      const initial = { ...browserState, backend: "comet", selectedBackend: "comet", frame: undefined };
      vi.mocked(api).mockImplementation(async (_path, options) => {
        if (options?.method !== "POST") return reads++ === 0 ? initial : delayed;
        return { ...initial, bindingId: "instance:2", revision: 2, title: "Current tab" };
      });
      await act(async () => { render(<BrowserPanel />); });
      await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
      expect(reads).toBe(2);
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Open outside" })); });
      await act(async () => resolveOld({ ...initial, title: "Old private tab" }));
      expect(screen.getByRole("combobox", { name: "Browser profile" })).toHaveValue("comet");
      expect(screen.queryByText("Old private tab")).not.toBeInTheDocument();
      expect(screen.getAllByText("Current tab").length).toBeGreaterThan(0);
    } finally { vi.useRealTimers(); }
  });

  it("keeps one status read in flight and does not invent a private browser during restoration", async () => {
    vi.useFakeTimers();
    try {
      let finish!: (value: BrowserState) => void;
      vi.mocked(api).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
      useAppStore.setState({ runStatus: "running" });
      render(<BrowserPanel />);
      await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
      expect(api).toHaveBeenCalledTimes(1);
      expect(screen.getByText("Connecting to this chat’s browser")).toBeVisible();
      expect(screen.queryByText(/Private browser/)).not.toBeInTheDocument();
      expect(screen.queryByText("Disconnected")).not.toBeInTheDocument();
      vi.mocked(api).mockResolvedValue({ ...browserState, backend: "comet", owned: false });
      await act(async () => { finish({ ...browserState, backend: "comet", owned: false }); });
      expect(screen.getByRole("combobox", { name: "Browser profile" })).toHaveValue("comet");
      expect(screen.queryByText("Connecting to this chat’s browser")).not.toBeInTheDocument();
    } finally { vi.useRealTimers(); }
  });

  it("offers browser recovery after a failed initial read without opening another browser", async () => {
    vi.mocked(api).mockRejectedValueOnce(new Error("Browser status timed out"));
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Retry browser connection" }));
    expect(await screen.findByRole("button", { name: "Copy URL" })).toBeInTheDocument();
    expect(vi.mocked(api).mock.calls.some(([, options]) => options?.method === "POST")).toBe(false);
  });

  it("waits for the recorded tab during backend restoration instead of reconnecting a default browser", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(api).mockResolvedValue({ ...browserState, backend: "comet", selectedBackend: "comet",
        connected: false, connectionStatus: "restoring", frame: undefined, url: "", tabs: [] });
      render(<BrowserPanel />);
      await act(async () => { await vi.advanceTimersByTimeAsync(15_000); });
      expect(screen.getByText("Connecting to this chat’s browser")).toBeVisible();
      expect(screen.queryByText(/Private browser/)).not.toBeInTheDocument();
      expect(vi.mocked(api).mock.calls.some(([, options]) => options?.method === "POST")).toBe(false);
      vi.mocked(api).mockResolvedValue({ ...browserState, backend: "comet", owned: false });
      await act(async () => { await vi.advanceTimersByTimeAsync(2500); });
      expect(screen.getByRole("combobox", { name: "Browser profile" })).toHaveValue("comet");
      fireEvent.click(screen.getByRole("button", { name: "Browser view settings" }));
      expect(screen.getByRole("button", { name: "Fit page to panel" })).toBeVisible();
    } finally { vi.useRealTimers(); }
  });

  it("lets the host browser reload the app while passively watching a task", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, backend: "comet", owned: false, lockedBy: "agent", agentLocked: true });
    render(<BrowserPanel />);
    await screen.findByRole("button", { name: "Copy URL" });
    expect(fireEvent.keyDown(window, { key: "r", metaKey: true, cancelable: true })).toBe(true);
    expect(fireEvent.keyDown(window, { key: "l", metaKey: true, cancelable: true })).toBe(true);
    expect(vi.mocked(api).mock.calls.some(([, options]) => options?.method === "POST")).toBe(false);
  });

  it("rejects a browser response belonging to a different chat", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, sessionId: "other-chat",
      backend: "comet", title: "Unrelated billing", url: "https://billing.invalid" });
    render(<BrowserPanel />);
    await act(async () => {});
    expect(screen.queryByText("Unrelated billing")).not.toBeInTheDocument();
    expect(document.querySelector("img.browser-stage-shot")).not.toBeInTheDocument();
    expect(vi.mocked(api).mock.calls.some(([, options]) => options?.method === "POST")).toBe(false);
  });

  it("rejects an old response after switching away and back to the same chat", async () => {
    let resolveOld!: (value: BrowserState) => void;
    const old = new Promise<BrowserState>((resolve) => { resolveOld = resolve; });
    vi.mocked(api).mockReturnValue(new Promise(() => {})).mockReturnValueOnce(old);
    render(<BrowserPanel />);
    await waitFor(() => expect(api).toHaveBeenCalled());
    await act(async () => useAppStore.setState({ sessionId: "session-2" }));
    await act(async () => useAppStore.setState({ sessionId: "session-1" }));
    await act(async () => resolveOld({ ...browserState, backend: "comet", title: "Old billing tab" }));
    // The fresh request is still pending; the first visit cannot paint it.
    expect(screen.queryByText("Old billing tab")).not.toBeInTheDocument();
  });

  it("sends input only after painting the current binding", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, bindingId: "instance:3", revision: 3,
      frame: { url: "/api/current.png", bindingId: "instance:3" } });
    render(<BrowserPanel />);
    await screen.findByRole("combobox", { name: "Browser profile" });
    await waitFor(() => expect(document.querySelector("img")?.src).toContain("binding=instance%3A3"));
    const image = document.querySelector("img")!;
    fireEvent.mouseDown(image, { button: 0, clientX: 10, clientY: 10 });
    expect(vi.mocked(api).mock.calls.some(([, options]) => options?.body && JSON.parse(String(options.body)).action === "mousedown")).toBe(false);
    fireEvent.load(image);
    fireEvent.mouseDown(image, { button: 0, clientX: 10, clientY: 10 });
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/action",
      expect.objectContaining({ body: expect.stringContaining('"bindingId":"instance:3"') })));
  });

  it("does not stream or switch a DOM-only browser when watched", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, backend: "lightpanda", frame: undefined });
    render(<BrowserPanel />);
    expect(await screen.findByText("DOM browser connected")).toBeInTheDocument();
    expect(document.querySelector(".stage-boot-ring")).toBeNull();
    expect(document.querySelector("img")).toBeNull();
    expect(vi.mocked(api).mock.calls.every(([path]) => !String(path).endsWith("/promote"))).toBe(true);
  });

  it("selects a profile explicitly with consent and the current binding", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, bindingId: "instance:3", revision: 3, selectedBackend: "chromium" });
    render(<BrowserPanel />);
    fireEvent.change(await screen.findByRole("combobox", { name: "Browser profile" }), { target: { value: "comet" } });
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/promote",
      expect.objectContaining({ body: JSON.stringify({ target: "comet", url: browserState.url, consent: true, bindingId: "instance:3" }) })));
  });

  it("restores a retained browser session without carrying the source URL", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, browserSessions: [
      { id: "browser_private", backend: "chromium", mode: "private", selected: true, connected: true },
      { id: "browser_personal", backend: "chrome", mode: "personal", selected: false, connected: true },
    ] });
    render(<BrowserPanel />);
    fireEvent.change(await screen.findByRole("combobox", { name: "Browser profile" }), {
      target: { value: "session:browser_personal" },
    });
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/promote",
      expect.objectContaining({ body: JSON.stringify({ browserSession: "browser_personal", bindingId: "instance:1" }) })));
  });

  it("dismisses a pending sign-in after successful revocation", async () => {
    const resolve = vi.fn();
    useAppStore.setState({ pendingApproval: { approval_id: "login-1", action: "request_secure_login", sessionId: "session-1" }, resolveApproval: resolve });
    let current: BrowserState = { ...browserState, loginAccess: { status: "waiting" } };
    vi.mocked(api).mockImplementation(async (path) => {
      if (String(path).includes("revoke-login")) {
        current = { ...browserState, revision: 2, loginAccess: { status: "revoked" } };
      }
      return current;
    });
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke access" }));
    await waitFor(() => expect(resolve).toHaveBeenCalledWith(false));
    expect(await screen.findByText("Agent browser access stopped")).toBeInTheDocument();
    act(() => useAppStore.setState({ pendingApproval: null }));
  });

  it("returns from the login browser to the approval in chat", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, loginAccess: { status: "waiting" } });
    useAppStore.setState({ prefs: { ...useAppStore.getState().prefs, rightPanelCollapsed: false } });
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Back to chat to approve" }));
    expect(useAppStore.getState().prefs.rightPanelCollapsed).toBe(true);
  });

  it("shows the account handoff in the active Rust browser panel", async () => {
    const resolve = vi.fn();
    vi.mocked(api).mockResolvedValue({ ...browserState, backend: "rust", lockedBy: null,
      tabs: [{ ...browserState.tabs[0], tabHandle: "account-tab", active: true }] });
    useAppStore.setState({ pendingApproval: { approval_id: "account-1", action: "request_secure_login", sessionId: "other-chat" },
      resolveApproval: resolve, prefs: { ...useAppStore.getState().prefs, rightPanelCollapsed: false } });
    render(<BrowserPanel />);
    await screen.findByText("Browser ready");
    expect(screen.queryByText("Complete the account step here")).not.toBeInTheDocument();
    act(() => useAppStore.setState({
      pendingApproval: { approval_id: "account-2", action: "request_secure_login", sessionId: "session-1",
        detail: JSON.stringify({ rust_tab: "account-tab" }) },
    }));
    expect(await screen.findByText("Waiting for you")).toBeInTheDocument();
    expect(screen.getByText("Complete the account step here")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Back to chat to resume" }));
    expect(useAppStore.getState().prefs.rightPanelCollapsed).toBe(true);
    expect(resolve).not.toHaveBeenCalled();
    act(() => useAppStore.setState({ pendingApproval: null }));
  });

  it("offers revocation for an approved browser session", async () => {
    vi.mocked(api).mockResolvedValue({ ...browserState, loginAccess: { status: "active" } });
    render(<BrowserPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Browser access" }));
    fireEvent.click(await screen.findByRole("button", { name: "Revoke access" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/revoke-login", expect.objectContaining({ method: "POST" })));
  });

  it("renders immersive controls and routes workspace actions", async () => {
    const onCollapse = vi.fn();

    render(
      <BrowserPanel

        onCollapse={onCollapse}
      />,
    );

    expect(await screen.findByDisplayValue("https://example.com/products")).toBeInTheDocument();
    expect(document.querySelector("section.browser-stage-docked")).toBeInTheDocument();
    expect(document.querySelector(".stage-window")).toBeInTheDocument();
    expect(document.querySelector(".stage-hud-wrap")?.classList.contains("is-hidden")).toBe(false);
    const live = document.querySelector("img.object-contain") as HTMLImageElement | null;
    expect(live).toBeInTheDocument();
    expect(live?.getAttribute("src")).toBe("/api/sessions/session-1/browser/stream?binding=instance%3A1");
    expect(api).not.toHaveBeenCalledWith(
      "/api/sessions/session-1/browser/connect",
      expect.anything(),
    );
    expect(screen.getByText("You have control")).toBeVisible();
    expect(screen.getAllByRole("button", { name: "Return control to agent" })).toHaveLength(1);
    expect(screen.getByRole("tablist", { name: "Browser tabs" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Trace" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Files" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Collapse panel" }));
    expect(onCollapse).toHaveBeenCalledOnce();

    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/action",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ action: "back", bindingId: "instance:1" }) }),
      );
    });
  });

  it("returns the browser lease from the control footer", async () => {
    render(<BrowserPanel />);

    await screen.findByDisplayValue("https://example.com/products");
    fireEvent.click(screen.getByRole("button", { name: "Return control to agent" }));

    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/lease",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ action: "return" }),
        }),
      );
    });
  });

  it("embeds Comet in the panel and offers open outside", async () => {
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (String(path).includes("/api/comet")) return { ok: true, action: "focus", message: "Opened Comet" };
      return {
        ...browserState,
        backend: "comet",
        owned: false,
        url: "https://www.amazon.ca/dp/B0H7817LL7",
      };
    });

    render(<BrowserPanel />);

    expect(await screen.findByRole("button", { name: "Open outside" })).toBeInTheDocument();
    expect(screen.queryByText("Comet")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pop out panel" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Open outside" }));
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/action",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ action: "focus", bindingId: "instance:1" }) }),
      );
    });
    expect(document.querySelector("img.object-contain")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open outside" })).toHaveAttribute(
      "title",
      "Open in Comet",
    );
  });

  it("focuses Chrome through the session browser action", async () => {
    vi.mocked(api).mockImplementation(async () => ({
      ...browserState,
      backend: "chrome",
      owned: false,
    }));

    render(<BrowserPanel />);

    const open = await screen.findByRole("button", { name: "Open outside" });
    expect(open).toHaveAttribute("title", "Open in Chrome");
    expect(document.querySelector("img.object-contain")).toBeInTheDocument();

    fireEvent.click(open);
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/action",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ action: "focus", bindingId: "instance:1" }) }),
      );
    });
    expect(api).not.toHaveBeenCalledWith(
      "/api/comet",
      expect.objectContaining({ body: JSON.stringify({ action: "focus", bindingId: "instance:1" }) }),
    );
  });

  it("pops out generic CDP instead of activating Comet", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      backend: "cdp",
      owned: false,
    });

    render(<BrowserPanel />);

    const open = await screen.findByRole("button", { name: "Open outside" });
    expect(open).toHaveAttribute("title", "Pop out panel");
    expect(document.querySelector("img.object-contain")).toBeInTheDocument();

    fireEvent.click(open);
    expect(useAppStore.getState().browserPanelMode).toBe("popup");
    expect(api).not.toHaveBeenCalledWith(
      "/api/comet",
      expect.objectContaining({ body: JSON.stringify({ action: "focus", bindingId: "instance:1" }) }),
    );
  });

  it("aligns external-browser highlights with the contained screenshot", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      backend: "comet",
      owned: false,
      viewport: { width: 1000, height: 800 },
      highlight: { x: 0, y: 0, width: 1000, height: 800 },
    });

    render(<BrowserPanel />);
    expect(await screen.findByRole("button", { name: "Open outside" })).toBeInTheDocument();
    const img = document.querySelector("img");
    expect(img).toBeTruthy();
    Object.defineProperty(img, "naturalWidth", { configurable: true, value: 1000 });
    Object.defineProperty(img, "naturalHeight", { configurable: true, value: 800 });
    vi.spyOn(img as HTMLImageElement, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: 200,
      bottom: 80,
      width: 200,
      height: 80,
      toJSON() {
        return {};
      },
    });
    fireEvent.load(img as HTMLImageElement);

    await waitFor(() => {
      const highlight = document.querySelector("[data-testid=browser-highlight]");
      expect(highlight).toBeInTheDocument();
      expect(highlight).toHaveStyle({ left: "50px", top: "0px", width: "100px", height: "80px" });
    });
  });

  it("points Chromium at the live MJPEG stream", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      backend: "chrome",
      owned: false,
      lockedBy: "agent",
      frame: undefined,
    });
    useAppStore.setState({
      sessionId: "session-1",
      runStatus: "running",
      messages: [
        {
          id: "a1",
          role: "assistant",
          text: "",
          computerFrames: [
            {
              app: "Kageha Browser",
              action: "evaluate",
              path: "artifacts/browser_live.jpg",
              url: "/api/sessions/session-1/files/artifacts/browser_live.jpg?v=99",
            },
          ],
        },
      ],
    });

    render(<BrowserPanel />);

    const img = await waitFor(() => {
      const node = document.querySelector("img.object-contain") as HTMLImageElement | null;
      expect(node?.getAttribute("src")).toBe("/api/sessions/session-1/browser/stream?binding=instance%3A1");
      return node;
    });
    expect(img).toBeTruthy();
  });

  it("shows the URL in the watching address bar and the page title on its tab", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      lockedBy: "agent",
      title: "Triphala Navel Oil",
      url: "https://bareandfair.com/products/oil",
    });

    render(<BrowserPanel />);

    expect(await screen.findByText("bareandfair.com/products/oil")).toBeInTheDocument();
    expect(screen.queryByText("Agent driving")).not.toBeInTheDocument();
    expect(document.querySelector(".browser-stage-livebar")).not.toBeInTheDocument();
    expect(document.querySelector(".stage-window")).toBeInTheDocument();
    expect(document.querySelector(".stage-hud-wrap")).not.toHaveClass("is-hidden");
    expect(document.querySelector(".stage-hud-wrap")).not.toHaveClass("is-floating");
    expect(screen.queryByRole("button", { name: "Back" })).not.toBeInTheDocument();
    expect(screen.getByRole("tablist", { name: "Browser tabs" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Triphala Navel Oil" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: "Maximize" })).toBeEnabled();
  });

  it("shows the live stream instead of a waiting screenshot for Chromium", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      backend: "chrome",
      owned: false,
      lockedBy: "agent",
      url: "https://bareandfair.com/products/oil",
      frame: undefined,
    });
    useAppStore.setState({ sessionId: "session-1", runStatus: "running", messages: [] });

    render(<BrowserPanel />);

    await waitFor(() => {
      const node = document.querySelector("img.object-contain") as HTMLImageElement | null;
      expect(node?.getAttribute("src")).toBe("/api/sessions/session-1/browser/stream?binding=instance%3A1");
      // liveStreamUrl's effect resets streamReady after mount; load after that flush.
      fireEvent.load(node as HTMLImageElement);
      expect(screen.queryByText("Opening bareandfair.com/products/oil…")).not.toBeInTheDocument();
    });
    expect(document.querySelector(".stage-window")).toBeInTheDocument();
  });

  it("waits for a successful state read before reconnecting after a poll error", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(api).mockRejectedValue(new Error("state unavailable"));
      render(<BrowserPanel />);
      await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
      expect(vi.mocked(api).mock.calls.some(([path]) => path.endsWith("/connect"))).toBe(false);
      vi.mocked(api).mockResolvedValue({ ...browserState, connected: false,
        url: "", title: "", frame: undefined });
      await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
      await act(async () => { await vi.advanceTimersByTimeAsync(1); });
      expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/connect",
        expect.objectContaining({ body: JSON.stringify({ bindingId: "instance:1" }) }));
    } finally {
      vi.useRealTimers();
    }
  });

  it("warms Chromium only after it knows the session is empty", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      connected: false,
      url: "",
      title: "",
      frame: undefined,
    });

    render(<BrowserPanel />);

    expect(await screen.findByText("Ask to open a page, or type a URL")).toBeInTheDocument();
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/connect",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ bindingId: "instance:1" }),
        }),
      );
    });
  });

  it("skips the browser stream when noVNC is active", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      novncUrl: "http://127.0.0.1:9/vnc.html?autoconnect=1",
    });

    render(<BrowserPanel />);

    const frame = await screen.findByTitle("This computer's screen");
    expect(frame).toBeInTheDocument();
    expect(document.querySelector("img")).toBeNull();
  });

  it("falls back to the file frame when the live stream errors", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      backend: "chromium",
      frame: { url: "/api/sessions/session-1/files/artifacts/browser_live.jpg?v=3", bindingId: "instance:1" },
    });

    render(<BrowserPanel />);

    const live = await waitFor(() => {
      const node = document.querySelector("img.object-contain") as HTMLImageElement | null;
      expect(node?.getAttribute("src")).toBe("/api/sessions/session-1/browser/stream?binding=instance%3A1");
      return node;
    });
    fireEvent.error(live as HTMLImageElement);

    await waitFor(() => {
      const fallback = document.querySelector("img.object-contain") as HTMLImageElement | null;
      expect(fallback?.getAttribute("src")).toContain("browser_live.jpg");
    });
  });

  it("lets an agent-owned disconnected Chrome profile recover through Comet", async () => {
    const failed = { ...browserState, connected: false, backend: "chrome",
      selectedBackend: "chrome", lockedBy: "agent", canControl: false, frame: undefined,
      error: "Chrome profile is disconnected at http://127.0.0.1:9223",
      decision: { backend: "chrome", reason: "explicit_user_profile_backend", privateProfile: false, requiresConsent: true } };
    let attached = false;
    vi.mocked(api).mockImplementation(async (path) => {
      if (String(path).endsWith("/lease")) return { ...failed, lockedBy: "user", canControl: true };
      if (String(path).endsWith("/promote")) attached = true;
      return attached ? { ...browserState, backend: "comet", selectedBackend: "comet", owned: false,
        decision: { ...failed.decision, backend: "comet" } } : failed;
    });
    render(<BrowserPanel />);
    expect(await screen.findByRole("button", { name: "Use Comet" })).toBeEnabled();
    expect(screen.queryByText(/Browser profile connected/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Use Comet" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/session-1/browser/promote",
      expect.objectContaining({ body: JSON.stringify({ target: "comet", url: browserState.url, consent: true, bindingId: "instance:1" }) })));
    await waitFor(() => expect(screen.getByRole("combobox", {name: "Browser profile"})).toHaveValue("comet"));
    expect(screen.queryByText(/Browser profile connected/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Use Comet" })).not.toBeInTheDocument();
  });

  it("offers Use Comet, Use Chrome, and Stay private when login is required", async () => {
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      loginRequired: true,
      loginReason: "password field or sign-in prompt detected",
      recommendedBackend: "comet",
      decision: {
        backend: "comet",
        reason: "login_required; user browser requires approval",
        privateProfile: false,
        requiresConsent: true,
      },
    });

    render(<BrowserPanel />);

    expect(await screen.findByText(/Use a logged-in browser/)).toBeInTheDocument();
    expect(screen.getByTestId("browser-consent-bar").closest(".browser-stage-page")).toBeNull();
    expect(screen.getByTestId("browser-consent-bar").closest(".stage-hud-wrap")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Use Comet" }));
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/promote",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({
            target: "comet",
            url: "https://example.com/products",
            consent: true,
            bindingId: "instance:1",
          }),
        }),
      );
    });

    fireEvent.click(screen.getByRole("button", { name: "Stay private" }));
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/promote",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({
            target: "chromium",
            url: "https://example.com/products",
            consent: false,
            bindingId: "instance:1",
          }),
        }),
      );
    });
  });

  it("keeps Use Comet clickable and shows an attach failure in the bar", async () => {
    const login = {
      ...browserState,
      loginRequired: true,
      loginReason: "password field or sign-in prompt detected",
      recommendedBackend: "comet",
      decision: {
        backend: "comet",
        reason: "login_required; user browser requires approval",
        privateProfile: false,
        requiresConsent: true,
      },
    };
    const failed = {
      ...login,
      result: "ERROR: Comet CDP is not reachable at http://127.0.0.1:9222",
      error: "ERROR: Comet CDP is not reachable at http://127.0.0.1:9222",
    };
    let attached = false;
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (String(path).endsWith("/promote")) {
        attached = true;
        return failed;
      }
      return attached ? { ...login, error: failed.error } : login;
    });

    render(<BrowserPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Use Comet" }));
    expect(await screen.findByText(/Comet CDP is not reachable/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Use Comet" })).not.toBeDisabled();
  });

  it("types into the page and sends pointer events without a type bar", async () => {
    render(<BrowserPanel />);

    const img = await waitFor(() => {
      const node = document.querySelector("img.object-contain") as HTMLImageElement | null;
      expect(node).toBeTruthy();
      return node as HTMLImageElement;
    });
    expect(screen.queryByPlaceholderText("Type into the page")).not.toBeInTheDocument();

    fireEvent.load(img);
    fireEvent.mouseDown(img, { clientX: 20, clientY: 20, button: 0 });
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/action",
        expect.objectContaining({
          method: "POST",
          body: expect.stringContaining('"action":"mousedown"'),
        }),
      );
    });

    fireEvent.keyDown(img, { key: "a" });
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/action",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ action: "type", text: "a", bindingId: "instance:1" }),
        }),
      );
    });

    fireEvent.keyDown(img, { key: "Enter" });
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/action",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ action: "press", key: "Enter", bindingId: "instance:1" }),
        }),
      );
    });
  });

  it("renders the embedded guest slot instead of the MJPEG image", async () => {
    const setGuestBounds = vi.fn();
    const setGuestInteractive = vi.fn();
    vi.stubGlobal("kageha", { embedded: true, setGuestBounds, setGuestInteractive });
    Object.defineProperty(window, "kageha", {
      configurable: true,
      value: { embedded: true, setGuestBounds, setGuestInteractive },
    });
    vi.mocked(api).mockResolvedValue({
      ...browserState,
      backend: "embedded",
      connected: false,
      frame: undefined,
    });

    render(<BrowserPanel />);

    expect(await screen.findByTestId("embedded-guest")).toBeInTheDocument();
    expect(document.querySelector("img.browser-stage-shot")).not.toBeInTheDocument();
    expect(document.querySelector("img.object-contain")).not.toBeInTheDocument();
    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/browser/connect",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ bindingId: "instance:1" }),
        }),
      );
    });
  });

  it("toggles between desktop and mobile studio canvas modes", async () => {
    render(<BrowserPanel />);

    fireEvent.click(await screen.findByRole("button", { name: "Browser view settings" }));
    const desktopBtn = screen.getByRole("radio", { name: "Desktop view" });
    const mobileBtn = screen.getByRole("radio", { name: "Mobile view" });
    expect(desktopBtn).toHaveAttribute("aria-checked", "true");
    expect(mobileBtn).toHaveAttribute("aria-checked", "false");

    await act(async () => {
      fireEvent.click(mobileBtn);
    });
    expect(mobileBtn).toHaveAttribute("aria-checked", "true");
    expect(desktopBtn).toHaveAttribute("aria-checked", "false");
    expect(document.querySelector(".browser-stage-page")).toHaveClass("is-mobile-canvas");
    expect(screen.getByRole("radiogroup", { name: "Device view" })).toBeVisible();

    await act(async () => {
      fireEvent.click(desktopBtn);
    });
    expect(desktopBtn).toHaveAttribute("aria-checked", "true");
    expect(document.querySelector(".browser-stage-page")).not.toHaveClass("is-mobile-canvas");
  });
});
