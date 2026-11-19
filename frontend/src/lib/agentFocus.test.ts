import { describe, expect, it } from "vitest";
import { shouldAgentFocus } from "./agentFocus";

describe("shouldAgentFocus", () => {
  it("never steals the pane while You hold a lease", () => {
    expect(shouldAgentFocus({
      pinnedAt: null,
      controlling: true,
      now: 10_000,
      firstFrameOfTurn: true,
    })).toBe(false);
  });

  it("respects a user pin even before the first live frame", () => {
    expect(shouldAgentFocus({
      pinnedAt: 9_000,
      controlling: false,
      now: 10_000,
      firstFrameOfTurn: true,
    })).toBe(false);
  });

  it("respects a user pin after the first frame", () => {
    expect(shouldAgentFocus({
      pinnedAt: 9_000,
      controlling: false,
      now: 10_000,
      firstFrameOfTurn: false,
    })).toBe(false);
  });

  it("reveals when nothing is pinned and the human is not driving", () => {
    expect(shouldAgentFocus({
      pinnedAt: null,
      controlling: false,
      now: 10_000,
      firstFrameOfTurn: false,
    })).toBe(true);
  });
});
