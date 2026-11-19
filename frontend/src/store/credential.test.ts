import { afterEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from "./index";
import { isCredentialApproval, isQuestionApproval } from "./helpers";

function jsonResponse(body: unknown) {
  return { ok: true, status: 200, json: async () => body };
}

const credPending = {
  approval_id: "credential:turn-1",
  sessionId: "sess-1",
  action: "request_credential",
  risk_class: "hitl",
  secret: true,
  label: "instagram password",
} as const;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  useAppStore.setState({ pendingApproval: null });
});

describe("credential approval routing", () => {
  it("classifies a credential frame as credential, not a question", () => {
    expect(isCredentialApproval(credPending)).toBe(true);
    expect(isQuestionApproval(credPending)).toBe(false);
  });

  it("a plain hitl question is NOT treated as a credential", () => {
    const q = { approval_id: "question:1", action: "ask_human", risk_class: "hitl" };
    expect(isCredentialApproval(q)).toBe(false);
    expect(isQuestionApproval(q)).toBe(true);
  });
});

describe("submitCredential", () => {
  it("POSTs the secret to the credential endpoint and resumes with a SAFE message", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ ok: true, credential_ref: "cred_x" }));
    vi.stubGlobal("fetch", fetchMock);

    const sendMessage = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({
      pendingApproval: { ...credPending },
      sessionId: "sess-1",
      sendMessage,
    });

    await useAppStore.getState().submitCredential("hunter2");

    // Banner cleared immediately.
    expect(useAppStore.getState().pendingApproval).toBeNull();

    // The secret was sent to the dedicated credential endpoint…
    const call = fetchMock.mock.calls.find(([url]) =>
      String(url).includes("/browser/credential"),
    );
    expect(call).toBeTruthy();
    const body = String((call![1] as RequestInit).body);
    expect(body).toContain("hunter2");

    // …and the resume message that WAS sent contains no secret.
    expect(sendMessage).toHaveBeenCalledTimes(1);
    const resume = String(sendMessage.mock.calls[0][0]);
    expect(resume).not.toContain("hunter2");
  });

  it("does nothing for an empty value", async () => {
    const sendMessage = vi.fn();
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    useAppStore.setState({ pendingApproval: { ...credPending }, sessionId: "sess-1", sendMessage });

    await useAppStore.getState().submitCredential("");

    expect(fetchMock).not.toHaveBeenCalled();
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it("ignores non-credential pending approvals", async () => {
    const sendMessage = vi.fn();
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    useAppStore.setState({
      pendingApproval: { approval_id: "question:1", action: "ask_human", risk_class: "hitl", sessionId: "sess-1" },
      sessionId: "sess-1",
      sendMessage,
    });

    await useAppStore.getState().submitCredential("hunter2");

    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("does not resume if the deposit request fails", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("network down")));
    const sendMessage = vi.fn();
    useAppStore.setState({ pendingApproval: { ...credPending }, sessionId: "sess-1", sendMessage });

    await useAppStore.getState().submitCredential("hunter2");

    expect(sendMessage).not.toHaveBeenCalled();
  });

  it.each(["network", "rejected"])("restores the prompt after a %s deposit failure", async (failure) => {
    const pending = { ...credPending, approval_id: `credential:failed-${failure}` };
    vi.stubGlobal("fetch", failure === "network"
      ? vi.fn().mockRejectedValue(new Error("network down"))
      : vi.fn().mockResolvedValue(jsonResponse({ ok: false, error: "deposit failed" })));
    const sendMessage = vi.fn();
    useAppStore.setState({ pendingApproval: pending, sessionId: "sess-1", sendMessage });
    await useAppStore.getState().submitCredential("test-secret");
    expect(useAppStore.getState().pendingApproval).toEqual(pending);
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it("deposits a credential-intent question through secure send", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ ok: true }));
    vi.stubGlobal("fetch", fetchMock);
    const sendMessage = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({ sessionId: "sess-1", sendMessage, pendingApproval: {
      approval_id: "question:secure-fallback", action: "ask_human", risk_class: "hitl",
      question: "Enter your password", sessionId: "sess-1",
    } });
    await useAppStore.getState().submitCredential("test-secret");
    expect(fetchMock).toHaveBeenCalled();
    expect(sendMessage).toHaveBeenCalledWith("Credential provided securely — continue signing in.");
  });

  it("retains only approval receipts across a module reload", async () => {
    const before = await import("./helpers");
    const pending = { ...credPending, approval_id: "credential:reload-test" };
    before.suppressCredentialApproval("reload-session", pending.approval_id);
    vi.resetModules();
    const after = await import("./helpers");
    expect(after.isCredentialApprovalSuppressed("reload-session", pending)).toBe(true);
    expect(after.isCredentialApprovalSuppressed("another-session", pending)).toBe(false);
    expect(after.isCredentialApprovalSuppressed("reload-session", { ...pending, approval_id: "credential:new" })).toBe(false);
  });

  it("cancels synthetic credential gates without calling the permission endpoint", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    useAppStore.setState({ sessionId: "sess-1", pendingApproval: {
      ...credPending, approval_id: "credential:cancel-test",
    } });
    await useAppStore.getState().cancelCredential();
    expect(useAppStore.getState().pendingApproval).toBeNull();
    expect(useAppStore.getState().runs["sess-1"].waitingApproval).toBe(false);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
