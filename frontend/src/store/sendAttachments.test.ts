import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from ".";
import { acknowledgeSteering, emptyRun } from "./helpers";
import { DEFAULT_PREFS } from "./prefs";

vi.mock("../api/client", () => ({
  api: vi.fn(),
  ApiError: class ApiError extends Error {
    status: number;
    constructor(message: string, status: number) {
      super(message);
      this.status = status;
    }
  },
}));

vi.mock("../api/stream", async () => {
  const actual = await vi.importActual<typeof import("../api/stream")>("../api/stream");
  return {
    ...actual,
    streamChat: vi.fn().mockResolvedValue({}),
    streamSessionEvents: vi.fn().mockImplementation(async (_url, _update, signal) => {
      await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }));
    }),
  };
});

import { api } from "../api/client";
import { streamChat, streamSessionEvents } from "../api/stream";

const original = useAppStore.getState();

function reset() {
  useAppStore.setState({
    ...original,
    sessionId: null,
    threadId: null,
    draft: "",
    pendingFiles: [],
    messages: [],
    runs: {},
    tabs: [],
    sending: false,
    abort: null,
    runStatus: "idle",
    remoteActive: false,
    remoteHostId: "",
    remoteHostName: "",
    prefs: { ...DEFAULT_PREFS },
    capabilities: { ...original.capabilities },
  });
  vi.mocked(api).mockReset();
  vi.mocked(streamChat).mockReset();
  vi.mocked(streamSessionEvents).mockClear();
  vi.mocked(streamChat).mockResolvedValue({});
}

describe("saved chat recovery", () => {
  beforeEach(() => {
    reset();
    const storage = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
      removeItem: (key: string) => storage.delete(key),
    });
    localStorage.setItem("kageha.lastSessionId", "saved-chat");
    useAppStore.setState({
      probeCapabilities: vi.fn().mockResolvedValue(undefined),
      loadSlashCatalog: vi.fn().mockResolvedValue(undefined),
      loadMeta: vi.fn().mockResolvedValue(undefined),
      loadModels: vi.fn().mockResolvedValue(undefined),
      loadSearch: vi.fn().mockResolvedValue(undefined),
      syncPermissions: vi.fn().mockResolvedValue(undefined),
      refreshPersonas: vi.fn().mockResolvedValue(undefined),
      refreshTeams: vi.fn().mockResolvedValue(undefined),
      loadProjects: vi.fn().mockResolvedValue(undefined),
      openSession: vi.fn().mockResolvedValue(undefined),
    });
  });
  afterEach(() => {
    localStorage.removeItem("kageha.lastSessionId");
    vi.unstubAllGlobals();
    reset();
  });

  it("restores the saved conversation even when the sidebar request fails", async () => {
    vi.mocked(api).mockRejectedValue(new Error("Sidebar unavailable"));
    await useAppStore.getState().boot();
    expect(useAppStore.getState().openSession).toHaveBeenCalledWith("saved-chat");
    expect(useAppStore.getState().bootError).toBe("Sidebar unavailable");
  });

  it("restores the conversation while the sidebar request is still pending", async () => {
    let finish!: (value: unknown) => void;
    vi.mocked(api).mockReturnValue(new Promise(resolve => { finish = resolve; }));
    const boot = useAppStore.getState().boot();
    try {
      expect(useAppStore.getState().openSession).toHaveBeenCalledWith("saved-chat");
    } finally {
      finish({ sessions: [] });
      await boot;
    }
  });
});

describe("opening history without waiting for replay", () => {
  beforeEach(() => {
    reset();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}")));
  });
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); reset(); });

  it("shows saved messages while event replay is stalled, then restores activity", async () => {
    let finishReplay!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === "/api/sessions/history") return {
        session_id: "history", messages: [{ role: "assistant", text: "Saved answer" }],
        turns: [{ id: "old-turn", status: "success" }],
      };
      if (String(path).includes("/events?")) {
        return new Promise((resolve) => { finishReplay = resolve; });
      }
      return {};
    });
    await useAppStore.getState().openSession("history");
    expect(useAppStore.getState().sessionLoading).toBe(false);
    expect(useAppStore.getState().messages[0].text).toBe("Saved answer");
    finishReplay({ events: [{ kind: "progress", payload: {
      source_kind: "assistant_narrative", text: "Restored activity", offset: 0,
    } }] });
    await vi.waitFor(() => expect(useAppStore.getState().messages[0].blocks).toContainEqual(
      expect.objectContaining({ type: "narrative", text: expect.stringContaining("Restored activity") }),
    ));
  });

  it("publishes the latest turn while an older journal is still loading", async () => {
    const requests: string[] = [];
    let releaseOld!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === "/api/sessions/long") return {
        session_id: "long",
        messages: [
          { role: "assistant", text: "Old reply", turn_id: "old" },
          { role: "user", text: "Latest request", turn_id: "latest" },
          { role: "user", text: "Follow-up", turn_id: "latest", steering: "applied", message_id: "receipt" },
          { role: "assistant", text: "Latest reply", turn_id: "latest" },
        ],
        turns: [{ id: "old" }, { id: "latest" }],
      };
      if (String(path).includes("/events?")) {
        const params = new URL(String(path), "http://localhost").searchParams;
        expect(params.get("include_live")).toBe("0");
        requests.push(params.get("turn_id")!);
        if (requests.at(-1) === "old") return new Promise(resolve => { releaseOld = resolve; });
        return { events: [{ kind: "progress", payload: { source_kind: "assistant_progress", text: "Latest activity" } }] };
      }
      return {};
    });
    await useAppStore.getState().openSession("long");
    try {
      await vi.waitFor(() => expect(requests).toEqual(["latest", "old"]));
      const rows = useAppStore.getState().messages;
      expect(rows.slice(1).map(row => row.text)).toEqual(["Latest request", "Follow-up", "Latest reply"]);
      expect(rows[2].steering).toBe("applied");
      expect(rows[3].blocks).toContainEqual(expect.objectContaining({type: "progress", text: "Latest activity"}));
    } finally { releaseOld?.({ events: [] }); }
  });

  it("ends loading with a useful error if the session request hangs", async () => {
    vi.useFakeTimers();
    vi.mocked(api).mockImplementation((_path, options) => new Promise((_resolve, reject) => {
      options?.signal?.addEventListener("abort", () => reject(new Error("aborted")));
    }));
    const opening = useAppStore.getState().openSession("unreachable");
    const rejected = expect(opening).rejects.toThrow("aborted");
    await vi.advanceTimersByTimeAsync(20_000);
    await rejected;
    expect(useAppStore.getState().sessionLoading).toBe(false);
    expect(useAppStore.getState().error).toContain("timed out");
  });

  it("ignores a late response after another thread has been selected", async () => {
    let finishFirst!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === "/api/sessions/first") return new Promise((resolve) => { finishFirst = resolve; });
      if (path === "/api/sessions/second") return {
        session_id: "second", messages: [{ role: "assistant", text: "Second answer" }],
      };
      return {};
    });
    const first = useAppStore.getState().openSession("first");
    await useAppStore.getState().openSession("second");
    finishFirst({ session_id: "first", messages: [] });
    await first;
    expect(useAppStore.getState().sessionId).toBe("second");
    expect(useAppStore.getState().messages[0].text).toBe("Second answer");
  });

  it("allows leaving an opening thread for a new chat", async () => {
    let finish!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const opening = useAppStore.getState().openSession("slow");
    await useAppStore.getState().newChat();
    expect(useAppStore.getState().sessionLoading).toBe(false);
    finish({ session_id: "slow", messages: [] });
    await opening;
    expect(useAppStore.getState().sessionId).toBeNull();
  });
});

describe("sendMessage attachment snapshot", () => {
  beforeEach(reset);
  afterEach(reset);

  it("shows local preparation before session creation and cancellation prevents dispatch", async () => {
    let release!: (value: { sessionId: string; threadId: string }) => void;
    const gate = new Promise<{ sessionId: string; threadId: string }>(resolve => { release = resolve; });
    const file = new File(["content"], "resume.txt", { type: "text/plain" });
    useAppStore.setState({ draft: "Hello", pendingFiles: [file], ensureSession: vi.fn(() => gate) });
    const send = useAppStore.getState().sendMessage();
    expect(useAppStore.getState().preparingSends).toMatchObject([{ sessionId: null, text: "Hello" }]);
    expect(useAppStore.getState().messages).toEqual([]);
    useAppStore.getState().cancelPreparingSend();
    expect(useAppStore.getState().attachmentUpload).toBeNull();
    expect(useAppStore.getState().pendingFiles).toEqual([file]);
    release({ sessionId: "late", threadId: "web-late" });
    await send;
    expect(useAppStore.getState().preparingSends).toEqual([]);
    expect(useAppStore.getState().draft).toBe("Hello");
    expect(streamChat).not.toHaveBeenCalled();
  });

  it("clears preparation when session creation fails", async () => {
    useAppStore.setState({ draft: "Hello", ensureSession: vi.fn().mockRejectedValue(new Error("offline")) });
    await useAppStore.getState().sendMessage();
    expect(useAppStore.getState().preparingSends).toEqual([]);
    expect(useAppStore.getState().draft).toBe("Hello");
    expect(useAppStore.getState().error).toBe("offline");
  });

  it("does not restore a cancelled draft after a newer request was sent", async () => {
    let release!: (value: { sessionId: string; threadId: string }) => void;
    const gate = new Promise<{ sessionId: string; threadId: string }>(resolve => { release = resolve; });
    const ensure = vi.fn().mockReturnValueOnce(gate).mockResolvedValue({ sessionId: "one", threadId: "web-one" });
    useAppStore.setState({ sessionId: "one", threadId: "web-one", runs: { one: emptyRun("one", "web-one") },
      draft: "First", ensureSession: ensure, refreshSessions: vi.fn().mockResolvedValue(undefined), refreshArtifacts: vi.fn().mockResolvedValue(undefined) });
    vi.mocked(streamChat).mockResolvedValue({ status: "success", message: "Done" });
    const first = useAppStore.getState().sendMessage();
    useAppStore.getState().cancelPreparingSend();
    useAppStore.setState({ draft: "Second" });
    await useAppStore.getState().sendMessage();
    expect(useAppStore.getState().draft).toBe("");
    release({ sessionId: "one", threadId: "web-one" });
    await first;
    expect(useAppStore.getState().draft).toBe("");
    expect(streamChat).toHaveBeenCalledOnce();
  });

  it("does not let an aborted upload clear a newer upload or its controller", async () => {
    const uploads: { signal?: AbortSignal | null; reject: (error: Error) => void }[] = [];
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (String(path).endsWith("/upload")) return new Promise((_resolve, reject) => {
        uploads.push({ signal: options?.signal, reject });
      });
      return {};
    });
    const firstFile = new File(["first"], "first.txt");
    const secondFile = new File(["second"], "second.txt");
    useAppStore.setState({ sessionId: "one", threadId: "web-one", runs: { one: emptyRun("one", "web-one") },
      draft: "First", pendingFiles: [firstFile] });
    const first = useAppStore.getState().sendMessage();
    await vi.waitFor(() => expect(uploads).toHaveLength(1));
    useAppStore.getState().cancelPreparingSend();
    expect(uploads[0].signal?.aborted).toBe(true);
    useAppStore.setState({ draft: "Second", pendingFiles: [secondFile] });
    const second = useAppStore.getState().sendMessage();
    await vi.waitFor(() => expect(uploads).toHaveLength(2));
    const secondAttempt = useAppStore.getState().attachmentUpload?.attemptId;
    uploads[0].reject(new Error("old upload aborted"));
    await first;
    expect(useAppStore.getState().draft).toBe("Second");
    expect(useAppStore.getState().attachmentUpload).toMatchObject({ attemptId: secondAttempt, status: "uploading", text: "Second" });
    useAppStore.getState().cancelPreparingSend();
    expect(uploads[1].signal?.aborted).toBe(true);
    uploads[1].reject(new Error("new upload aborted"));
    await second;
    expect(useAppStore.getState().pendingFiles).toEqual([secondFile]);
    expect(streamChat).not.toHaveBeenCalled();
  });

  it("does not open a stale session created after cancellation", async () => {
    let release!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path === "/api/sessions" && options?.method === "POST") return new Promise(resolve => { release = resolve; });
      return {};
    });
    useAppStore.setState({ draft: "First" });
    const first = useAppStore.getState().sendMessage();
    useAppStore.getState().cancelPreparingSend();
    useAppStore.setState({ sessionId: "newer", threadId: "web-newer", draft: "Second" });
    release({ session_id: "stale", thread_id: "web-stale" });
    await first;
    expect(useAppStore.getState().sessionId).toBe("newer");
    expect(useAppStore.getState().draft).toBe("Second");
    expect(streamChat).not.toHaveBeenCalled();
  });

  it("does not open a cancelled parallel session or overwrite a newer draft", async () => {
    let release!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path === "/api/sessions" && options?.method === "POST") return new Promise(resolve => { release = resolve; });
      return {};
    });
    useAppStore.setState({ draft: "First", composerChip: { kind: "multitask", value: null } });
    const first = useAppStore.getState().sendMessage();
    useAppStore.getState().cancelPreparingSend();
    useAppStore.setState({ draft: "Second" });
    release({ session_id: "stale", thread_id: "web-stale" });
    await first;
    expect(useAppStore.getState().sessionId).toBeNull();
    expect(useAppStore.getState().draft).toBe("Second");
    expect(streamChat).not.toHaveBeenCalled();
  });

  it("keeps preparation and cancellation scoped to the selected chat", async () => {
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    const ensure = vi.fn(async () => {
      const sessionId = useAppStore.getState().sessionId!;
      await gate;
      return { sessionId, threadId: `web-${sessionId}` };
    });
    useAppStore.setState({ sessionId: "one", threadId: "web-one", draft: "First", ensureSession: ensure });
    const first = useAppStore.getState().sendMessage();
    useAppStore.setState({ sessionId: "two", threadId: "web-two", draft: "Second" });
    const second = useAppStore.getState().sendMessage();
    expect(useAppStore.getState().preparingSends.map(item => item.sessionId)).toEqual(["one", "two"]);
    useAppStore.getState().cancelPreparingSend();
    expect(useAppStore.getState().preparingSends.map(item => item.sessionId)).toEqual(["one"]);
    useAppStore.setState({ sessionId: "one", threadId: "web-one" });
    useAppStore.getState().cancelPreparingSend();
    release();
    await Promise.all([first, second]);
    expect(streamChat).not.toHaveBeenCalled();
  });

  it("ensureSession keeps pendingFiles when minting a session", async () => {
    const file = new File(["# hi"], "brief.md", { type: "text/markdown" });
    useAppStore.setState({ pendingFiles: [file], sessionId: null, threadId: null });
    vi.mocked(api).mockImplementation(async (path, opts) => {
      if (path === "/api/sessions" && opts?.method === "POST") {
        return { session_id: "sess-mint", thread_id: "web-sess-mint" };
      }
      if (String(path).includes("/api/sessions?limit")) {
        return { sessions: [] };
      }
      return {};
    });

    const out = await useAppStore.getState().ensureSession();
    expect(out.sessionId).toBe("sess-mint");
    expect(useAppStore.getState().pendingFiles).toHaveLength(1);
    expect(useAppStore.getState().pendingFiles[0]?.name).toBe("brief.md");
    expect(useAppStore.getState().runs["sess-mint"]?.pendingFiles).toHaveLength(1);
  });

  it("persists a home-screen remote choice before the first turn", async () => {
    const requests: string[] = [];
    useAppStore.setState({
      remoteActive: true,
      remoteHostId: "remote-host-1",
      remoteHostName: "SSH host",
    });
    vi.mocked(api).mockImplementation(async (path, opts) => {
      requests.push(String(path));
      if (path === "/api/sessions" && opts?.method === "POST") {
        return { session_id: "remote-session", thread_id: "web-remote-session" };
      }
      if (path === "/api/remote/activate") {
        expect(JSON.parse(String(opts?.body))).toEqual({
          session_id: "remote-session",
          active: true,
          host_id: "remote-host-1",
        });
        return { ok: true };
      }
      return { sessions: [] };
    });

    await useAppStore.getState().ensureSession();

    expect(requests.indexOf("/api/remote/activate")).toBeGreaterThan(requests.indexOf("/api/sessions"));
    expect(useAppStore.getState().runs["remote-session"]?.remoteActive).toBe(true);
  });

  it("uploads snapped files even if ensureSession would have cleared state", async () => {
    const files = [
      new File(["red"], "chart-red.png", { type: "image/png" }),
      new File(["# x"], "brief.md", { type: "text/markdown" }),
    ];
    useAppStore.setState({
      sessionId: null,
      threadId: null,
      draft: "list attachments",
      pendingFiles: files,
    });

    const uploaded: string[] = [];
    vi.mocked(api).mockImplementation(async (path, opts) => {
      if (path === "/api/sessions" && opts?.method === "POST") {
        return { session_id: "sess-e2e", thread_id: "web-sess-e2e" };
      }
      if (String(path).includes("/upload")) {
        const body = opts?.body as FormData;
        const file = body.get("file") as File;
        uploaded.push(file.name);
        return { path: `inputs/${file.name}` };
      }
      if (String(path).includes("/api/sessions?limit")) {
        return { sessions: [] };
      }
      return {};
    });

    await useAppStore.getState().sendMessage();

    expect(uploaded.sort()).toEqual(["brief.md", "chart-red.png"]);
    expect(streamChat).toHaveBeenCalled();
    const body = vi.mocked(streamChat).mock.calls[0]?.[0] as {
      attachments?: string[];
    };
    expect(body.attachments).toEqual([
      "inputs/chart-red.png",
      "inputs/brief.md",
    ]);
    expect(useAppStore.getState().pendingFiles).toEqual([]);
    const user = useAppStore.getState().messages.find((m) => m.role === "user");
    expect(user?.attachments).toEqual([
      "inputs/chart-red.png",
      "inputs/brief.md",
    ]);
  });
});

describe("chat machine recovery", () => {
  beforeEach(reset);
  afterEach(() => {
    vi.unstubAllGlobals();
    reset();
  });

  it("opens the right panel when the saved chat has an active desktop", async () => {
    useAppStore.setState({ liveSplit: false, liveSurface: "computer" });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({
      active: true, runtime: { browserReady: true, novncReady: true },
    }), { status: 200 })));
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === "/api/sessions/desktop-session") {
        return { session_id: "desktop-session", thread_id: "web-desktop-session", messages: [], turns: [] };
      }
      return { sessions: [] };
    });

    await useAppStore.getState().openSession("desktop-session");

    await vi.waitFor(() => expect(useAppStore.getState().machineActive).toBe(true));
    const state = useAppStore.getState();
    expect(state.machineActive).toBe(true);
    expect(state.liveSplit).toBe(true);
    expect(state.liveSurface).toBe("browser");
  });
});

describe("remote chat health", () => {
  beforeEach(reset);
  afterEach(reset);

  it("restores and checks the remote host when opening a chat", async () => {
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === "/api/sessions/remote-session") {
        return { session_id: "remote-session", thread_id: "web-remote-session", messages: [], turns: [] };
      }
      if (path === "/api/remote?session_id=remote-session") {
        return {
          active: true,
          host_id: "remote-host-1",
          hosts: [{ id: "remote-host-1", name: "SSH host", host: "example.test", risk_tier: "normal" }],
        };
      }
      if (path === "/api/remote/test") return { ok: true, report: {} };
      return { sessions: [] };
    });

    await useAppStore.getState().openSession("remote-session");

    await vi.waitFor(() => expect(useAppStore.getState().remoteHealth).toBe("connected"));
    const state = useAppStore.getState();
    expect(state.remoteActive).toBe(true);
    expect(state.remoteHostName).toBe("SSH host");
    expect(state.remoteHealth).toBe("connected");
  });
});

describe("queue and steering during an active turn", () => {
  beforeEach(() => {
    reset();
    useAppStore.setState({ sessionId: "active", threadId: "web-active", draft: "Use the new instructions", sending: true,
      runs: { active: { ...emptyRun("active", "web-active"), sending: true, statusLabel: "Reading email…", transport: { mode: "sse", turnId: "turn-active" } } },
    } as never);
  });
  afterEach(reset);

  it("queues without interrupting the current turn and allows removal", async () => {
    await useAppStore.getState().sendMessage();
    const state = useAppStore.getState();
    expect(state.runs.active.queue).toEqual([{ text: "Use the new instructions", files: [] }]);
    expect(state.runs.active.statusLabel).toBe("Reading email…");
    expect(streamChat).not.toHaveBeenCalled();
    state.removeQueuedMessage(0);
    expect(useAppStore.getState().runs.active.queue).toEqual([]);
  });

  it("steers the active turn without starting another turn", async () => {
    vi.mocked(api).mockResolvedValue({ ok: true });
    await useAppStore.getState().steerMessage();
    expect(api).toHaveBeenCalledWith("/api/chat/steer", { method: "POST", body: JSON.stringify({ session_id: "active", turn_id: "turn-active", message_id: useAppStore.getState().runs.active.messages.at(-1)?.steeringId, message: "Use the new instructions", steering_boundary: { blockCount: 0, tailTextLength: 0, afterSequence: 0 } }) });
    expect(streamChat).not.toHaveBeenCalled();
    expect(useAppStore.getState().draft).toBe("");
    expect(useAppStore.getState().runs.active.messages.at(-1)).toMatchObject({ role: "user", text: "Use the new instructions", steering: "pending" });
    expect(useAppStore.getState().runs.active.sending).toBe(true);
  });

  it("keeps draft and attachments if the turn finishes before steering is accepted", async () => {
    const file = new File(["details"], "details.txt");
    useAppStore.setState({ pendingFiles: [file] });
    vi.mocked(api).mockImplementation(async (path) => {
      if (String(path).endsWith("/upload")) return { path: "inputs/details.txt" };
      throw new Error("The active turn has changed");
    });
    await useAppStore.getState().steerMessage();
    expect(useAppStore.getState().draft).toBe("Use the new instructions");
    expect(useAppStore.getState().pendingFiles).toEqual([file]);
    expect(useAppStore.getState().runs.active.messages).toEqual([]);
  });

  it("keeps an acknowledgement that arrives before the acceptance response", async () => {
    vi.mocked(api).mockImplementation(async () => {
      const state = useAppStore.getState();
      expect(state.runs.active.messages.at(-1)?.steering).toBe("pending");
      useAppStore.setState({ runs: { ...state.runs, active: acknowledgeSteering(state.runs.active, "Use the new instructions") } });
      return { ok: true };
    });
    await useAppStore.getState().steerMessage();
    expect(useAppStore.getState().runs.active.messages.at(-1)?.steering).toBe("applied");
  });

  it("promotes a queued message to steering while preserving the composer draft", async () => {
    const file = new File(["queued details"], "queued.txt");
    const queued = { text: "Use these details", files: [file] };
    useAppStore.setState((s) => ({ runs: { active: { ...s.runs.active, queue: [queued] } } }));
    vi.mocked(api).mockImplementation(async (path) => String(path).endsWith("/upload") ? { path: "inputs/queued.txt" } : { ok: true });
    await useAppStore.getState().steerMessage(queued.text, queued);
    expect(useAppStore.getState().runs.active.queue).toEqual([]);
    expect(useAppStore.getState().draft).toBe("Use the new instructions");
    expect(useAppStore.getState().runs.active.messages.at(-1)?.attachments).toEqual(["inputs/queued.txt"]);
    const body = JSON.parse(vi.mocked(api).mock.calls.find(([path]) => path === "/api/chat/steer")![1]!.body as string);
    expect(body.message).toContain("inputs/queued.txt");
  });
});

describe("resume preflight", () => {
  beforeEach(reset);
  afterEach(reset);

  it('forwards selected outcome IDs and recovery mode without clearing the draft', async () => {
    useAppStore.setState({ sessionId: 'saved', threadId: 'web-saved', draft: 'Keep this',
      runs: { saved: emptyRun('saved', 'web-saved') } });
    vi.mocked(api).mockResolvedValue({ turns: [{ id: 'latest' }], active_turn: null });
    vi.mocked(streamChat).mockResolvedValueOnce({ status: 'success', message: 'Checked' });
    await useAppStore.getState().resumeLastTask({ mode: 'recheck', outcomeIds: ['pending'], turnId: 'latest' });
    expect(streamChat).toHaveBeenCalledWith(expect.objectContaining({ resume_turn_id: 'latest',
      recovery_mode: 'recheck', recovery_outcome_ids: ['pending'] }), expect.anything(), expect.anything());
    expect(useAppStore.getState().draft).toBe('Keep this');
  });

  it('rejects stale per-outcome buttons before sending a new turn', async () => {
    useAppStore.setState({ sessionId: 'saved', threadId: 'web-saved', runs: { saved: emptyRun('saved', 'web-saved') } });
    vi.mocked(api).mockResolvedValue({ turns: [{ id: 'newer' }], active_turn: null });
    await expect(useAppStore.getState().resumeLastTask({ mode: 'recheck', outcomeIds: ['pending'], turnId: 'old' })).rejects.toThrow('newer task');
    expect(streamChat).not.toHaveBeenCalled();
  });

  it("uses the newest durable turn even when the last visible reply is older", async () => {
    const messages = [{ id: "a", role: "assistant" as const, text: "Earlier reply", turnId: "old" }];
    useAppStore.setState({ sessionId: "saved", threadId: "web-saved", messages, draft: "Keep this draft",
      runs: { saved: { ...emptyRun("saved", "web-saved"), messages } } });
    vi.mocked(api).mockImplementation(async (path) => path === "/api/sessions/saved"
      ? { turns: [{ id: "old" }, { id: "interrupted-without-reply" }], active_turn: null } : {});
    vi.mocked(streamChat).mockResolvedValueOnce({ status: "success", message: "Continuing saved work" });
    await useAppStore.getState().resumeLastTask();
    expect(streamChat).toHaveBeenCalledWith(expect.objectContaining({ resume: true, resume_turn_id: "interrupted-without-reply" }), expect.anything(), expect.anything());
    expect(useAppStore.getState().draft).toBe("Keep this draft");
    expect(useAppStore.getState().resumingSessionId).toBeNull();
  });

  it("reattaches to a running turn without submitting a second request", async () => {
    const openSession = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({ sessionId: "saved", threadId: "web-saved", openSession,
      runs: { saved: emptyRun("saved", "web-saved") } });
    vi.mocked(api).mockResolvedValue({ active_turn: { turn_id: "running" }, turns: [{ id: "running" }] });
    await useAppStore.getState().resumeLastTask();
    expect(openSession).toHaveBeenCalledWith("saved");
    expect(streamChat).not.toHaveBeenCalled();
    expect(useAppStore.getState().runs.saved.messages).toEqual([]);
  });

  it("leaves chat untouched when the preflight fails", async () => {
    const messages = [{ id: "a", role: "assistant" as const, text: "Saved progress", turnId: "old" }];
    useAppStore.setState({ sessionId: "saved", messages, runs: { saved: { ...emptyRun("saved", "web-saved"), messages } } });
    vi.mocked(api).mockRejectedValue(new Error("Connection unavailable"));
    await expect(useAppStore.getState().resumeLastTask()).rejects.toThrow("Connection unavailable");
    expect(useAppStore.getState().runs.saved.messages).toEqual(messages);
    expect(streamChat).not.toHaveBeenCalled();
  });
});

describe("attachments during mobile resume", () => {
  beforeEach(() => {
    reset();
    useAppStore.setState({
      sessionId: "phone", threadId: "web-phone",
      runs: { phone: emptyRun("phone", "web-phone") },
    });
  });
  afterEach(reset);

  it("keeps a file chosen while a resume request is in flight", async () => {
    let resolve!: (value: unknown) => void;
    vi.mocked(api).mockResolvedValue({ active_turn: null });
    vi.mocked(api).mockReturnValueOnce(new Promise(r => { resolve = r; }));
    const recovery = useAppStore.getState().recoverLiveProgress();
    const photo = new File(["photo"], "photo.png", { type: "image/png" });
    useAppStore.getState().setDraft("Look at this photo");
    useAppStore.getState().addPendingFiles([photo]);
    resolve({ active_turn: null, live_message: "" });
    await recovery;
    expect(useAppStore.getState().pendingFiles).toEqual([photo]);
    expect(useAppStore.getState().draft).toBe("Look at this photo");
    await useAppStore.getState().openSession("phone");
    expect(useAppStore.getState().pendingFiles).toEqual([photo]);
  });

  it("does not resurrect removed or cleared files on resume", async () => {
    vi.mocked(api).mockResolvedValue({ active_turn: null });
    const files = [new File(["a"], "a.txt"), new File(["b"], "b.txt")];
    useAppStore.getState().addPendingFiles(files);
    useAppStore.getState().removePendingFile(0);
    await useAppStore.getState().recoverLiveProgress();
    expect(useAppStore.getState().pendingFiles).toEqual([files[1]]);
    useAppStore.getState().clearPendingFiles();
    await useAppStore.getState().recoverLiveProgress();
    expect(useAppStore.getState().pendingFiles).toEqual([]);
  });
});


describe("retryable attachment sends", () => {
  beforeEach(reset);
  afterEach(reset);
  it("keeps the draft and files after upload failure, then retries successfully", async () => {
    const file = new File(["photo"], "photo.png", {type:"image/png"});
    useAppStore.setState({sessionId:"retry-send",threadId:"web-retry-send",draft:"Review this", runs:{"retry-send":emptyRun("retry-send","web-retry-send")}});
    useAppStore.getState().addPendingFiles([file]);
    let fail = true;
    vi.mocked(api).mockImplementation(async path => {
      if (String(path).includes("/upload")) {
        if (fail) throw new Error("Network disconnected");
        return {path:"inputs/photo.png"};
      }
      return {};
    });
    await useAppStore.getState().sendMessage();
    expect(useAppStore.getState().draft).toBe("Review this");
    expect(useAppStore.getState().pendingFiles).toEqual([file]);
    expect(useAppStore.getState().attachmentUpload?.status).toBe("failed");
    expect(streamChat).not.toHaveBeenCalled();
    fail = false;
    await useAppStore.getState().sendMessage();
    expect(useAppStore.getState().attachmentUpload).toBeNull();
    expect(useAppStore.getState().pendingFiles).toEqual([]);
    expect(streamChat).toHaveBeenCalledTimes(1);
  });
});

describe("send now", () => {
  beforeEach(() => {
    reset();
    useAppStore.setState({ sessionId: "active", threadId: "web-active", draft: "Start this instead", sending: true,
      runs: { active: { ...emptyRun("active", "web-active"), sending: true, statusLabel: "Reading email…" } },
    });
  });
  afterEach(reset);

  it("waits for server cancellation before starting the replacement", async () => {
    let accept!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async path => path === "/api/chat/cancel"
      ? await new Promise(resolve => { accept = resolve; }) : {});
    const pending = useAppStore.getState().sendNowMessage();
    expect(streamChat).not.toHaveBeenCalled();
    expect(useAppStore.getState().draft).toBe("Start this instead");
    await vi.waitFor(() => expect(accept).toBeTypeOf("function"));
    accept({ ok: true });
    await pending;
    expect(api).toHaveBeenCalledWith("/api/chat/cancel", expect.objectContaining({ method: "POST", body: JSON.stringify({ session_id: "active", thread_id: "web-active", immediate: true }), signal: expect.anything() }));
    expect(vi.mocked(streamChat).mock.calls[0][0].message).toBe("Start this instead");
  });

  it("keeps the active turn, draft and files if cancellation fails", async () => {
    const file = new File(["brief"], "brief.txt");
    useAppStore.setState({ pendingFiles: [file] });
    vi.mocked(api).mockRejectedValue(new Error("Cancellation unavailable"));
    await useAppStore.getState().sendNowMessage();
    expect(streamChat).not.toHaveBeenCalled();
    expect(useAppStore.getState().runs.active.sending).toBe(true);
    expect(useAppStore.getState().draft).toBe("Start this instead");
    expect(useAppStore.getState().pendingFiles).toEqual([file]);
    expect(useAppStore.getState().error).toBe("Cancellation unavailable");
  });

  it("sends selected queued attachments first and preserves the composer draft", async () => {
    const queued = { text: "Urgent update", files: [new File(["urgent"], "urgent.txt")] };
    const later = { text: "Then this", files: [] };
    useAppStore.setState(s => ({ runs: { active: { ...s.runs.active, queue: [later, queued] } } }));
    vi.mocked(api).mockImplementation(async path => String(path).endsWith("/upload") ? { path: "inputs/urgent.txt" } : {});
    // Keep the replacement active to inspect the remaining queue.
    vi.mocked(streamChat).mockImplementation(() => new Promise(() => {}));
    void useAppStore.getState().sendNowMessage(queued.text, queued);
    await vi.waitFor(() => expect(streamChat).toHaveBeenCalled());
    expect(vi.mocked(streamChat).mock.calls[0][0]).toMatchObject({ message: "Urgent update", attachments: ["inputs/urgent.txt"] });
    expect(useAppStore.getState().runs.active.queue).toEqual([later]);
    expect(useAppStore.getState().draft).toBe("Start this instead");
  });

  it("ignores delayed completion and callbacks from the cancelled stream", async () => {
    useAppStore.setState(s => ({ sending: false, runs: { active: { ...s.runs.active, sending: false } } }));
    let finishOld!: (value: {}) => void;
    let finishNew!: (value: {}) => void;
    vi.mocked(api).mockResolvedValue({});
    vi.mocked(streamChat)
      .mockImplementationOnce(() => new Promise(resolve => { finishOld = resolve; }))
      .mockImplementationOnce(() => new Promise(resolve => { finishNew = resolve; }));
    const old = useAppStore.getState().sendMessage("Old task");
    await vi.waitFor(() => expect(streamChat).toHaveBeenCalledTimes(1));
    useAppStore.setState({ draft: "Replacement task" });
    const replacement = useAppStore.getState().sendNowMessage();
    await vi.waitFor(() => expect(streamChat).toHaveBeenCalledTimes(2));
    vi.mocked(streamChat).mock.calls[0][1].onStatus?.("Stale status", {});
    finishOld({ message: "Old result", status: "success" });
    await old;
    expect(useAppStore.getState().runs.active.sending).toBe(true);
    expect(useAppStore.getState().runs.active.statusLabel).not.toBe("Stale status");
    finishNew({ message: "New result", status: "success" });
    await replacement;
    expect(useAppStore.getState().runs.active.status).toBe("success");
  });
});

describe("saved terminal status", () => {
  beforeEach(reset);
  afterEach(reset);
  it.each([
    ["invalid_output", "interrupted", "Verification unresolved"],
    ["success", "success", "Done"],
    ["blocked", "interrupted", "Blocked"],
    ["awaiting_clarify", "waiting_approval", "Needs your input"],
  ])("restores %s without resetting the overview to idle", async (status, expected, label) => {
    vi.mocked(api).mockImplementation(async (path) => path === "/api/sessions/saved-status"
      ? { session_id: "saved-status", thread_id: "web-saved-status", status, messages: [], turns: [] }
      : {});
    await useAppStore.getState().openSession("saved-status");
    expect(useAppStore.getState().runStatus).toBe(expected);
    expect(useAppStore.getState().statusLabel).toBe(label);
  });
});


describe("live thread opening priority", () => {
  beforeEach(() => {
    reset();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}")));
  });
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); reset(); });

  it("keeps a background run intact when another thread fails to load", async () => {
    const background = { ...emptyRun("background-active", "web-background-active"), status: "running" as const, sending: true };
    useAppStore.setState({ sessionId: "background-active", threadId: "web-background-active",
      runs: { "background-active": background }, runStatus: "running", sending: true });
    vi.mocked(api).mockRejectedValue(new Error("thread read failed"));
    await expect(useAppStore.getState().openSession("unavailable-thread")).rejects.toThrow("thread read failed");
    const state = useAppStore.getState();
    expect(state.runs["background-active"].status).toBe("running");
    expect(state.runStatus).not.toBe("error");
    expect(state.sessionLoading).toBe(false);
    expect(state.sessionOpenFailure).toEqual({ sessionId: "unavailable-thread", message: "thread read failed" });
    await state.newChat();
    expect(useAppStore.getState().sessionOpenFailure).toBeNull();
  });

  it("starts live progress without waiting for a stalled optional remote check", async () => {
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === "/api/sessions/live-first") return {
        session_id: "live-first", thread_id: "web-live-first", messages: [],
        active_turn: { turn_id: "turn-live-first", phase: "planning", status: "running" },
        live_user_message: "Read the page", live_message: "Finding the page",
      };
      if (String(path).startsWith("/api/remote?")) return new Promise(() => {});
      return {};
    });
    await useAppStore.getState().openSession("live-first");
    const state = useAppStore.getState();
    expect(state.sessionLoading).toBe(false);
    expect(state.statusLabel).toBe("Planning…");
    expect(state.messages.at(-1)?.text).toBe("Finding the page");
    expect(streamSessionEvents).toHaveBeenCalledWith(expect.stringContaining("turn-live-first"), expect.any(Function), expect.any(AbortSignal));
    expect(streamChat).not.toHaveBeenCalled();
    await state.newChat();
  });

  it("times out optional probes without disrupting the active chat", async () => {
    vi.useFakeTimers();
    let probeSignal: AbortSignal | null | undefined;
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path === "/api/sessions/probe-timeout") return {
        session_id: "probe-timeout", messages: [],
        active_turn: { turn_id: "probe-turn", phase: "executing" },
      };
      if (String(path).startsWith("/api/remote?")) {
        probeSignal = options?.signal;
        return new Promise((_resolve, reject) => options?.signal?.addEventListener("abort", () => reject(new Error("optional probe timeout"))));
      }
      return {};
    });
    await useAppStore.getState().openSession("probe-timeout");
    expect(useAppStore.getState().sessionLoading).toBe(false);
    await vi.advanceTimersByTimeAsync(4_000);
    expect(probeSignal?.aborted).toBe(true);
    expect(useAppStore.getState().statusLabel).toBe("Working…");
    expect(useAppStore.getState().error).toBeNull();
    expect(streamChat).not.toHaveBeenCalled();
    await useAppStore.getState().newChat();
  });

  it("cancels an obsolete opening request before loading a different thread", async () => {
    let firstSignal: AbortSignal | null | undefined;
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path === "/api/sessions/old-opening") {
        firstSignal = options?.signal;
        return new Promise((_resolve, reject) => options?.signal?.addEventListener("abort", () => reject(new Error("cancelled old load"))));
      }
      if (path === "/api/sessions/new-opening") return { session_id: "new-opening", messages: [] };
      return {};
    });
    const first = useAppStore.getState().openSession("old-opening");
    await useAppStore.getState().openSession("new-opening");
    await first;
    expect(firstSignal?.aborted).toBe(true);
    expect(useAppStore.getState().sessionId).toBe("new-opening");
    expect(useAppStore.getState().sessionLoading).toBe(false);
    expect(useAppStore.getState().error).toBeNull();
  });

  it("ignores stale surface status after the user switches threads", async () => {
    let finishRemote!: (value: unknown) => void;
    let signal: AbortSignal | null | undefined;
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path === "/api/sessions/old-surface") return { session_id: "old-surface", messages: [] };
      if (path === "/api/sessions/new-surface") return { session_id: "new-surface", messages: [] };
      if (path === "/api/remote?session_id=old-surface") {
        signal = options?.signal;
        return new Promise(resolve => { finishRemote = resolve; });
      }
      return {};
    });
    await useAppStore.getState().openSession("old-surface");
    await useAppStore.getState().openSession("new-surface");
    expect(signal?.aborted).toBe(true);
    finishRemote({ active: true, host_id: "old-host", hosts: [] });
    await Promise.resolve();
    expect(useAppStore.getState().sessionId).toBe("new-surface");
    expect(useAppStore.getState().remoteActive).toBe(false);
    expect(useAppStore.getState().machineActive).toBe(false);
  });
});

it("does not revive Stop when a visibility recovery response arrives late", async () => {
  reset();
  useAppStore.setState({sessionId:"late-stop",threadId:"web-late-stop",runs:{"late-stop":{...emptyRun("late-stop","web-late-stop"),sending:true,clientRequestId:"request",transport:{...emptyRun("late-stop").transport!,turnId:"old"}}}});
  let resolve!: (value:unknown)=>void;
  vi.mocked(api).mockImplementation(path=>path==="/api/sessions/late-stop" ? new Promise(done=>{resolve=done;}) : Promise.resolve({}));
  const recovery=useAppStore.getState().recoverLiveProgress();
  await vi.waitFor(()=>expect(resolve).toBeTypeOf("function"));
  await useAppStore.getState().stopGeneration();
  resolve({active_turn:{turn_id:"old"},pending_approval:{approval_id:"old-question",action:"ask_human"},live_message:"late"});
  await recovery;
  expect(useAppStore.getState().runs["late-stop"].status).toBe("cancelled");
  expect(useAppStore.getState().runs["late-stop"].waitingApproval).toBe(false);
  reset();
});

it("does not overwrite a newer request with a delayed failed Stop", async () => {
  reset();
  useAppStore.setState({sessionId:"late-stop",threadId:"web-late-stop",runs:{"late-stop":{...emptyRun("late-stop","web-late-stop"),sending:true,clientRequestId:"old"}}});
  let reject!: (reason:Error)=>void;
  vi.mocked(api).mockImplementation(path=>path==="/api/chat/cancel" ? new Promise((_resolve,fail)=>{reject=fail;}) : Promise.resolve({}));
  const stopping=useAppStore.getState().stopGeneration();
  await vi.waitFor(()=>expect(reject).toBeTypeOf("function"));
  useAppStore.setState(state=>({runs:{...state.runs,"late-stop":{...state.runs["late-stop"],clientRequestId:"new",statusLabel:"New work"}}}));
  reject(new Error("network"));
  await stopping;
  expect(useAppStore.getState().runs["late-stop"].statusLabel).toBe("New work");
  reset();
});

it("does not start Send now after the user stops while cancellation is awaiting confirmation", async () => {
  reset();
  useAppStore.setState({sessionId:"stop-send",threadId:"web-stop-send",draft:"replacement",runs:{"stop-send":{...emptyRun("stop-send","web-stop-send"),sending:true,clientRequestId:"request"}}});
  let resolve!: (value:unknown)=>void;
  vi.mocked(api).mockImplementation(path=>path==="/api/chat/cancel" ? new Promise(done=>{resolve=done;}) : Promise.resolve({}));
  const pending=useAppStore.getState().sendNowMessage();
  await vi.waitFor(()=>expect(resolve).toBeTypeOf("function"));
  useAppStore.setState(state=>({runs:{...state.runs,"stop-send":{...state.runs["stop-send"],status:"cancelled",sending:false}}}));
  resolve({ok:true});
  await pending;
  expect(streamChat).not.toHaveBeenCalled();
  expect(useAppStore.getState().draft).toBe("replacement");
  reset();
});

it("never recovers an unconfirmed request using the unrelated latest turn", async () => {
  reset();
  useAppStore.setState({sessionId:"unconfirmed",threadId:"web-unconfirmed",runs:{unconfirmed:{...emptyRun("unconfirmed","web-unconfirmed"),status:"interrupted",clientRequestId:"request"}}});
  vi.mocked(api).mockImplementation(path=>Promise.resolve(path.includes("/requests/") ? {receipt:{state:"received",turn_id:""}} : {active_turn:{turn_id:"unrelated"}}));
  await useAppStore.getState().recoverLiveProgress();
  expect(vi.mocked(api).mock.calls.every(([path])=>path.includes("/requests/request"))).toBe(true);
  expect(useAppStore.getState().runs.unconfirmed.transport?.turnId).toBeFalsy();
  reset();
});

it("reopens the exact settled delivery in its original bubble without resending", async () => {
  reset();
  useAppStore.setState({sessionId:"settled-delivery",threadId:"web-settled-delivery",runs:{"settled-delivery":{
    ...emptyRun("settled-delivery","web-settled-delivery"),status:"interrupted",clientRequestId:"request",
    messages:[{id:"original-bubble",role:"assistant",text:"",resultStatus:"delivery_unconfirmed",failureCause:"Unconfirmed"}],
  }}});
  vi.mocked(api).mockImplementation(path=>Promise.resolve(path.includes("/requests/")
    ? {receipt:{state:"finished",turn_id:"exact-turn"}}
    : {active_turn:{turn_id:"unrelated"}}));
  await useAppStore.getState().recoverLiveProgress();
  const run = useAppStore.getState().runs["settled-delivery"];
  expect(run.messages).toHaveLength(1);
  expect(run.messages[0]).toMatchObject({id:"original-bubble",turnId:"exact-turn",streaming:true});
  expect(run.messages[0].failureCause).toBeUndefined();
  expect(streamChat).not.toHaveBeenCalled();
  reset();
});

it("drains an in-memory queue without erasing the composer draft or files", async () => {
  reset();
  const file=new File(["draft"],"draft.txt");
  useAppStore.setState({sessionId:"queued",threadId:"web-queued",runs:{queued:emptyRun("queued","web-queued")}});
  vi.mocked(api).mockResolvedValue({});
  let resolve!: (value:Record<string,unknown>)=>void;
  vi.mocked(streamChat).mockImplementationOnce(()=>new Promise(done=>{resolve=done;})).mockResolvedValue({status:"success",message:"done"});
  const sending=useAppStore.getState().sendMessage("first");
  await vi.waitFor(()=>expect(resolve).toBeTypeOf("function"));
  useAppStore.setState(state=>({draft:"still typing",pendingFiles:[file],runs:{...state.runs,queued:{...state.runs.queued,queue:[{text:"queued reply",files:[]}]}}}));
  useAppStore.getState().addPendingFiles([file]);
  resolve({status:"success",message:"first done"});
  await sending;
  expect(vi.mocked(streamChat).mock.calls.map(([body])=>body.message)).toEqual(["first","queued reply"]);
  expect(useAppStore.getState().draft).toBe("still typing");
  expect(useAppStore.getState().pendingFiles).toEqual([file]);
  reset();
});

it("reopening an interrupted pre-dispatch request keeps its saved reply for review", async () => {
  reset();
  const data=new Map<string,string>();
  vi.stubGlobal("localStorage",{get length(){return data.size;},key:(i:number)=>[...data.keys()][i] ?? null,getItem:(key:string)=>data.get(key) ?? null,setItem:(key:string,value:string)=>{data.set(key,value);},removeItem:(key:string)=>{data.delete(key);}});
  const {rememberDelivery,pendingDeliveries}=await import("./delivery");
  rememberDelivery({id:"no-dispatch",sessionId:"recovery",text:"saved reply",createdAt:Date.now(),state:"pending"});
  vi.mocked(api).mockImplementation(path=>Promise.resolve(path.includes("/requests/") ? {receipt:{state:"finished",turn_id:"",result_status:"interrupted"}}
    : path==="/api/sessions/recovery" ? {session_id:"recovery",thread_id:"web-recovery",messages:[],turns:[],active_turn:null} : {}));
  await useAppStore.getState().openSession("recovery");
  await vi.waitFor(()=>expect(useAppStore.getState().draft).toBe("saved reply"));
  expect(pendingDeliveries("recovery")[0].text).toBe("saved reply");
  expect(streamChat).not.toHaveBeenCalled();
  reset();
});

it("excludes legacy private knowledge selections when resuming a chat", async () => {
  reset();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => key === "kageha.knowledgeBases"
      ? JSON.stringify(["agent_memory", "agent-private", "bad/id", "research", "research"])
      : null,
    setItem: vi.fn(),
    removeItem: vi.fn(),
  });
  try {
    useAppStore.setState({ sessionId: "saved", threadId: "web-saved",
      runs: { saved: emptyRun("saved", "web-saved") } });
    vi.mocked(api).mockResolvedValue({ turns: [{ id: "latest" }], active_turn: null });
    vi.mocked(streamChat).mockResolvedValueOnce({ status: "success", message: "Checked" });
    await useAppStore.getState().resumeLastTask();
    expect(streamChat).toHaveBeenCalledWith(
      expect.objectContaining({ knowledge_bases: ["research"] }),
      expect.anything(), expect.anything(),
    );
  } finally {
    vi.unstubAllGlobals();
    reset();
  }
});
