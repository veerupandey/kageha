import { describe, expect, it } from "vitest";
import { STAGE_CHROME_HOLD_MS, stageChromeVisible } from "./stageChrome";

describe("stageChromeVisible", () => {
  it("is always visible when You control", () => {
    expect(stageChromeVisible({
      controlling: true,
      pointerInside: false,
      focusWithin: false,
      recentActionAt: 0,
      now: 10_000,
    })).toBe(true);
  });

  it("hides while the agent drives with no hover, focus, or recent action", () => {
    expect(stageChromeVisible({
      controlling: false,
      pointerInside: false,
      focusWithin: false,
      recentActionAt: 0,
      now: 10_000,
    })).toBe(false);
  });

  it("shows on hover or focus while the agent drives", () => {
    expect(stageChromeVisible({
      controlling: false,
      pointerInside: true,
      focusWithin: false,
      recentActionAt: 0,
      now: 10_000,
    })).toBe(true);
    expect(stageChromeVisible({
      controlling: false,
      pointerInside: false,
      focusWithin: true,
      recentActionAt: 0,
      now: 10_000,
    })).toBe(true);
  });

  it("shows for a short hold after an action", () => {
    expect(stageChromeVisible({
      controlling: false,
      pointerInside: false,
      focusWithin: false,
      recentActionAt: 10_000,
      now: 10_000 + STAGE_CHROME_HOLD_MS - 1,
    })).toBe(true);
    expect(stageChromeVisible({
      controlling: false,
      pointerInside: false,
      focusWithin: false,
      recentActionAt: 10_000,
      now: 10_000 + STAGE_CHROME_HOLD_MS,
    })).toBe(false);
  });
});
