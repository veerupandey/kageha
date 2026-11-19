import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useSelectedAgent } from "./useSelectedAgent";
import { agentKey } from "./agentContinuity";
import { useDurableJobs, type DurableJob } from "../components/DurableJobs";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
it("restores and monitors the selected specialist from one native session stream after the parent ends", async () => {
  const session = "native-ui-parent";
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
  });
  localStorage.setItem(
    agentKey(session, "selected-agent"),
    JSON.stringify("native-worker"),
  );
  const job: DurableJob = {
    id: "native-worker",
    label: "Reviewer",
    kind: "agent",
    state: "running",
    revision: 2,
    session_id: "native-child",
    parent_session: session,
    result: {},
    error: {},
    artifacts: [],
    created_at: 1,
    started_at: 1,
    attachable: true,
    can_cancel: true,
  };
  let source!: ReadableStreamDefaultController<Uint8Array>;
  const cancelled = vi.fn();
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      source = controller;
    },
    cancel: cancelled,
  });
  const fetch = vi.fn(
    async () =>
      new Response(stream, {
        headers: { "Content-Type": "text/event-stream" },
      }),
  );
  vi.stubGlobal("fetch", fetch);
  const hook = renderHook(() => ({
    selection: useSelectedAgent(session),
    monitor: useDurableJobs(session),
  }));
  const send = (event: string, data: unknown) =>
    source.enqueue(
      new TextEncoder().encode(
        `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
      ),
    );
  await act(async () => {
    send("snapshot", { sequence: 10, session_id: session, workers: [job] });
  });
  await waitFor(() =>
    expect(hook.result.current.selection[0]?.id).toBe("native-worker"),
  );
  expect(hook.result.current.selection[0]).toBe(
    hook.result.current.monitor.jobs[0],
  );
  await act(async () => {
    send("update", {
      events: [
        {
          sequence: 11,
          kind: "done",
          turn_id: "parent-turn",
          payload: { status: "success" },
        },
      ],
    });
  });
  expect(cancelled).not.toHaveBeenCalled();
  await act(async () => {
    send("update", {
      events: [
        {
          sequence: 12,
          kind: "worker",
          job: { ...job, state: "succeeded", revision: 3 },
        },
      ],
    });
  });
  await waitFor(() =>
    expect(hook.result.current.selection[0]?.state).toBe("succeeded"),
  );
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch).toHaveBeenCalledWith(
    expect.stringContaining(`/api/sessions/${session}/live/stream`),
    expect.anything(),
  );
  hook.unmount();
  await waitFor(() => expect(cancelled).toHaveBeenCalledTimes(1));
});
