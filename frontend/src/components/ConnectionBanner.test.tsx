import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAppStore } from "../store";
import { ConnectionBanner } from "./ConnectionBanner";

const original = useAppStore.getState();
const refreshSessions = vi.fn(async () => {});
beforeEach(() => {
  vi.useFakeTimers();
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(true);
  useAppStore.setState({ connectionOnline: true, bootError: null, sessionsError: "Refresh timed out", refreshSessions });
});
afterEach(() => {
  cleanup();
  useAppStore.setState(original);
  vi.restoreAllMocks();
  vi.clearAllMocks();
  vi.useRealTimers();
});
const advance = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

it("backs off failed refreshes and clears the warning on recovery", async () => {
  render(<ConnectionBanner />);
  await advance(1999);
  expect(refreshSessions).not.toHaveBeenCalled();
  await advance(1);
  expect(refreshSessions).toHaveBeenCalledTimes(1);
  await advance(3999);
  expect(refreshSessions).toHaveBeenCalledTimes(1);
  refreshSessions.mockImplementationOnce(async () => { useAppStore.setState({ sessionsError: null }); });
  await advance(1);
  expect(refreshSessions).toHaveBeenCalledTimes(2);
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  await advance(60_000);
  expect(refreshSessions).toHaveBeenCalledTimes(2);
});

it("pauses in hidden tabs, resumes on return, and never overlaps recovery reads", async () => {
  vi.spyOn(document, "hidden", "get").mockReturnValue(true);
  render(<ConnectionBanner />);
  await advance(2000);
  expect(refreshSessions).not.toHaveBeenCalled();
  let finish!: () => void;
  refreshSessions.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  fireEvent(document, new Event("visibilitychange"));
  fireEvent(window, new Event("online"));
  await advance(60_000);
  expect(refreshSessions).toHaveBeenCalledTimes(1);
  await act(async () => { finish(); });
});

it("caps repeated failures at one recovery attempt every thirty seconds", async () => {
  render(<ConnectionBanner />);
  for (const delay of [2000, 4000, 8000, 16_000, 30_000, 30_000]) await advance(delay);
  expect(refreshSessions).toHaveBeenCalledTimes(6);
  await advance(29_999);
  expect(refreshSessions).toHaveBeenCalledTimes(6);
  await advance(1);
  expect(refreshSessions).toHaveBeenCalledTimes(7);
});

it("stops automatic retries when unmounted and leaves manual retry available", async () => {
  const view = render(<ConnectionBanner />);
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(refreshSessions).toHaveBeenCalledTimes(1);
  view.unmount();
  await advance(60_000);
  expect(refreshSessions).toHaveBeenCalledTimes(1);
});

it("does not retry while offline or when boot needs recovery", async () => {
  useAppStore.setState({ connectionOnline: false });
  render(<ConnectionBanner />);
  await advance(60_000);
  expect(refreshSessions).not.toHaveBeenCalled();
  act(() => { useAppStore.setState({ connectionOnline: true, bootError: "Boot failed" }); });
  await advance(60_000);
  expect(refreshSessions).not.toHaveBeenCalled();
});
