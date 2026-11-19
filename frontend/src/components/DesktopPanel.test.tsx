import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, act } from "@testing-library/react";
import { api } from "../api/client";
import { useAppStore } from "../store";
import { DesktopPanel } from "./DesktopPanel";

const rfb = vi.hoisted(() => ({
  urls: [] as string[],
  last: null as {
    viewOnly: boolean;
    qualityLevel: number;
    compressionLevel: number;
    _fbWidth: number;
    _fbHeight: number;
  } | null,
}));

vi.mock("../api/client", async () => ({
  ...await vi.importActual<typeof import("../api/client")>("../api/client"),
  api: vi.fn(),
}));

vi.mock("@novnc/novnc", () => ({
  default: class RFB {
    viewOnly = true;
    scaleViewport = false;
    clipViewport = false;
    resizeSession = false;
    qualityLevel = 2;
    compressionLevel = 6;
    _fbWidth = 1920;
    _fbHeight = 1080;
    constructor(_target: HTMLElement, url: string) {
      rfb.urls.push(url);
      rfb.last = this;
    }
    disconnect() {}
    addEventListener(type: string, listener: (event: Event) => void) {
      if (type === "connect") {
        queueMicrotask(() => listener(new Event("connect")));
      }
    }
    removeEventListener() {}
  },
}));

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

describe("DesktopPanel", () => {
  it("shows desktop attachment errors above the connecting overlay", async () => {
    vi.mocked(api).mockRejectedValue(new Error("existing machine belongs to a different session or workspace"));
    render(<DesktopPanel />);
    await waitFor(() => expect(screen.getByText("Computer unavailable")).toBeInTheDocument());
    expect(screen.getAllByText("existing machine belongs to a different session or workspace").length).toBeGreaterThan(0);
    expect(screen.queryByText("Connect screen")).not.toBeInTheDocument();
  });
  beforeEach(() => {
    rfb.urls = [];
    rfb.last = null;
    vi.mocked(api).mockImplementation(async (path: string) => {
      if (String(path).includes("/desktop") && !String(path).includes("/lease")) {
        return {
          ok: true,
          wsPath: "/api/sessions/session-1/desktop/ws",
          password: "s3cret",
          adopted: false,
          novncReady: true,
        };
      }
      return { ok: true, owner: "user" };
    });
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    useAppStore.setState({
      sessionId: "session-1",
      runStatus: "idle",
      machineActive: true,
      machineBlocked: false,
      machineNovncUrl: "ready",
      machineAdopted: false,
      machineBootPhase: "",
      machineBrowserReady: false,
      machineDetail: "",
      desktopLeaseOwner: "",
      workspaceSurface: "live",
      messages: [],
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    useAppStore.setState({
      sessionId: null,
      machineActive: false,
      machineBlocked: false,
      machineNovncUrl: "",
      desktopLeaseOwner: "",
      messages: [],
    });
  });

  it("shows a desktop HUD without an address bar", async () => {
    render(<DesktopPanel />);

    expect(screen.getByRole("tab", { name: "You" })).toHaveAttribute("aria-selected", "false");
    expect(screen.getByRole("button", { name: "Take control" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Address")).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText("Open a page…")).not.toBeInTheDocument();
    await waitFor(() => expect(rfb.urls.length).toBe(1));
    expect(rfb.urls[0]).toContain("/api/sessions/session-1/desktop/ws");
    expect(rfb.urls[0]).not.toContain("127.0.0.1");
    expect(rfb.last?.qualityLevel).toBe(6);
    expect(screen.getByTestId("desktop-rfb")).toHaveAttribute("data-fb-width", "1920");
  });

  it("shows a blocked machine instead of connecting to noVNC", async () => {
    useAppStore.setState({
      runStatus: "running",
      machineBlocked: true,
      machineBootPhase: "screen",
      machineDetail: "Docker daemon timed out",
    });
    render(<DesktopPanel />);

    expect(screen.getByText("Computer unavailable")).toBeInTheDocument();
    expect(screen.getByText("Docker daemon timed out")).toBeInTheDocument();
    expect(screen.queryByText("Connect screen")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Take control" })).not.toBeInTheDocument();
    expect(screen.queryByTestId("desktop-rfb")).not.toBeInTheDocument();
    expect(api).not.toHaveBeenCalled();

    act(() => useAppStore.setState({ machineBlocked: false, machineDetail: "" }));
    await waitFor(() => expect(rfb.urls).toHaveLength(1));
    expect(screen.queryByText("Computer unavailable")).not.toBeInTheDocument();
  });

  it("toggles viewOnly on Take control without reconnecting", async () => {
    render(<DesktopPanel />);
    await waitFor(() => expect(rfb.last).toBeTruthy());
    expect(rfb.last?.viewOnly).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Take control" }));

    await waitFor(() => {
      expect(api).toHaveBeenCalledWith(
        "/api/sessions/session-1/machine/lease",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ owner: "user" }),
        }),
      );
    });
    expect(screen.getByRole("tab", { name: "You" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: "Release" })).toBeInTheDocument();
    expect(rfb.urls.length).toBe(1);
    expect(rfb.last?.viewOnly).toBe(false);
    expect(rfb.last?.qualityLevel).toBe(8);
  });

  it("keeps the RFB canvas across chat updates", async () => {
    const { rerender } = render(<DesktopPanel />);
    await waitFor(() => expect(rfb.urls.length).toBe(1));
    act(() => {
      useAppStore.setState({
        messages: [
          {
            id: "a1",
            role: "assistant",
            text: "clicking",
            computerFrames: [{ url: "/shot.png", app: "Machine Desktop", action: "click" }],
          },
        ],
      });
    });
    rerender(<DesktopPanel />);
    expect(await screen.findByTestId("desktop-action")).toHaveTextContent("Clicking");
    expect(rfb.urls.length).toBe(1);
  });

  it("attaches the live screen even before a boot event sets noVNC", async () => {
    useAppStore.setState({ machineNovncUrl: "" });
    render(<DesktopPanel />);
    expect(screen.getByText(/Starting this computer|Connecting the screen/)).toBeInTheDocument();
    await waitFor(() => expect(rfb.urls.length).toBe(1));
    expect(await screen.findByTestId("desktop-rfb")).toBeInTheDocument();
  });

  it("shows reconnect copy for an adopted box before the screen is up", async () => {
    vi.mocked(api).mockRejectedValue(new Error("task machine is not active"));
    useAppStore.setState({
      machineNovncUrl: "",
      machineAdopted: true,
      machineBootPhase: "screen",
    });
    render(<DesktopPanel />);
    expect(screen.getByText(/Reconnecting to this computer/)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText(/Reconnecting to this computer/)).toBeInTheDocument());
  });

  it("drops RFB quality when Live is hidden", async () => {
    render(<DesktopPanel />);
    await waitFor(() => expect(rfb.last?.qualityLevel).toBe(6));
    act(() => {
      useAppStore.setState({ workspaceSurface: "monitor" });
    });
    await waitFor(() => expect(rfb.last?.qualityLevel).toBe(2));
    expect(rfb.urls.length).toBe(1);
  });

  it("shows a browser-starting chip once the screen is painted", async () => {
    render(<DesktopPanel />);
    expect(await screen.findByTestId("desktop-browser-boot")).toHaveTextContent(
      "Screen is live · browser starting",
    );
    act(() => {
      useAppStore.setState({ machineBrowserReady: true, machineBootPhase: "browser" });
    });
    await waitFor(() => expect(screen.queryByTestId("desktop-browser-boot")).toBeNull());
  });

  it("offers a full-screen toggle that requests and exits browser fullscreen", async () => {
    const requestFullscreen = vi.fn().mockResolvedValue(undefined);
    const exitFullscreen = vi.fn(() => {
      currentEl = null;
      document.dispatchEvent(new Event("fullscreenchange"));
      return Promise.resolve();
    });
    let currentEl: Element | null = null;
    // JSDOM has no Fullscreen API — stub what the panel reads/calls.
    Object.defineProperty(document, "fullscreenEnabled", {
      configurable: true,
      value: true,
    });
    Object.defineProperty(document, "fullscreenElement", {
      configurable: true,
      get: () => currentEl,
    });
    document.exitFullscreen = exitFullscreen as unknown as typeof document.exitFullscreen;
    // requestFullscreen lands on the panel element; capture and flip state.
    Element.prototype.requestFullscreen = function requestFs(
      this: Element,
    ): Promise<void> {
      currentEl = this as Element;
      requestFullscreen();
      document.dispatchEvent(new Event("fullscreenchange"));
      return Promise.resolve();
    };

    render(<DesktopPanel />);

    const enter = screen.getByRole("button", { name: "Full screen desktop" });
    fireEvent.click(enter);
    await waitFor(() => expect(requestFullscreen).toHaveBeenCalledTimes(1));

    const exit = await screen.findByRole("button", { name: "Exit full screen" });
    fireEvent.click(exit);
    await waitFor(() => expect(exitFullscreen).toHaveBeenCalledTimes(1));
    // Back to the enter-affordance once fullscreen is released.
    await screen.findByRole("button", { name: "Full screen desktop" });
  });
});
