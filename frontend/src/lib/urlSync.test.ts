import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { applyUrlSurface, parseUrlState, updateUrl } from "./urlSync";

describe("urlSync", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", "/");
  });

  afterEach(() => {
    window.history.replaceState(null, "", "/");
  });

  it("opens the exact shared Computer session without granting control", () => {
    window.history.replaceState(null, "", "/?session=cli-chat&surface=computer");
    const state = parseUrlState();
    const pinned: string[] = [];
    expect(applyUrlSurface(state, {sessionId:"other-chat",pinLiveSurface:surface=>pinned.push(surface)})).toBe(false);
    expect(applyUrlSurface(state, {sessionId:"cli-chat",pinLiveSurface:surface=>pinned.push(surface)})).toBe(true);
    expect(pinned).toEqual(["computer"]);
    window.history.replaceState(null, "", "/?session=cli-chat&surface=unknown");
    expect(parseUrlState().surface).toBeUndefined();
  });

  it("parses session from query parameters", () => {
    window.history.replaceState(null, "", "/?session=sess-123&agent=worker-456");
    const parsed = parseUrlState();
    expect(parsed.sessionId).toBe("sess-123");
    expect(parsed.agentId).toBe("worker-456");
  });

  it("parses session from shorthand s parameter", () => {
    window.history.replaceState(null, "", "/?s=sess-short");
    expect(parseUrlState().sessionId).toBe("sess-short");
  });

  it("parses session from /c/ path", () => {
    window.history.replaceState(null, "", "/c/sess-path");
    expect(parseUrlState().sessionId).toBe("sess-path");
  });

  it("updates URL with new session without reloading", () => {
    updateUrl({ sessionId: "sess-abc" });
    expect(window.location.search).toContain("session=sess-abc");
    expect(parseUrlState().sessionId).toBe("sess-abc");
  });

  it("updates agent parameter and cleans it up when dismissed", () => {
    updateUrl({ sessionId: "sess-abc", agentId: "agent-xyz" });
    expect(window.location.search).toContain("agent=agent-xyz");
    expect(parseUrlState().agentId).toBe("agent-xyz");

    updateUrl({ agentId: null });
    expect(window.location.search).not.toContain("agent=");
    expect(parseUrlState().sessionId).toBe("sess-abc");
  });

  it("clears session parameter when navigating to new chat", () => {
    updateUrl({ sessionId: "sess-abc" });
    updateUrl({ sessionId: null });
    expect(window.location.search).not.toContain("session=");
    expect(parseUrlState().sessionId).toBeNull();
  });

  it("supports replaceState to update in-place without adding extra history entries", () => {
    updateUrl({ sessionId: "sess-1" });
    updateUrl({ sessionId: "sess-2" }, { replace: true });
    expect(parseUrlState().sessionId).toBe("sess-2");
  });

  it("handles empty or root location safely", () => {
    window.history.replaceState(null, "", "/");
    expect(parseUrlState()).toEqual({ sessionId: null, agentId: null });
  });
});
