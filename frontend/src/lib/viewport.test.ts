import { describe, expect, it } from "vitest";
import { isNarrowViewport, NARROW_VIEWPORT_MAX_PX } from "./viewport";

describe("isNarrowViewport", () => {
  it("is true at the phone/tablet breakpoint", () => {
    expect(isNarrowViewport({ matches: true })).toBe(true);
  });

  it("is false on wide layouts", () => {
    expect(isNarrowViewport({ matches: false })).toBe(false);
  });

  it("is false when media is unavailable", () => {
    expect(isNarrowViewport(null)).toBe(false);
    expect(isNarrowViewport(undefined)).toBe(false);
  });

  it("exports the CSS-aligned breakpoint", () => {
    expect(NARROW_VIEWPORT_MAX_PX).toBe(1023);
  });
});
