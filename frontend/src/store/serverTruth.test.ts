import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
vi.mock("../api/client", async () => ({ ...await vi.importActual<typeof import("../api/client")>("../api/client"), api: vi.fn() }));
import { api } from "../api/client";
import type { SessionRun } from "../api/types";
import { emptyRun } from "./helpers";
import { useAppStore } from "./index";

const original = useAppStore.getState();

/** A run left "running" by a stream that dropped after the turn t1 was admitted. */
function staleRun(): SessionRun {
  return {
    ...emptyRun("s1", "web-s1"),
    sending: true,
    status: "running",
    statusLabel: "Reconnecting…",
    clientRequestId: "request-1",
    startedAt: Date.now() - 60_000,
    transport: { mode: "polling", lastSequence: 4, lastFrameAt: 0, reconnectCount: 1, turnId: "t1" },
    messages: [
      { id: "u1", role: "user", text: "Write the report", turnId: "t1" },
      { id: "a1", role: "assistant", text: "", streaming: true, statusLabel: "Reconnecting…", turnId: "t1" },
    ],
  };
}

const detail = {
  session_id: "s1", thread_id: "web-s1", active_turn: null, status: "success",
  turns: [{ id: "t1", turn_id: "t1", status: "success" }],
  messages: [{ role: "user", text: "Write the report" }, { role: "assistant", text: "The report is ready.", turn_id: "t1" }],
};

beforeEach(() => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value), removeItem: (key: string) => data.delete(key) });
  useAppStore.setState({
    sessionId: "s1", threadId: "web-s1", runs: { s1: staleRun() }, serverTurns: {},
    sending: true, runStatus: "running", statusLabel: "Reconnecting…",
    sessions: [{ session_id: "s1", title: "Report", status: "running", active_turn: { turn_id: "t1", status: "running" } }],
    refreshArtifacts: vi.fn().mockResolvedValue(undefined),
  });
  vi.mocked(api).mockImplementation(async (path: string) => {
    if (path.startsWith("/api/sessions?")) {
      return { sessions: [{ session_id: "s1", title: "Report", status: "success", active: false, active_turn: null }], next_offset: null };
    }
    if (path === "/api/sessions/s1") return detail;
    return {};
  });
});
afterEach(() => { useAppStore.setState(original); vi.unstubAllGlobals(); vi.clearAllMocks(); });

describe("server turn status is the source of truth (Requirement 18.4)", () => {
  it("settles a stale local Running flag from the session status, with the turn's saved answer", async () => {
    await useAppStore.getState().recoverLiveProgress("s1");
    const state = useAppStore.getState();
    expect(state.serverTurns.s1).toMatchObject({ running: false });
    // Composer (Stop button), header and Activity read these.
    expect(state.sending).toBe(false);
    expect(state.runStatus).toBe("success");
    expect(state.runs.s1.messages.at(-1)).toMatchObject({ text: "The report is ready.", streaming: false, statusLabel: "Done" });
  });

  it("refreshes the sidebar and the current chat on a status refresh (tab visible or focused)", async () => {
    await useAppStore.getState().refreshSessionStatus();
    await vi.waitFor(() => expect(useAppStore.getState().sending).toBe(false));
    const paths = vi.mocked(api).mock.calls.map(([path]) => path);
    expect(paths.some(path => path.startsWith("/api/sessions?"))).toBe(true);
    expect(paths).toContain("/api/sessions/s1");
    expect(useAppStore.getState().sessions[0].active_turn).toBeNull();
    expect(useAppStore.getState().serverTurns.s1.running).toBe(false);
  });

  it("keeps a turn the server still runs, and does not settle a send the server has not admitted", async () => {
    vi.mocked(api).mockResolvedValue({ ...detail, active_turn: { turn_id: "t1", status: "waiting_for_unlock" } });
    useAppStore.getState().noteServerTurn("s1", { turnId: "t1", status: "waiting_for_unlock", running: true, observedAt: Date.now() });
    expect(useAppStore.getState().sending).toBe(true);
    const pending = { ...staleRun(), startedAt: Date.now() + 1_000, transport: undefined };
    useAppStore.setState({ runs: { s1: pending } });
    useAppStore.getState().noteServerTurn("s1", { turnId: "", status: "success", running: false, observedAt: Date.now() + 2_000 });
    await Promise.resolve();
    expect(useAppStore.getState().runs.s1.sending).toBe(true);
  });
});
