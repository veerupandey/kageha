import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { PluginCatalog } from "./PluginCatalog";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../../api/client", () => ({ api: mocks.api }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it("filters catalog entries and starts review by name", async () => {
  mocks.api.mockResolvedValue({ status: "available", plugins: [
    { name: "design", description: "Design interfaces", source: "https://example.test/design" },
    { name: "docs", description: "Documentation" },
  ] });
  const review = vi.fn();
  render(<PluginCatalog installed={[]} busy={false} onPreview={review} />);
  fireEvent.click(await screen.findByRole("button", { name: "Review design" }));
  expect(review).toHaveBeenCalledWith("design");
  fireEvent.change(screen.getByLabelText("Search plugins"), { target: { value: "nonexistent" } });
  expect(screen.getByText("No matching plugins.")).toBeInTheDocument();
});

it("distinguishes unavailable catalogs from no matches and supports retry", async () => {
  mocks.api.mockResolvedValueOnce({ status: "unavailable", plugins: [] });
  mocks.api.mockResolvedValue({ status: "available", plugins: [{ name: "docs", description: "Docs" }] });
  render(<PluginCatalog installed={["docs"]} busy={false} onPreview={vi.fn()} />);
  expect(await screen.findByText("Plugin catalog is unavailable.")).toBeInTheDocument();
  expect(screen.queryByText("No matching plugins.")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Refresh catalog" }));
  expect(await screen.findByRole("button", { name: "Review docs" })).toBeDisabled();
  expect(mocks.api).toHaveBeenCalledWith("/api/plugins/catalog?refresh=1");
});
