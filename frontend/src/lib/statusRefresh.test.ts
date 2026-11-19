import { afterAll, describe, expect, it, vi } from "vitest";
import { watchSessionStatus } from "./statusRefresh";

describe("watchSessionStatus (Requirement 18.3)", () => {
  let visibility: DocumentVisibilityState = "visible";
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => visibility });
  afterAll(() => { delete (document as { visibilityState?: unknown }).visibilityState; });

  it("refreshes when the tab becomes visible or the window gains focus", () => {
    let now = 0;
    const refresh = vi.fn();
    const stop = watchSessionStatus(refresh, { now: () => now });
    visibility = "hidden";
    document.dispatchEvent(new Event("visibilitychange"));
    expect(refresh).not.toHaveBeenCalled();
    visibility = "visible";
    document.dispatchEvent(new Event("visibilitychange"));
    expect(refresh).toHaveBeenCalledTimes(1);
    // Focus right after visibility is the same return to the tab.
    window.dispatchEvent(new Event("focus"));
    expect(refresh).toHaveBeenCalledTimes(1);
    now = 5_000;
    window.dispatchEvent(new Event("focus"));
    expect(refresh).toHaveBeenCalledTimes(2);
    stop();
    now = 10_000;
    document.dispatchEvent(new Event("visibilitychange"));
    expect(refresh).toHaveBeenCalledTimes(2);
  });
});
