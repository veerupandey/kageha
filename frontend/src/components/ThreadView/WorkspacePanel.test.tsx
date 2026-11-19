import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api } from "../../api/client";
import { useAppStore } from "../../store";
import { WorkspacePanel } from "./WorkspacePanel";

vi.mock("../../api/client", () => ({ api: vi.fn() }));
vi.mock("../../lib/sessionTruth", () => ({ useSessionTruth: () => ({ status: "checking", data: null }) }));
vi.mock("../AgentCanvas", () => ({ AgentCanvas: () => <div>Task activity content</div> }));
vi.mock("./ArtifactPanel", () => ({ ArtifactPanel: () => <div>Session files content</div> }));

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.mocked(api).mockResolvedValue({ sessionId: "workspace-test", bindingId: "page:1", revision: 1,
    connected: true, backend: "chromium", url: "https://example.test", title: "Research", tabs: [],
    lockedBy: "agent", viewport: { width: 1280, height: 800 } });
  useAppStore.setState({ sessionId: "workspace-test", workspaceSurface: "live", liveSurface: "browser",
    machineActive: false, browserPanelMode: "expanded", runStatus: "idle", canvasItems: [], messages: [] });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it("keeps Browser, Files and Activity discoverable and retains the browser across view switches", async () => {
  render(<WorkspacePanel artifactFilter="all" artifactCount={2} onOpenLightbox={vi.fn()} />);
  const header = screen.getByRole("tablist", { name: "Workspace surfaces" });
  expect(header).not.toHaveClass("ka-workspace-tabs--immersive");
  expect(screen.getByRole("tab", { name: "Browser" })).toHaveAttribute("aria-selected", "true");
  await screen.findByRole("button", { name: "Copy URL" });
  const browser = screen.getByRole("region", { name: "Browser" });
  fireEvent.click(screen.getByRole("tab", { name: "Activity" }));
  await screen.findByText("Task activity content");
  expect(useAppStore.getState().workspaceSurface).toBe("monitor");
  expect(browser).toBeInTheDocument();
  fireEvent.click(screen.getByRole("tab", { name: /^Files/ }));
  await screen.findByText("Session files content");
  fireEvent.click(screen.getByRole("tab", { name: "Browser" }));
  await waitFor(() => expect(browser).toBeVisible());
  expect(vi.mocked(api).mock.calls.some(([path]) => /\/(lease|action|connect|promote)$/.test(path))).toBe(false);
});

it("shows Files alongside the same browser and can focus either surface", async () => {
  render(<WorkspacePanel artifactFilter="all" artifactCount={2} onOpenLightbox={vi.fn()} />);
  await screen.findByRole("button", { name: "Copy URL" });
  const browser = screen.getByRole("region", { name: "Browser" });
  const split = screen.getByRole("button", { name: "Show Browser and Files together" });
  fireEvent.click(split);
  expect(split).toHaveAttribute("aria-pressed", "true");
  const files = screen.getByText("Session files content");
  expect(files).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Focus Browser" }));
  expect(files).toBeInTheDocument();
  expect(files).not.toBeVisible();
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.getByText("Session files content")).toBe(files);
  expect(files).toBeVisible();
  fireEvent.click(screen.getByRole("tab", { name: /^Files/ }));
  expect(screen.getByRole("region", { name: "Browser" })).toBe(browser);
  fireEvent.click(screen.getByRole("button", { name: "Focus Files" }));
  expect(browser).not.toBeVisible();
  expect(screen.getByText("Session files content")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Back to split" }));
  expect(screen.getByRole("region", { name: "Browser" })).toBe(browser);
});

it("collapses the workspace from its toolbar", () => {
  const onCollapse = vi.fn();
  render(<WorkspacePanel artifactFilter="all" artifactCount={0} onOpenLightbox={vi.fn()} onCollapse={onCollapse} />);
  const closeBtn = screen.getByRole("button", { name: "Close workspace" });
  fireEvent.click(closeBtn);
  expect(onCollapse).toHaveBeenCalledTimes(1);
});

it("displays running tool in the Activity Capsule and opens activity tab on click", async () => {
  useAppStore.setState({
    runStatus: "running",
    messages: [
      {
        id: "msg-1",
        role: "assistant",
        text: "Researching company",
        toolCards: [
          {
            id: "t-1",
            name: "parallel_web_search",
            argsPreview: 'query="Seequent AI"',
            status: "running",
          },
        ],
      },
    ],
  });
  render(<WorkspacePanel artifactFilter="all" artifactCount={0} onOpenLightbox={vi.fn()} />);
  const activityBtn = screen.getByRole("button", { name: /Activity status:/i });
  expect(activityBtn).toBeVisible();
  expect(activityBtn).toHaveTextContent(/Web research/i);
  fireEvent.click(activityBtn);
  expect(useAppStore.getState().workspaceSurface).toBe("monitor");
});

it("expands the workspace without remounting the browser, restores on Escape, and resets on chat change", async () => {
  render(<WorkspacePanel artifactFilter="all" artifactCount={0} onOpenLightbox={vi.fn()} />);
  await screen.findByRole("button", {name: "Copy URL"});
  const viewer = screen.getByRole("region", {name: "Browser"});
  const workspace = screen.getByRole("region", {name: "Session workspace"});
  fireEvent.click(screen.getByRole("button", {name: "Expand workspace"}));
  expect(workspace).toHaveClass("ka-workspace-surface--expanded");
  expect(screen.getByRole("region", {name: "Browser"})).toBe(viewer);
  fireEvent.keyDown(window, {key: "Escape"});
  expect(workspace).not.toHaveClass("ka-workspace-surface--expanded");
  fireEvent.click(screen.getByRole("button", {name: "Expand workspace"}));
  await act(async () => useAppStore.setState({sessionId: "other-chat"}));
  await waitFor(() => expect(workspace).not.toHaveClass("ka-workspace-surface--expanded"));
});

it("supports arrow-key navigation without losing the active tab focus", async () => {
  render(<WorkspacePanel artifactFilter="all" artifactCount={0} onOpenLightbox={vi.fn()} />);
  const browser = screen.getByRole("tab", {name: "Browser"});
  browser.focus();
  fireEvent.keyDown(browser, {key: "ArrowRight"});
  const files = screen.getByRole("tab", {name: "Files"});
  expect(files).toHaveFocus();
  expect(files).toHaveAttribute("aria-selected", "true");
  fireEvent.keyDown(files, {key: "End"});
  expect(screen.getByRole("tab", {name: "Activity"})).toHaveFocus();
  fireEvent.keyDown(screen.getByRole("tab", {name: "Activity"}), {key: "Home"});
  expect(browser).toHaveFocus();
});

it("renders the review for the owning chat and sends the selected item and revision", async () => {
  const path = "/api/sessions/workspace-test/ui-panel";
  const state = { revision: 4, owner: "agent", view: {
    component: "checklist", props: { title: "Review draft", items: [{ id: "draft", label: "Draft report" }] },
  } };
  vi.mocked(api).mockImplementation(async (url, options) => {
    if (url === path && options?.method === "POST") {
      return { enabled: true, state: { ...state, revision: 5 }, selection: { payload: { id: "draft" } } };
    }
    return url === path ? { enabled: true, state } : { enabled: false };
  });
  useAppStore.setState({ workspaceSurface: "monitor", surfacePinnedAt: Date.now() });
  render(<WorkspacePanel artifactFilter="all" artifactCount={0} onOpenLightbox={vi.fn()} />);
  const review = await screen.findByRole("tab", { name: "Review" });
  expect(screen.getByRole("tab", { name: "Activity" })).toHaveAttribute("aria-selected", "true");
  fireEvent.click(review);
  fireEvent.click(screen.getByRole("button", { name: "Draft report" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Draft report" })).toHaveAttribute("aria-pressed", "true"));
  expect(api).toHaveBeenCalledWith(path, {
    method: "POST", body: JSON.stringify({ action: "select", id: "draft", revision: 4 }),
  });
  act(() => useAppStore.setState({ sessionId: "other-chat" }));
  expect(screen.queryByRole("button", { name: "Draft report" })).toBeNull();
});

it("reveals Files from a native panel request without reopening a collapsed workspace", async () => {
  const path = "/api/sessions/workspace-test/ui-panel";
  vi.mocked(api).mockImplementation(async url => url === path ? {
    enabled: true, state: { revision: 1, owner: "agent", focus: "files", focus_revision: 1, view: null },
  } : {});
  useAppStore.setState(state => ({
    surfacePinnedAt: null, surfaceControlling: false, prefs: { ...state.prefs, rightPanelCollapsed: true },
  }));
  render(<WorkspacePanel artifactFilter="all" artifactCount={0} onOpenLightbox={vi.fn()} />);
  await screen.findByText("Session files content");
  expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute("aria-selected", "true");
  expect(useAppStore.getState().prefs.rightPanelCollapsed).toBe(true);
});


it("keeps the user's tab during a run and restores saved Review after refresh", async () => {
  const path = "/api/sessions/workspace-test/ui-panel";
  let panel = { enabled: true, worker_available: true, state: {
    revision: 1, focus: "files", focus_revision: 1, owner: "agent", view: null as object | null,
  } };
  vi.mocked(api).mockImplementation(async url => url === path ? panel : {});
  useAppStore.setState({ surfacePinnedAt: null, surfaceControlling: false, runStatus: "running" });
  const props = { artifactFilter: "all" as const, artifactCount: 0, onOpenLightbox: vi.fn() };
  const mounted = render(<WorkspacePanel {...props} />);
  await screen.findByText("Session files content");
  fireEvent.click(screen.getByRole("tab", { name: "Activity" }));
  panel = { ...panel, state: { ...panel.state, revision: 2, focus_revision: 2, focus: "checklist",
    view: { component: "checklist", props: { title: "Review draft", items: [{ id: "draft", label: "Draft report" }] } },
  } };
  await screen.findByRole("tab", { name: "Review" }, { timeout: 2500 });
  expect(screen.getByRole("tab", { name: "Activity" })).toHaveAttribute("aria-selected", "true");

  mounted.unmount();
  useAppStore.setState({ workspaceSurface: "live", surfacePinnedAt: null });
  panel = { ...panel, worker_available: false };
  render(<WorkspacePanel {...props} />);
  expect(await screen.findByRole("button", { name: "Draft report" })).toBeDisabled();
  expect(screen.getByText("Review controls are temporarily unavailable.")).toBeInTheDocument();
  panel = { ...panel, worker_available: true };
  await waitFor(() => expect(screen.getByRole("button", { name: "Draft report" })).toBeEnabled(), { timeout: 2500 });
});
