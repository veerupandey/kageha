import { afterEach, describe, expect, it, vi } from "vitest";
import type { PendingApproval } from "../api/types";
import { useAppStore } from ".";
import { emptyRun, beginApprovalResolution, finishApprovalResolution, isApprovalSuppressed } from "./helpers";
import { api } from "../api/client";

vi.mock("../api/client", () => ({
  api: vi.fn(),
}));

const original = useAppStore.getState();

describe("approval resolution", () => {
  afterEach(() => {
    vi.mocked(api).mockReset();
    useAppStore.setState({
      sessionId: original.sessionId,
      threadId: original.threadId,
      runs: original.runs,
      pendingApproval: original.pendingApproval,
      sendMessage: original.sendMessage,
    });
  });

  it("clears an answered question from both active and per-session state", async () => {
    const pending: PendingApproval = {
      approval_id: "question:turn-1",
      sessionId: "session-1",
      action: "ask_human",
      risk_class: "hitl",
      detail: "Which concept should I use?",
    };
    const run = {
      ...emptyRun("session-1", "thread-1"),
      waitingApproval: true,
      pendingApproval: pending,
    };
    const sendMessage = vi.fn().mockResolvedValue(undefined);

    useAppStore.setState({
      sessionId: "session-1",
      threadId: "thread-1",
      runs: { "session-1": run },
      pendingApproval: pending,
      sendMessage,
    });

    await useAppStore.getState().resolveApproval(true, "Concept A");

    expect(sendMessage).toHaveBeenCalledWith("Concept A");
    expect(useAppStore.getState().pendingApproval).toBeNull();
    expect(useAppStore.getState().runs["session-1"]?.pendingApproval).toBeNull();
    expect(useAppStore.getState().runs["session-1"]?.waitingApproval).toBe(false);
  });

  it("dismisses an expired or unknown approval without re-adding it to state", async () => {
    vi.mocked(api).mockRejectedValue(
      new Error("unknown or expired approval: test-aid-1"),
    );

    const pending: PendingApproval = {
      approval_id: "test-aid-1",
      sessionId: "session-1",
      action: "bash_elevated",
      risk_class: "shell_elevated",
      detail: "rm -rf something",
    };
    const run = {
      ...emptyRun("session-1", "thread-1"),
      waitingApproval: true,
      pendingApproval: pending,
    };

    useAppStore.setState({
      sessionId: "session-1",
      threadId: "thread-1",
      runs: { "session-1": run },
      pendingApproval: pending,
    });

    await useAppStore.getState().resolveApproval(false);

    // Must be cleared so the user is not locked with a broken card
    expect(useAppStore.getState().pendingApproval).toBeNull();
    expect(useAppStore.getState().runs["session-1"]?.pendingApproval).toBeNull();
    expect(useAppStore.getState().runs["session-1"]?.waitingApproval).toBe(false);
  });
});


describe("approval replay protection", () => {
  it("suppresses browser prompts during submission and after reconnect", () => {
    const pending = { approval_id: "browser-replay-1", action: "request_secure_login" };
    expect(beginApprovalResolution("browser-session", pending.approval_id)).toBe(true);
    expect(isApprovalSuppressed("browser-session", pending)).toBe(true);
    expect(beginApprovalResolution("browser-session", pending.approval_id)).toBe(false);
    finishApprovalResolution("browser-session", pending.approval_id, true);
    expect(isApprovalSuppressed("browser-session", pending)).toBe(true);
    expect(isApprovalSuppressed("other-session", pending)).toBe(false);
    expect(isApprovalSuppressed("browser-session", { ...pending, approval_id: "new-request" })).toBe(false);
  });
  it("allows retry after a failed approval submission", () => {
    const pending = { approval_id: "failed-browser-1", action: "request_secure_login" };
    expect(beginApprovalResolution("retry-session", pending.approval_id)).toBe(true);
    finishApprovalResolution("retry-session", pending.approval_id, false);
    expect(isApprovalSuppressed("retry-session", pending)).toBe(false);
  });
});
