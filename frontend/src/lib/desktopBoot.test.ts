import { describe, expect, it } from "vitest";
import { desktopActionLabel, desktopBootCopy, desktopBootSteps, desktopToolNarration, desktopSessionNarration, desktopWsUrl, rfbViewerMode } from "./desktopBoot";

describe("desktopBootCopy", () => {
  it("prioritizes a startup failure over stale screen and reconnect state", () => {
    expect(desktopBootCopy({ blocked: true, adopted: true, phase: "screen", screenReady: true, connecting: true, detail: "Docker unavailable" })).toEqual({
      title: "Computer unavailable",
      sub: "Docker unavailable",
    });
    expect(desktopBootCopy({ blocked: true }).sub).toContain("Check Docker");
  });
  it("walks boot phases before the screen is up", () => {
    expect(desktopBootCopy({ phase: "image" }).title).toMatch(/Pulling/);
    expect(desktopBootCopy({ phase: "desktop" }).title).toMatch(/Starting this computer/);
    expect(desktopBootCopy({ phase: "screen" }).title).toMatch(/Connecting the screen/);
  });

  it("uses reconnect copy for an adopted box", () => {
    expect(desktopBootCopy({ adopted: true, screenReady: false }).title).toMatch(/Reconnecting/);
    expect(
      desktopBootCopy({ adopted: true, screenReady: true, connecting: true }).title,
    ).toMatch(/Reconnected/);
  });

  it("names the screen-before-browser gap", () => {
    expect(
      desktopBootCopy({ screenReady: true, browserReady: false }).sub,
    ).toMatch(/browser is still starting/);
  });
});

describe("rfbViewerMode", () => {
  it("raises quality while Live is visible and lowers it when hidden", () => {
    expect(rfbViewerMode({ controlling: false, visible: true }).qualityLevel).toBe(6);
    expect(rfbViewerMode({ controlling: true, visible: true }).qualityLevel).toBe(8);
    expect(rfbViewerMode({ controlling: true, visible: false }).qualityLevel).toBe(2);
    expect(rfbViewerMode({ controlling: true, visible: false }).viewOnly).toBe(true);
    expect(rfbViewerMode({ controlling: true, visible: true }).viewOnly).toBe(false);
  });
});

describe("desktopActionLabel", () => {
  it("turns computer_* into a HUD chip", () => {
    expect(desktopActionLabel("click")).toBe("Clicking");
    expect(desktopActionLabel("computer_type")).toBe("Typing");
    expect(desktopActionLabel("observe", "Looking at Firefox")).toBe("Looking at Firefox");
  });
});

describe("desktop session narration", () => {
  it("prefers Opening host when the agent launches a browser", () => {
    expect(desktopToolNarration("browser_open", "url=https://www.amazon.ca/s")).toBe(
      "Opening amazon.ca",
    );
    expect(
      desktopSessionNarration({
        toolName: "browser_open",
        toolPreview: "https://example.com",
        running: true,
      }),
    ).toBe("Opening example.com");
  });

  it("walks boot steps like a recorded session", () => {
    expect(desktopBootSteps({ phase: "desktop" }).map((step) => step.state)).toEqual([
      "done",
      "current",
      "wait",
    ]);
    expect(desktopBootSteps({ screenReady: true }).every((step) => step.state === "done")).toBe(
      true,
    );
  });
});

describe("desktopWsUrl", () => {
  it("stays on the UI origin", () => {
    expect(desktopWsUrl("sess-1")).toContain("/api/sessions/sess-1/desktop/ws");
    expect(desktopWsUrl("sess-1")).not.toContain("127.0.0.1");
  });
});
