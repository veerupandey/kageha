import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "../api/client";
import type { BrowserState } from "../api/types";
import { useAppStore } from "../store";
import { BrowserPanel } from "./BrowserPanel";
import { BrowserActionNotice } from "./BrowserActionNotice";

vi.mock("./BrowserLiveSurface", async () => ({
  BrowserLiveSurface: (await import("./test_helpers/MockBrowserLiveSurface")).MockBrowserLiveSurface,
}));

vi.mock("../api/client", () => ({ api: vi.fn() }));

const browserState: BrowserState = {
  sessionId: "audit-ui", bindingId: "binding:1", revision: 1, connected: true,
  backend: "chromium", url: "https://example.test/form", title: "Form",
  tabs: [{ index: 0, active: true, title: "Form", url: "https://example.test/form" }],
  lockedBy: "user", canControl: true, frame: { url: "/api/fixture.png", bindingId: "binding:1" },
  viewport: { width: 1280, height: 800 },
};

beforeEach(() => {
  vi.mocked(api).mockResolvedValue(browserState);
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  useAppStore.setState({ sessionId: "audit-ui", runStatus: "idle", machineActive: false,
    browserPanelMode: "expanded", workspaceSurface: "live", liveSurface: "browser", messages: [] });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  useAppStore.setState({ sessionId: null, messages: [] });
});

async function painted() {
  render(<BrowserPanel />);
  const image = await waitFor(() => {
    const img = document.querySelector("img.object-contain") as HTMLImageElement;
    expect(img).toBeTruthy();
    return img;
  });
  fireEvent.load(image);
  vi.mocked(api).mockClear();
  return image;
}

function inputActions() {
  return vi.mocked(api).mock.calls.filter(([, options]) => options?.method === "POST")
    .map(([, options]) => JSON.parse(options!.body as string).action);
}

it("does not offer new-tab recovery when the viewer cannot identify its current tab", () => {
  const open = vi.fn();
  render(<BrowserActionNotice actions={[
    { id: "held", attemptId: "attempt", action: "click", state: "uncertain", blocksCurrentTab: true },
  ]} hold={{ blocksCurrentTab: true, unknownTarget: true, total: 1, truncated: false }} onNewTab={open} />);
  expect(screen.queryByText(/which tab is safe/)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Review action details" }));
  expect(screen.getByText(/which tab is safe/)).toBeVisible();
  expect(screen.queryByRole("button", { name: "Open a new tab" })).not.toBeInTheDocument();
  expect(screen.queryByText("Output")).not.toBeInTheDocument();
  expect(open).not.toHaveBeenCalled();
});

it("toolbar keyboard activation must not press Enter on the remote website", async () => {
  await painted();
  const button = screen.getByRole("button", { name: "Copy URL" });
  await act(async () => {
    button.focus();
    fireEvent.keyDown(button, { key: "Enter", cancelable: true });
  });
  expect(inputActions()).not.toContain("press");
});

it("editing another contenteditable must not type into the remote website", async () => {
  await painted();
  const editor = document.createElement("div");
  editor.setAttribute("contenteditable", "true");
  editor.tabIndex = 0;
  document.body.appendChild(editor);
  try {
    await act(async () => {
      editor.focus();
      fireEvent.keyDown(editor, { key: "x", cancelable: true });
    });
    expect(inputActions()).not.toContain("type");
  } finally { editor.remove(); }
});

it("releases the actual right button when a drag leaves the preview", async () => {
  const image = await painted();
  fireEvent.mouseDown(image, { clientX: 20, clientY: 20, button: 2 });
  await waitFor(() => expect(inputActions()).toContain("mousedown"));
  fireEvent.mouseUp(document.body, { button: 2 });
  await waitFor(() => expect(inputActions()).toContain("mouseup"));
  const up = vi.mocked(api).mock.calls.find(([, options]) => options?.body && JSON.parse(String(options.body)).action === "mouseup");
  expect(JSON.parse(String(up![1]!.body))).toMatchObject({ button: "right", bindingId: "binding:1" });
});

it("keeps a failed local action visible through healthy state polling", async () => {
  await painted();
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
    writeText: vi.fn().mockRejectedValue(new Error("Denied")),
  } });
  fireEvent.click(screen.getByRole("button", { name: "Copy URL" }));
  const message = "Could not copy the URL. Copy it from the address bar.";
  await screen.findByText(message);
  const reads = () => vi.mocked(api).mock.calls.filter(([path, options]) =>
    String(path).endsWith("/browser") && !options?.method).length;
  const before = reads();
  await waitFor(() => expect(reads()).toBeGreaterThan(before), { timeout: 4000 });
  expect(screen.getByText(message)).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss action error" }));
  expect(screen.queryByText(message)).not.toBeInTheDocument();
});

it.each([false, true])("orders input and drops queued typing after unmount=%s", async (unmount) => {
  const image = await painted();
  const original = vi.mocked(api).getMockImplementation()!;
  let release!: (value: unknown) => void;
  const pending = new Promise(resolve => { release = resolve; });
  vi.mocked(api).mockImplementation((path, options) =>
    options?.body && JSON.parse(String(options.body)).action === "mousedown" ? pending : original(path, options));
  fireEvent.mouseDown(image, { button: 0, clientX: 10, clientY: 10 });
  await waitFor(() => expect(inputActions()).toContain("mousedown"));
  await act(async () => { fireEvent.keyDown(image, { key: "x" }); });
  expect(inputActions()).not.toContain("type");
  if (unmount) cleanup();
  await act(async () => { release({ ok: true }); });
  if (unmount) expect(inputActions()).not.toContain("type");
  else await waitFor(() => expect(inputActions()).toContain("type"));
});

it("releasing outside the preview must release the remote mouse button", async () => {
  const image = await painted();
  fireEvent.mouseDown(image, { clientX: 20, clientY: 20, button: 0 });
  await waitFor(() => expect(inputActions()).toContain("mousedown"));
  fireEvent.mouseUp(document.body, { clientX: 1000, clientY: 1000, button: 0 });
  fireEvent.blur(window);
  await waitFor(() => expect(inputActions()).toContain("mouseup"));
});


it("shows browser readiness without duplicating the agent's live status", async () => {
  const state = browserState;
  vi.mocked(api).mockResolvedValue({ ...state, lockedBy: "agent" });
  render(<BrowserPanel />);
  expect(await screen.findByText("Browser ready")).toBeVisible();
  expect(screen.queryByText("Agent is working")).not.toBeInTheDocument();
  expect(document.querySelector(".browser-status-dot")).not.toHaveClass("is-working");
  expect(screen.getByRole("tab", { name: "Form" })).toHaveAttribute("aria-selected", "true");
  expect(screen.getByRole("tab", { name: "Form" })).toBeDisabled();
  await act(async () => { useAppStore.setState({ runStatus: "running" }); });
  expect(screen.getByText("Browser ready")).toBeVisible();
  expect(screen.queryByText("Agent is working")).not.toBeInTheDocument();
  expect(document.querySelector(".browser-status-dot")).not.toHaveClass("is-working");
  await act(async () => { useAppStore.setState({ runStatus: "idle" }); });
  expect(screen.getByText("Browser ready")).toBeVisible();
});

it("takes control without resizing the external browser and uses a single handoff action", async () => {
  const state = { ...browserState, backend: "comet", owned: false };
  let owner: "agent" | "user" = "agent";
  vi.mocked(api).mockImplementation(async (path, options) => {
    if (String(path).endsWith('/lease') && JSON.parse(String(options?.body)).action === 'take') owner = "user";
    return { ...state, lockedBy: owner };
  });
  render(<BrowserPanel />);
  fireEvent.click(await screen.findByRole("button", { name: "Take control" }));
  await screen.findByText("You have control");
  expect(inputActions()).toContain("take");
  expect(inputActions()).not.toContain("resize");
  expect(screen.getAllByRole("button", { name: "Return control to agent" })).toHaveLength(1);
  expect(screen.queryByRole("tablist", { name: "Who controls this browser" })).not.toBeInTheDocument();
});

it.each([false, true])("styles review notices according to the authoritative current-tab hold=%s", (blocking) => {
  render(<BrowserActionNotice actions={[
    { id: "held", attemptId: "attempt", action: "click", state: "uncertain", blocksCurrentTab: !blocking },
  ]} hold={{ blocksCurrentTab: blocking, unknownTarget: false, total: 1, truncated: false }} />);
  const notice = screen.getByLabelText("Browser action needs review");
  expect(notice).toHaveAttribute("data-blocking", String(blocking));
  expect(notice).toHaveClass(blocking ? "text-warn" : "text-muted");
  expect(screen.queryByText("Output")).not.toBeInTheDocument();
});


it("prevents duplicate ownership transfers while the browser lease is pending", async () => {
  const state = browserState;
  let finish!: (value: unknown) => void;
  let owner: "agent" | "user" = "agent";
  vi.mocked(api).mockImplementation(async (path) => String(path).endsWith('/lease')
    ? new Promise(resolve => { finish = resolve; }) : { ...state, lockedBy: owner });
  render(<BrowserPanel />);
  const button = await screen.findByRole("button", { name: "Take control" });
  fireEvent.click(button);
  fireEvent.click(button);
  expect(screen.getByRole("button", { name: "Transferring…" })).toBeDisabled();
  expect(inputActions().filter(action => action === 'take')).toHaveLength(1);
  await act(async () => { owner = "user"; finish({ ...state, lockedBy: owner }); });
  expect(screen.getByRole("button", { name: "Return control to agent" })).toBeEnabled();
});


it("keeps the browser controls in the header outside the remote page in full-screen mode", async () => {
  useAppStore.setState({ browserPanelMode: "fullscreen" });
  render(<BrowserPanel />);
  const header = await screen.findByLabelText("Browser control");
  expect(header.tagName).toBe("HEADER");
  expect(header.closest(".stage-window-well")).toBeNull();
  expect(header.compareDocumentPosition(document.querySelector(".stage-window-well")!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(document.querySelector(".stage-window-well")).not.toHaveClass("is-overlay");
  expect(screen.getByRole("button", { name: "Exit full screen" })).toBeVisible();
});


it("keeps the profile picker available when the task tab is unavailable", async () => {
  vi.mocked(api).mockResolvedValue({ ...browserState, connected: false, connectionStatus: "target_unavailable", backend: "comet", selectedBackend: "comet" });
  render(<BrowserPanel />);
  expect(await screen.findByText("Task tab unavailable")).toBeVisible();
  expect(screen.getByRole("combobox", { name: "Browser profile" })).toBeEnabled();
  expect(screen.getByRole("combobox", { name: "Browser profile" })).toHaveValue("comet");
  expect(screen.getByRole("button", { name: "Retry browser connection" })).toBeEnabled();
  expect(inputActions()).toEqual([]);
});

it("keeps failed control transfers actionable without claiming human ownership", async () => {
  vi.mocked(api).mockImplementation(async (path) => {
    if (String(path).endsWith('/lease')) throw new Error('Browser control could not be transferred');
    return { ...browserState, lockedBy: "agent" };
  });
  render(<BrowserPanel />);
  fireEvent.click(await screen.findByRole("button", { name: "Take control" }));
  await screen.findByText('Browser control could not be transferred');
  expect(await screen.findByRole("button", { name: "Take control" })).toBeEnabled();
  expect(screen.getByText("Browser ready")).toBeVisible();
  expect(screen.queryByText("You have control")).not.toBeInTheDocument();
});


it("does not label a blocked current tab as ready or actively operating", async () => {
  vi.mocked(api).mockResolvedValue({ ...browserState, lockedBy: "agent", pendingBrowserActions: [],
    browserActionHold: { blocksCurrentTab: true, unknownTarget: true, total: 1, truncated: true } });
  useAppStore.setState({ runStatus: "running" });
  render(<BrowserPanel />);
  expect(await screen.findByText("Action needs review")).toBeVisible();
  expect(screen.queryByText("Browser ready")).not.toBeInTheDocument();
  expect(screen.queryByText("Agent is working")).not.toBeInTheDocument();
  expect(document.querySelector(".browser-status-dot")).not.toHaveClass("is-working");
});


it("pauses page input and tab navigation while returning browser control", async () => {
  const image = await painted();
  let finish!: (value: unknown) => void;
  vi.mocked(api).mockImplementation(async (path) => String(path).endsWith('/lease')
    ? new Promise(resolve => { finish = resolve; }) : browserState);
  fireEvent.click(screen.getByRole("button", { name: "Return control to agent" }));
  expect(screen.getByRole("button", { name: "Back" })).toBeDisabled();
  expect(screen.getByRole("tab", { name: "Form" })).toBeDisabled();
  expect(screen.getByRole("textbox", { name: "Address" })).toBeDisabled();
  fireEvent.mouseDown(image, { button: 0, clientX: 20, clientY: 20 });
  await act(async () => {});
  expect(inputActions()).not.toContain("mousedown");
  await act(async () => finish({ ...browserState, lockedBy: "agent" }));
});


it("keeps the painted surface during a transient disconnect and blocks input", async () => {
  let state = { ...browserState };
  vi.mocked(api).mockImplementation(async () => state);
  const surface = await painted();
  state = { ...state, connected: false };
  await screen.findByText("Browser disconnected", {}, { timeout: 4000 });
  expect(document.querySelector("img.object-contain")).toBe(surface);
  expect(surface).not.toHaveClass("opacity-0");
  expect(document.querySelector(".stage-boot")).toBeNull();
  fireEvent.mouseDown(surface, { button: 0, clientX: 20, clientY: 20 });
  await act(async () => {});
  expect(inputActions()).not.toContain("mousedown");
});

it("reopens resized video geometry and blocks input until the new frame is painted", async () => {
  let state = { ...browserState, layout: { mode: "panel" as const }, viewport: { width: 800, height: 700 } };
  vi.mocked(api).mockImplementation(async () => state);
  const first = await painted();
  expect(first.src).toContain("viewport=800x700");
  state = { ...state, viewport: { width: 900, height: 650 } };
  const next = await waitFor(() => {
    const image = document.querySelector("img.object-contain") as HTMLImageElement;
    expect(image.src).toContain("viewport=900x650");
    return image;
  }, { timeout: 4000 });
  expect(next).toBe(first);
  fireEvent.mouseDown(next, { button: 0, clientX: 20, clientY: 20 });
  await act(async () => {});
  expect(inputActions()).not.toContain("mousedown");
  fireEvent.load(next);
  fireEvent.mouseDown(next, { button: 0, clientX: 20, clientY: 20 });
  await waitFor(() => expect(inputActions()).toContain("mousedown"));
});
