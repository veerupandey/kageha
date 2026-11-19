import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { AppShell } from "./AppShell";

const fixture = vi.hoisted(() => ({
  state: {
    sessionId: "s1", sessionLoading: false, sessionRefreshing: null as string | null,
    sessionRefreshError: null as { sessionId: string; message: string } | null,
    sessionOpenFailure: null as { sessionId: string; message: string } | null, openSession: vi.fn().mockResolvedValue(undefined), canvasItems: [], canvasExpanded: false,
    canvasSelectedPath: "artifacts/report.md" as string | null,
    prefs: { sidebarCollapsed: true, rightPanelCollapsed: true },
    peerAlertNavRequestId: 0, setPrefs: vi.fn(), setCanvasExpanded: vi.fn(),
    newChat: vi.fn(), showToast: vi.fn(), loadModels: vi.fn().mockResolvedValue(undefined),
  },
}));
vi.mock("../store", () => ({ useAppStore: (select: (state: typeof fixture.state) => unknown) => select(fixture.state) }));
vi.mock("../api/client", () => ({ api: vi.fn(async () => ({ jobs: [] })) }));
vi.mock("../lib/usePeerAlerts", () => ({ usePeerAlerts: vi.fn() }));
vi.mock("./GeminiLiveVoiceContext", () => ({ GeminiLiveVoiceProvider: ({ children }: { children: ReactNode }) => children }));
vi.mock("./Sidebar/Sidebar", () => ({ Sidebar: () => null }));
vi.mock("./CommandCenter/CommandCenter", () => ({ CommandCenter: () => null }));
vi.mock("./ThreadView/ThreadView", () => ({ ThreadView: () => <div>Chat</div> }));

describe("restoring artifact previews", () => {
  beforeEach(() => {
    fixture.state.sessionOpenFailure = null;
    fixture.state.sessionRefreshing = null;
    fixture.state.sessionRefreshError = null;
    fixture.state.openSession.mockClear();
    fixture.state.setPrefs.mockClear();
    fixture.state.sessionId = "s1";
    fixture.state.sessionLoading = false;
    fixture.state.canvasSelectedPath = "artifacts/report.md";
  });
  afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

  it("keeps chat visible on phone when a restored artifact is selected", async () => {
    vi.stubGlobal("matchMedia", () => ({ matches: true }));
    const view = render(<AppShell />);
    await waitFor(() => expect(view.getByText("Chat")).toBeTruthy());
    expect(fixture.state.setPrefs).toHaveBeenCalledWith({ rightPanelCollapsed: true });
    expect(fixture.state.setPrefs).not.toHaveBeenCalledWith({ rightPanelCollapsed: false });
    fixture.state.canvasSelectedPath = "artifacts/new-report.md";
    view.rerender(<AppShell />);
    expect(fixture.state.setPrefs).not.toHaveBeenCalledWith({ rightPanelCollapsed: false });
  });

  it("continues showing restored previews on desktop", async () => {
    vi.stubGlobal("matchMedia", () => ({ matches: false }));
    const view = render(<AppShell />);
    await waitFor(() => expect(view.getByText("Chat")).toBeTruthy());
    expect(fixture.state.setPrefs).toHaveBeenCalledWith({ rightPanelCollapsed: false });
  });

  it("offers a scoped retry after loading fails without claiming the task failed", async () => {
    fixture.state.sessionOpenFailure = { sessionId: "requested-thread", message: "Request timed out" };
    const view = render(<AppShell />);
    expect(view.getByRole("alert").textContent).toContain("background task");
    await act(async () => { fireEvent.click(view.getByRole("button", { name: "Retry opening thread" })); });
    expect(fixture.state.openSession).toHaveBeenCalledWith("requested-thread");
  });

  it("keeps chat visible while refreshing and offers an inline refresh retry", async () => {
    fixture.state.sessionRefreshing = "s1";
    const view = render(<AppShell />);
    await waitFor(() => expect(view.getByText("Chat")).toBeTruthy());
    expect(view.getByRole("status").textContent).toBe("Refreshing thread…");
    fixture.state.sessionRefreshing = null;
    fixture.state.sessionRefreshError = { sessionId: "s1", message: "Disconnected" };
    view.rerender(<AppShell />);
    expect(view.getByText("Chat")).toBeTruthy();
    await act(async () => fireEvent.click(view.getByRole("button", { name: "Retry refresh" })));
    expect(fixture.state.openSession).toHaveBeenCalledWith("s1");
  });

  it("shows feedback while a thread opens from home or another conversation", async () => {
    fixture.state.sessionLoading = true;
    const view = render(<AppShell />);
    await waitFor(() => expect(view.getByRole("status").textContent).toBe("Opening thread…"));
    expect(view.queryByText("Chat")).toBeNull();
    fixture.state.sessionId = "";
    view.rerender(<AppShell />);
    expect(view.getByRole("status").textContent).toBe("Opening thread…");
  });
});


describe("model catalog refresh", () => {
  afterEach(() => { cleanup(); vi.useRealTimers(); });
  it("refreshes on focus and every minute, and stops when unmounted", () => {
    vi.useFakeTimers();
    fixture.state.loadModels.mockClear();
    const view = render(<AppShell />);
    fireEvent.focus(window);
    expect(fixture.state.loadModels).toHaveBeenCalledTimes(1);
    act(() => vi.advanceTimersByTime(60_000));
    expect(fixture.state.loadModels).toHaveBeenCalledTimes(2);
    view.unmount();
    act(() => vi.advanceTimersByTime(60_000));
    fireEvent.focus(window);
    expect(fixture.state.loadModels).toHaveBeenCalledTimes(2);
  });
});
