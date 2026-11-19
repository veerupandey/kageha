import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { WorkerTrace } from "./WorkerTrace";
import { streamSessionEvents } from "../api/stream";
vi.mock("../api/stream", () => ({ streamSessionEvents: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it("observes attributed child reasoning live, ignores other jobs and aborts only the observer", async () => {
  let receive: (packet: Record<string, unknown>) => void = () => {};
  let signal: AbortSignal | undefined;
  vi.mocked(streamSessionEvents).mockImplementation(async (_url, onUpdate, observed) => {
    receive = onUpdate; signal = observed;
    await new Promise<void>(resolve => observed.addEventListener("abort", () => resolve()));
  });
  render(<WorkerTrace jobId="job" sessionId="child" turnId="turn" />);
  expect(streamSessionEvents).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Show worker activity" }));
  const frame = (sequence: number, text: string, job = "job", state = "delta") => ({ sequence, turn_id: "turn", kind: "stream_item",
    payload: { job_id: job, generation: 0, channel: "reasoning", item_id: "thought", state, text, offset: 0 } });
  await act(async () => { receive({ events: [frame(1, "Reading evidence"), frame(2, "Wrong child", "other")], live: { active_turn: { turn_id: "turn" } } }); });
  expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("Reading evidence");
  expect(screen.queryByText("Wrong child")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
  expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("Reading evidence");
  await act(async () => { receive({ events: [frame(1, "Reading evidence"), frame(3, "Reading evidence", "job", "completed")], live: {} }); });
  expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Thought" }));
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("Reading evidence");
  fireEvent.click(screen.getByRole("button", { name: "Hide worker activity" }));
  expect(signal?.aborted).toBe(true);
  expect(streamSessionEvents).toHaveBeenCalledTimes(1);
});

it("reconnects from the last applied sequence without repeating the child text", async () => {
  vi.useFakeTimers();
  try {
    const frame = (sequence: number, text: string, offset: number) => ({ sequence, turn_id: "turn", kind: "stream_item",
      payload: { job_id: "job", generation: 0, channel: "reasoning", item_id: "thought", state: "delta", text, offset } });
    vi.mocked(streamSessionEvents)
      .mockImplementationOnce(async (_url, receive) => {
        receive({ events: [frame(1, "First", 0)], live: { active_turn: { turn_id: "turn" } } });
        throw new Error("Transport dropped");
      })
      .mockImplementationOnce(async (_url, receive, signal) => {
        receive({ events: [frame(1, "First", 0), frame(2, " more", 5)], live: { active_turn: { turn_id: "turn" } } });
        await new Promise<void>(resolve => signal.addEventListener("abort", () => resolve()));
      });
    render(<WorkerTrace jobId="job" sessionId="child" turnId="turn" />);
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Show worker activity" })); });
    expect(screen.getByRole("status")).toHaveTextContent("Reconnecting");
    await act(async () => { vi.advanceTimersByTime(2000); });
    expect(vi.mocked(streamSessionEvents).mock.calls[1][0]).toContain("after_sequence=1");
    fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
    expect(screen.getByTestId("reasoning-body")).toHaveTextContent("First more");
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  } finally {
    cleanup(); vi.useRealTimers();
  }
});
