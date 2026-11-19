import { describe, expect, it } from "vitest";
import {
  THREAD_PAGE,
  activeThreads,
  clampVisible,
  growVisible,
} from "./threadPaging";

describe("threadPaging", () => {
  it("drops archived sessions", () => {
    expect(
      activeThreads([
        { id: "a" },
        { id: "b", archived: true },
        { id: "c", archived: false },
      ]).map((s) => s.id),
    ).toEqual(["a", "c"]);
  });

  it("groups workers under their parent even when an older backend omits the worker flag", () => {
    expect(activeThreads([
      { session_id: "parent", parent_session_id: "parent", is_subagent: false },
      { session_id: "worker", parent_session_id: "parent", is_subagent: false },
      { session_id: "ordinary" },
    ]).map(s => s.session_id)).toEqual(["parent", "ordinary"]);
  });

  it("clamps the visible window when the list shrinks or grows", () => {
    expect(clampVisible(60, 10)).toBe(THREAD_PAGE);
    expect(clampVisible(60, 100)).toBe(60);
    expect(clampVisible(10, 100)).toBe(THREAD_PAGE);
  });

  it("grows by one page without passing total", () => {
    expect(growVisible(THREAD_PAGE, 100)).toBe(THREAD_PAGE * 2);
    expect(growVisible(90, 100)).toBe(100);
  });
});
