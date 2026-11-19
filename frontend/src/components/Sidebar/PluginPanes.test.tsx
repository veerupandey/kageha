import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { PluginPanes, type PluginPane } from "./PluginPanes";

afterEach(cleanup);

it("renders declarative blocks, stale status, and literal plugin text", () => {
  const pane: PluginPane = {
    mod: "modernize:status.py", title: "Module map", stale: true,
    blocks: [
      { type: "text", text: "<script>alert('not markup')</script>" },
      { type: "progress", label: "Touched", value: 1, total: 2 },
      { type: "table", columns: ["Phase", "Status"], rows: [["Verify", "Review"]] },
      { type: "modules", items: [{ name: "app.py", state: "needs-human", detail: "Tests need review" }] },
      { type: "links", items: [{ label: "Reference", url: "https://example.com/docs" }] },
    ],
  };
  const { container } = render(<PluginPanes panes={[pane]} />);
  expect(screen.getByRole("region", { name: "Module map" })).toBeInTheDocument();
  expect(screen.getByRole("progressbar", { name: "Touched" })).toHaveAttribute("value", "1");
  expect(screen.getByRole("table")).toHaveTextContent("Verify");
  expect(screen.getByText("needs-human")).toBeInTheDocument();
  expect(screen.getByText(/Last valid view/)).toBeInTheDocument();
  expect(container.querySelector("script")).toBeNull();
  expect(screen.getByText("<script>alert('not markup')</script>")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Reference" })).toHaveAttribute("rel", "noopener noreferrer");
});

it("does not make unsafe or credential-bearing URLs clickable", () => {
  render(<PluginPanes panes={[{
    mod: "demo:ui.py", title: "Links", stale: false,
    blocks: [{ type: "links", items: [
      { label: "Script", url: "javascript:alert(1)" },
      { label: "Credentials", url: "https://user:password@example.com" },
    ] }],
  }]} />);
  expect(screen.queryByRole("link")).not.toBeInTheDocument();
  expect(screen.getByText("Script (invalid link)")).toBeInTheDocument();
});
