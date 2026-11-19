import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { ActionSummaryCard } from "./ActionSummaryCard";
afterEach(cleanup);
it("mounts expensive tool details only while expanded and ignores nested toggles", () => {
  const view = render(<ActionSummaryCard cards={[{ id: "read", name: "read_file", status: "done" }]}>
    <details><summary>Nested details</summary><div>Expensive tool output</div></details>
  </ActionSummaryCard>);
  const outer = view.container.querySelector("details")!;
  expect(view.queryByText("Expensive tool output")).toBeNull();
  outer.open = true;
  fireEvent(outer, new Event("toggle"));
  expect(view.getByText("Expensive tool output")).toBeTruthy();
  const inner = outer.querySelector("details")!;
  fireEvent(inner, new Event("toggle", { bubbles: true }));
  expect(outer.open).toBe(true);
  outer.open = false;
  fireEvent(outer, new Event("toggle"));
  expect(view.queryByText("Expensive tool output")).toBeNull();
});
it("renders details immediately when initially expanded", () => {
  const view = render(<ActionSummaryCard defaultOpen cards={[{ id: "read", name: "read_file", status: "done" }]}><span>Tool output</span></ActionSummaryCard>);
  expect(view.getByText("Tool output")).toBeTruthy();
});
