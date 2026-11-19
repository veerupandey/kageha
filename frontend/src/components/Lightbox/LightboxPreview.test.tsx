import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { LightboxPreview } from "./LightboxPreview";

vi.mock("../shared/WordDocumentPreview", () => ({
  WordDocumentPreview: ({ compact }: { compact?: boolean }) => <div>Word preview {compact ? "panel" : "popup"}</div>,
}));
afterEach(cleanup);

it("opens DOCX files in the popup Word viewer", () => {
  render(<LightboxPreview item={{ path: "report.DOCX", caption: "report.DOCX", url: "/report.DOCX", kind: "document" }} />);
  expect(screen.getByText("Word preview popup")).toBeVisible();
});

it("keeps the native-file fallback for other document formats", () => {
  render(<LightboxPreview item={{ path: "report.doc", caption: "report.doc", url: "/report.doc", kind: "document" }} />);
  expect(screen.getByRole("link", { name: "Open file" })).toHaveAttribute("href", "/report.doc");
});
