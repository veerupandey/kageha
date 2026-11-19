import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api/client";
import { useAppStore } from ".";
import { emptyRun, mapHistoryMessages } from "./helpers";
import { restoreMissingAssistantRows } from "./replayTranscript";

vi.mock("../api/client", () => ({ api: vi.fn() }));
const original = useAppStore.getState();

describe("durable turn deletion", () => {
  afterEach(() => {
    useAppStore.setState(original, true);
    vi.clearAllMocks();
  });

  function prepare() {
    const sid = "delete-session";
    const messages = restoreMissingAssistantRows(mapHistoryMessages([
      { role: "user", text: "Prepare the dataset", turn_id: "first", history_index: 0 },
      { role: "user", text: "go go", turn_id: "second", history_index: 1 },
    ]), [
      { id: "first", objective: "Prepare the dataset", status: "interrupted" },
      { id: "second", objective: "go go", status: "interrupted" },
    ], "");
    const run = { ...emptyRun(sid, "thread"), messages };
    const toast = vi.fn();
    useAppStore.setState({ sessionId: sid, messages, runs: { [sid]: run },
      showToast: toast, refreshSessions: vi.fn().mockResolvedValue(undefined) });
    return { sid, messages, toast };
  }

  it("uses persisted identity after replay inserts an assistant row", async () => {
    const { sid } = prepare();
    vi.mocked(api).mockResolvedValue({ ok: true, removed: 1 });
    await useAppStore.getState().deleteTurn(2);
    expect(JSON.parse(vi.mocked(api).mock.calls[0][1]!.body as string)).toEqual({
      message_index: 2, history_index: 1, turn_id: "second", role: "user", message_text: "go go",
    });
    expect(useAppStore.getState().runs[sid].messages).toHaveLength(2);
  });

  it("retains the message when the server reports an unsuccessful delete", async () => {
    const { sid, messages, toast } = prepare();
    vi.mocked(api).mockResolvedValue({ ok: false, error: "not saved" });
    await useAppStore.getState().deleteTurn(2);
    expect(useAppStore.getState().runs[sid].messages).toEqual(messages);
    expect(toast).toHaveBeenCalledWith("Delete failed: not saved");
  });

  it("does not truncate another chat after switching while the request is pending", async () => {
    const { sid } = prepare();
    let resolve!: (result: unknown) => void;
    vi.mocked(api).mockImplementation(() => new Promise((done) => { resolve = done; }));
    const deletion = useAppStore.getState().deleteTurn(2);
    const other = { ...emptyRun("other", "other-thread"), messages: [
      { id: "other-message", role: "user" as const, text: "Keep this" },
    ] };
    useAppStore.setState({ sessionId: "other", messages: other.messages,
      runs: { ...useAppStore.getState().runs, other } });
    resolve({ ok: true });
    await deletion;
    expect(useAppStore.getState().messages).toEqual(other.messages);
    expect(useAppStore.getState().runs[sid].messages).toHaveLength(2);
  });
});
