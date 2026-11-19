import { afterEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from "./index";

function jsonResponse(body: unknown) {
  return { ok: true, status: 200, json: async () => body };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("syncPermissions", () => {
  it("adopts the server's process-wide grant on boot / refresh", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse({ ok: true, scope: "full", auto_approve: true, sandbox_network: true }),
      ),
    );
    useAppStore.setState({ autoApprove: false, permissionScope: "ask" });

    await useAppStore.getState().syncPermissions();

    expect(useAppStore.getState().autoApprove).toBe(true);
    expect(useAppStore.getState().permissionScope).toBe("full");
  });

  it("falls back to the locally persisted scope when the request fails", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("network down")));
    useAppStore.setState({ autoApprove: false, permissionScope: "ask" });

    await useAppStore.getState().syncPermissions();

    expect(useAppStore.getState().permissionScope).toBe("ask");
  });

  it("ignores a non-ok payload rather than clearing an active grant", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse({ ok: false })));
    useAppStore.setState({ autoApprove: true, permissionScope: "session" });

    await useAppStore.getState().syncPermissions();

    expect(useAppStore.getState().autoApprove).toBe(true);
    expect(useAppStore.getState().permissionScope).toBe("session");
  });
});
