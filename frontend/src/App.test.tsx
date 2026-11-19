import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const fixture = vi.hoisted(() => ({ state: {
  boot: vi.fn().mockResolvedValue(undefined), addPendingFiles: vi.fn(), setConnectionOnline: vi.fn(),
  refreshSessionStatus: vi.fn().mockResolvedValue(undefined), refreshSessions: vi.fn().mockResolvedValue(undefined),
  recoverLiveProgress: vi.fn().mockResolvedValue(undefined), sessionId: "thread", sending: false,
  runStatus: "idle", sessionLoading: false, sessionRefreshing: null as string | null,
} }));
vi.mock("./store", () => ({ useAppStore: Object.assign((select: (state: typeof fixture.state) => unknown) => select(fixture.state), { getState: () => fixture.state }) }));
vi.mock("./components/AppShell", () => ({ AppShell: () => null }));
vi.mock("./components/ConnectionBanner", () => ({ ConnectionBanner: () => null }));
vi.mock("./components/DropOverlay", () => ({ DropOverlay: () => null }));
vi.mock("./components/Toasts", () => ({ Toasts: () => null }));
vi.mock("./lib/attachmentRecovery", () => ({ startAttachmentRecovery: vi.fn().mockResolvedValue(() => {}) }));
vi.mock("./lib/urlSync", () => ({ applyUrlSurface: vi.fn(), parseUrlState: vi.fn().mockReturnValue({}) }));
import App from "./App";
beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(true);
  fixture.state.sending = false;
  fixture.state.runStatus = "idle";
  fixture.state.sessionRefreshing = null;
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.useRealTimers(); });

it("refreshes sidebar and idle conversation every ten seconds without a reload", async () => {
  render(<App />);
  await act(async () => vi.advanceTimersByTimeAsync(10_000));
  expect(fixture.state.refreshSessions).toHaveBeenCalledExactlyOnceWith({ background: true });
  expect(fixture.state.recoverLiveProgress).toHaveBeenCalledExactlyOnceWith(undefined, { lightweight: true });
});

it("pauses periodic work while hidden or offline and refreshes on reconnect", async () => {
  render(<App />);
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
  await act(async () => vi.advanceTimersByTimeAsync(10_000));
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
  await act(async () => vi.advanceTimersByTimeAsync(10_000));
  expect(fixture.state.refreshSessions).not.toHaveBeenCalled();
  expect(fixture.state.recoverLiveProgress).not.toHaveBeenCalled();
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(true);
  act(() => window.dispatchEvent(new Event("online")));
  expect(fixture.state.refreshSessionStatus).toHaveBeenCalledTimes(1);
});

it("checks running tasks cheaply while avoiding duplicate reads during selection", async () => {
  fixture.state.sending = true;
  fixture.state.runStatus = "running";
  render(<App />);
  await act(async () => vi.advanceTimersByTimeAsync(2000));
  expect(fixture.state.recoverLiveProgress).toHaveBeenCalledExactlyOnceWith(undefined, { lightweight: true });
  fixture.state.sessionRefreshing = "thread";
  await act(async () => vi.advanceTimersByTimeAsync(8000));
  expect(fixture.state.recoverLiveProgress).toHaveBeenCalledTimes(1);
  expect(fixture.state.refreshSessions).toHaveBeenCalledTimes(1);
});
