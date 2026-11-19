import { afterEach, describe, expect, it, vi } from "vitest";
import { notifyPeerAlert } from "./peerNotifications";

function stubNotification(permission: NotificationPermission) {
  const instances: Array<{ title: string; body?: string; onclick: (() => void) | null; close: () => void }> = [];
  class FakeNotification {
    static permission = permission;
    static requestPermission = vi.fn(async () => permission);
    onclick: (() => void) | null = null;
    close = vi.fn();
    constructor(title: string, opts?: { body?: string }) {
      instances.push({ title, body: opts?.body, onclick: null, close: this.close });
    }
  }
  vi.stubGlobal("Notification", FakeNotification);
  return { FakeNotification, instances };
}

describe("notifyPeerAlert", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("always shows the toast regardless of visibility", () => {
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    const showToast = vi.fn();
    notifyPeerAlert(showToast, "Right wants to pair");
    expect(showToast).toHaveBeenCalledWith("Right wants to pair", {
      actionLabel: "Review",
      actionId: "open_agent_network",
    });
  });

  it("does not touch Notification when the tab is visible", () => {
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    const { FakeNotification } = stubNotification("granted");
    notifyPeerAlert(vi.fn(), "Right wants to pair");
    expect(FakeNotification.requestPermission).not.toHaveBeenCalled();
  });

  it("requests permission and shows a native notification when backgrounded", async () => {
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    const { instances } = stubNotification("granted");
    notifyPeerAlert(vi.fn(), "Right wants to pair");
    await Promise.resolve();
    await Promise.resolve();
    expect(instances).toHaveLength(1);
    expect(instances[0].body).toBe("Right wants to pair");
  });

  it("never shows a native notification if permission was denied", async () => {
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    const { instances } = stubNotification("denied");
    notifyPeerAlert(vi.fn(), "Right wants to pair");
    await Promise.resolve();
    await Promise.resolve();
    expect(instances).toHaveLength(0);
  });

  it("only asks for permission once when it starts out undecided", async () => {
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    const { FakeNotification } = stubNotification("default");
    notifyPeerAlert(vi.fn(), "First alert");
    await Promise.resolve();
    await Promise.resolve();
    notifyPeerAlert(vi.fn(), "Second alert");
    await Promise.resolve();
    await Promise.resolve();
    expect(FakeNotification.requestPermission).toHaveBeenCalledTimes(1);
  });
});
