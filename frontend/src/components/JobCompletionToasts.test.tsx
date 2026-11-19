import { act, cleanup, render } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { JobCompletionToasts } from "./JobCompletionToasts";

const fixture = vi.hoisted(() => ({
  state: { sessionId: "global-native", showToast: vi.fn() },
  api: vi.fn(async () => ({ jobs: [] })),
}));
vi.mock("../store", () => ({
  useAppStore: (select: (state: typeof fixture.state) => unknown) =>
    select(fixture.state),
}));
vi.mock("../api/client", async (original) => ({
  ...(await original<typeof import("../api/client")>()),
  api: fixture.api,
}));
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it("keeps initial global discovery but streams completion without periodic jobs requests", async () => {
  vi.useFakeTimers();
  let source!: ReadableStreamDefaultController<Uint8Array>;
  const cancel = vi.fn();
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response(
          new ReadableStream<Uint8Array>({
            start(c) {
              source = c;
            },
            cancel,
          }),
          { headers: { "Content-Type": "text/event-stream" } },
        ),
    ),
  );
  const view = render(<JobCompletionToasts />);
  const send = (event: string, data: unknown) =>
    source.enqueue(
      new TextEncoder().encode(
        `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
      ),
    );
  await act(async () => {
    send("snapshot", {
      sequence: 10,
      session_id: fixture.state.sessionId,
      workers: [{ id: "w", label: "Reviewer", state: "running" }],
    });
    for (let i = 0; i < 3; i++) {
      source.enqueue(new TextEncoder().encode(": heartbeat\n\n"));
      await vi.advanceTimersByTimeAsync(8_000);
    }
  });
  expect(fixture.api).toHaveBeenCalledTimes(1);
  await act(async () => {
    send("update", {
      events: [
        { sequence: 11, kind: "done", turn_id: "parent" },
        {
          sequence: 12,
          kind: "worker",
          job: { id: "w", label: "Reviewer", state: "succeeded" },
        },
      ],
    });
  });
  expect(fixture.state.showToast).toHaveBeenCalledExactlyOnceWith(
    "Job success: Reviewer",
  );
  view.unmount();
  expect(cancel).toHaveBeenCalledOnce();
});

it("retains periodic global discovery for legacy servers", async () => {
  vi.useFakeTimers();
  fixture.state.sessionId = "global-legacy";
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response("Not supported", { status: 404 })),
  );
  const view = render(<JobCompletionToasts />);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(16_000);
  });
  expect(fixture.api).toHaveBeenCalledTimes(3);
  view.unmount();
});
