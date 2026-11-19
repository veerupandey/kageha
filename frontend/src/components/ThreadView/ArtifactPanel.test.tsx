import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ArtifactPanel } from "./ArtifactPanel";
import { useAppStore } from "../../store";

vi.mock("./ArtifactThumb", () => ({
  ArtifactThumb: ({ item, onClick }: { item: { caption: string }; onClick: () => void }) =>
    <button onClick={onClick}>{item.caption}</button>,
}));
vi.mock("../shared/WordDocumentPreview", () => ({
  WordDocumentPreview: ({ compact }: { compact?: boolean }) => <div>Word preview {compact ? "panel" : "popup"}</div>,
}));

beforeEach(() => {
  useAppStore.setState({
    sessionId: "board-test", runStatus: "idle", canvasSelectedPath: null,
    canvasTurnPaths: new Set(),
    canvasItems: [
      { path: "artifacts/hero.png", caption: "Hero image", url: "/hero.png", kind: "image" },
      { path: "artifacts/alternate.png", caption: "Alternate image", url: "/alternate.png", kind: "image" },
    ],
  });
});
afterEach(cleanup);

it("shows and opens an answer's workspace-relative deliverable", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, text: async () => "# Profile\nActual file contents" }));
  try {
    useAppStore.setState({
      messages: [{ id: "answer", role: "assistant", text: "See `outputs/profile.md`." }],
      canvasItems: [{ path: "workspace/outputs/profile.md", caption: "profile.md", url: "/profile.md", kind: "markdown" }],
    });
    render(<ArtifactPanel filter="all" onOpenLightbox={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Deliverables (1)" }));
    fireEvent.click(screen.getByRole("button", { name: "profile.md" }));
    expect(await screen.findByText("Actual file contents")).toBeVisible();
  } finally {
    vi.unstubAllGlobals();
  }
});

it("does not count an unregistered output as an available deliverable", () => {
  useAppStore.setState({
    messages: [{ id: "answer", role: "assistant", text: "See `outputs/missing.md`." }],
    canvasItems: [{ path: "outputs/missing.md", caption: "missing.md", url: "/missing.md", kind: "markdown", available: false }],
  });
  render(<ArtifactPanel filter="all" onOpenLightbox={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Deliverables (0)" }));
  expect(screen.getByText("No deliverables yet")).toBeVisible();
});

it("opens a board tile in the panel, expands it, and returns to the board", () => {
  const expand = vi.fn();
  render(<ArtifactPanel filter="all" onOpenLightbox={expand} />);
  expect(screen.getByRole("button", { name: "Board" })).toHaveAttribute("aria-pressed", "true");
  fireEvent.click(screen.getByRole("button", { name: "Hero image" }));
  expect(screen.getByRole("button", { name: "Open fullscreen image preview" })).toHaveAttribute("src", "/hero.png");
  fireEvent.click(screen.getByRole("button", { name: /Expand/ }));
  expect(expand).toHaveBeenCalledWith("artifacts/hero.png");
  fireEvent.click(screen.getByRole("button", { name: /All files/ }));
  expect(screen.getByRole("button", { name: "Alternate image" })).toBeVisible();
});

it("keeps list navigation available and respects empty filters", () => {
  const { rerender } = render(<ArtifactPanel filter="all" onOpenLightbox={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "List" }));
  expect(screen.getByRole("button", { name: "List" })).toHaveAttribute("aria-pressed", "true");
  rerender(<ArtifactPanel filter="documents" onOpenLightbox={vi.fn()} />);
  expect(screen.getByText("Your files will appear here")).toBeVisible();
  expect(screen.queryByRole("button", { name: "Hero image" })).not.toBeInTheDocument();
});

it("previews Word documents in the right panel and expands the selected file", () => {
  const item = { path: "artifacts/report.docx", caption: "report.docx", url: "/report.docx", kind: "document" as const };
  useAppStore.setState({ canvasItems: [item] });
  const expand = vi.fn();
  render(<ArtifactPanel filter="documents" onOpenLightbox={expand} />);
  fireEvent.click(screen.getByRole("button", { name: "report.docx" }));
  expect(screen.getByText("Word preview panel")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: /Expand/ }));
  expect(expand).toHaveBeenCalledWith(item.path);
});


it("keeps artifact text beyond the former 50000-character cutoff", async () => {
  const body = "Begin. " + "Artifact content. ".repeat(4000) + "Final artifact sentence.";
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, text: async () => body }));
  try {
    useAppStore.setState({ canvasItems: [{ path: "artifacts/large.txt", caption: "large.txt", url: "/large.txt", kind: "text" }] });
    const { container } = render(<ArtifactPanel filter="all" onOpenLightbox={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "large.txt" }));
    await waitFor(() => expect(container.querySelector("pre")?.textContent).toBe(body));
  } finally {
    vi.unstubAllGlobals();
  }
});
