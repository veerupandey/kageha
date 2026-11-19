import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { api } from "../../api/client";
import { KnowledgePanel } from "./KnowledgePanel";
vi.mock("../../api/client", () => ({ api: vi.fn() }));
beforeEach(() => {
  vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});
function fixture(secondCursor: number | null | undefined) {
  vi.mocked(api).mockImplementation(async (route: string) => {
    if (route.startsWith("/api/knowledge?"))
      return { enabled: true, memories: [] } as never;
    if (route === "/api/knowledge-bases")
      return {
        items: [{ id: "notes", sources: ["runbook.md"], stats: { chunks: 2 } }],
      } as never;
    if (route.includes("cursor=0"))
      return {
        items: [
          { id: "one", source: "runbook.md", text: "First page passage" },
        ],
        total: 2,
        next_cursor: secondCursor,
      } as never;
    if (route.includes("cursor=1"))
      return {
        items: [
          { id: "two", source: "runbook.md", text: "Second page passage" },
        ],
        total: 2,
        next_cursor: null,
      } as never;
    throw new Error("Unexpected route: " + route);
  });
}
describe("knowledge chunk paging", () => {
  it("shows passages from all server pages", async () => {
    fixture(1);
    render(<KnowledgePanel onClose={() => {}} />);
    fireEvent.click(
      await screen.findByTitle("Inspect actual chunks and AST boundaries"),
    );
    expect(await screen.findByText("Second page passage")).toBeInTheDocument();
    expect(screen.getByText("First page passage")).toBeInTheDocument();
  });
  it("accepts legacy servers without a cursor", async () => {
    fixture(undefined);
    render(<KnowledgePanel onClose={() => {}} />);
    fireEvent.click(
      await screen.findByTitle("Inspect actual chunks and AST boundaries"),
    );
    expect(await screen.findByText("First page passage")).toBeInTheDocument();
    expect(
      vi
        .mocked(api)
        .mock.calls.filter(([route]) => String(route).includes("/chunks")),
    ).toHaveLength(1);
  });
  it("stops a repeated cursor instead of looping", async () => {
    fixture(0);
    render(<KnowledgePanel onClose={() => {}} />);
    fireEvent.click(
      await screen.findByTitle("Inspect actual chunks and AST boundaries"),
    );
    expect(
      await screen.findByText(
        "Knowledge server returned an invalid chunk cursor",
      ),
    ).toBeInTheDocument();
  });
});
