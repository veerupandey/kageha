import { act, renderHook, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { usePeerAlerts } from "./usePeerAlerts";
import { api } from "../api/client";
import { useAppStore } from "../store";

vi.mock("../api/client", () => ({ api: vi.fn() }));

function setResponses(overrides: {
  requests?: Array<{ request_id: string; requester?: { name?: string } }>;
  grants?: Array<{ grant_id: string; capability_id: string; state: string; provider_id: string }>;
  localAgentId?: string;
}) {
  vi.mocked(api).mockImplementation(async (path: string) => {
    if (path === "/api/peers/pairing") return { requests: overrides.requests || [] };
    if (path === "/api/peers/grants") return { grants: overrides.grants || [] };
    if (path === "/api/peers") return { local_identity: { agent_id: overrides.localAgentId || "left" } };
    return {};
  });
}

describe("usePeerAlerts", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("skips peer transports when the backend declares peers unavailable", async () => {
    const originalMeta = useAppStore.getState().meta;
    useAppStore.setState({ meta: { features: { peers: false } } });
    try {
      renderHook(() => usePeerAlerts());
      await act(async () => { await Promise.resolve(); });
      expect(api).not.toHaveBeenCalled();
    } finally {
      cleanup();
      useAppStore.setState({ meta: originalMeta });
    }
  });

  it("does not toast on the first poll (avoids replaying stale requests on load)", async () => {
    const showToast = vi.fn();
    const setPeerPendingCount = vi.fn();
    useAppStore.setState({ showToast, setPeerPendingCount });
    setResponses({
      requests: [{ request_id: "r1", requester: { name: "Right" } }],
      grants: [],
    });

    await act(async () => {
      renderHook(() => usePeerAlerts());
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(showToast).not.toHaveBeenCalled();
    expect(setPeerPendingCount).toHaveBeenCalledWith(1);
  });

  it("toasts once for a newly-seen pairing request on a later poll", async () => {
    vi.useFakeTimers();
    const showToast = vi.fn();
    const setPeerPendingCount = vi.fn();
    useAppStore.setState({ showToast, setPeerPendingCount });
    setResponses({ requests: [], grants: [] });

    renderHook(() => usePeerAlerts());
    await act(async () => {
      await Promise.resolve();
    });
    expect(showToast).not.toHaveBeenCalled();

    setResponses({
      requests: [{ request_id: "r1", requester: { name: "Right" } }],
      grants: [],
    });
    await act(async () => {
      vi.advanceTimersByTime(6000);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(showToast).toHaveBeenCalledWith(
      "Right wants to pair",
      { actionLabel: "Review", actionId: "open_agent_network" },
    );

    // The same request seen again must not toast a second time.
    showToast.mockClear();
    await act(async () => {
      vi.advanceTimersByTime(6000);
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(showToast).not.toHaveBeenCalled();
  });

  it("only counts grant requests awaiting this agent's own approval", async () => {
    const setPeerPendingCount = vi.fn();
    useAppStore.setState({ showToast: vi.fn(), setPeerPendingCount });
    setResponses({
      requests: [],
      grants: [
        { grant_id: "g1", capability_id: "knowledge.query", state: "requested", provider_id: "left" },
        { grant_id: "g2", capability_id: "knowledge.query", state: "requested", provider_id: "someone-else" },
        { grant_id: "g3", capability_id: "knowledge.query", state: "active", provider_id: "left" },
      ],
      localAgentId: "left",
    });

    await act(async () => {
      renderHook(() => usePeerAlerts());
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(setPeerPendingCount).toHaveBeenCalledWith(1);
  });

  it("re-polls immediately when the SSE stream pushes a pairing event", async () => {
    class FakeEventSource {
      static instances: FakeEventSource[] = [];
      listeners: Record<string, Array<() => void>> = {};
      constructor(_url: string) {
        FakeEventSource.instances.push(this);
      }
      addEventListener(kind: string, handler: () => void) {
        (this.listeners[kind] ||= []).push(handler);
      }
      close() {}
      emit(kind: string) {
        for (const handler of this.listeners[kind] || []) handler();
      }
    }
    vi.stubGlobal("EventSource", FakeEventSource);

    const showToast = vi.fn();
    useAppStore.setState({ showToast, setPeerPendingCount: vi.fn() });
    setResponses({ requests: [], grants: [] });

    renderHook(() => usePeerAlerts());
    await act(async () => {
      await Promise.resolve();
    });
    expect(vi.mocked(api)).toHaveBeenCalledTimes(3); // initial poll: pairing+grants+identity

    setResponses({
      requests: [{ request_id: "r1", requester: { name: "Right" } }],
      grants: [],
    });
    const source = FakeEventSource.instances[0];
    await act(async () => {
      source.emit("peer.pairing_requested");
      await Promise.resolve();
    });

    expect(vi.mocked(api)).toHaveBeenCalledTimes(6); // re-polled on push, without waiting for the timer
    expect(showToast).toHaveBeenCalledWith(
      "Right wants to pair",
      { actionLabel: "Review", actionId: "open_agent_network" },
    );
    vi.unstubAllGlobals();
  });
});
