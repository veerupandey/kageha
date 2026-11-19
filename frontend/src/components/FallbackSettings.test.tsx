import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { FallbackSettings } from "./FallbackSettings";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../api/client", () => ({ api: mocks.api }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const inventory = () => ({
  policy: { enabled: true, providers: { custom: { mode: "never", allow_fallback: false } }, models: {} },
  providers: ["custom", "other"], models: [{ id: "custom/model", provider: "custom" }],
});

it("saves provider and model policies and global fallback without special provider names", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return inventory();
    saved = JSON.parse(options.body);
    return { policy: saved, message: "Saved for new runs" };
  });
  render(<FallbackSettings />);
  expect(await screen.findByLabelText("Automatic fallback participation")).toHaveValue("never");
  fireEvent.change(screen.getByLabelText("Automatic fallback participation"), { target: { value: "last" } });
  fireEvent.change(screen.getByLabelText("Fallback rule scope"), { target: { value: "models" } });
  expect(screen.getByText("Effective: last · stop on failure")).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("Automatic fallback participation"), { target: { value: "normal" } });
  fireEvent.change(screen.getByLabelText("Fallback after selected model fails"), { target: { value: "true" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Allow automatic model fallback" }));
  fireEvent.click(screen.getByRole("button", { name: "Save fallback settings" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Saved for new runs");
  expect(saved).toEqual({ enabled: false, providers: { custom: { mode: "last", allow_fallback: false } },
    models: { "custom/model": { mode: "normal", allow_fallback: true } } });
});

it("clears model overrides to inherit provider preferences", async () => {
  const data = inventory();
  mocks.api.mockResolvedValue({ ...data, policy: { ...data.policy, models: { "custom/model": { mode: "normal" } } } });
  render(<FallbackSettings />);
  await screen.findByLabelText("Fallback rule scope");
  fireEvent.change(screen.getByLabelText("Fallback rule scope"), { target: { value: "models" } });
  fireEvent.change(screen.getByLabelText("Automatic fallback participation"), { target: { value: "inherit" } });
  expect(screen.getByText("Effective: never · stop on failure")).toBeInTheDocument();
});

it("keeps edits when saving fails", async () => {
  mocks.api.mockResolvedValueOnce(inventory()).mockRejectedValue(new Error("Could not save settings"));
  render(<FallbackSettings />);
  fireEvent.change(await screen.findByLabelText("Automatic fallback participation"), { target: { value: "last" } });
  fireEvent.click(screen.getByRole("button", { name: "Save fallback settings" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Could not save settings");
  expect(screen.getByLabelText("Automatic fallback participation")).toHaveValue("last");
  expect(screen.getByRole("button", { name: "Save fallback settings" })).toBeEnabled();
});

it.each([{}, null, { policy: {}, providers: [], models: [] }])("shows an error instead of crashing for malformed inventory %j", async (response) => {
  mocks.api.mockResolvedValueOnce(response);
  render(<FallbackSettings />);
  expect(await screen.findByRole("alert")).toHaveTextContent("invalid response");
  expect(screen.getByRole("heading", { name: "Fallback behavior" })).toBeInTheDocument();
  expect(screen.queryByLabelText("Fallback rule scope")).not.toBeInTheDocument();
});

it("preserves edits when the save response is malformed", async () => {
  mocks.api.mockResolvedValueOnce(inventory()).mockResolvedValueOnce({});
  render(<FallbackSettings />);
  fireEvent.change(await screen.findByLabelText("Automatic fallback participation"), { target: { value: "last" } });
  fireEvent.click(screen.getByRole("button", { name: "Save fallback settings" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("invalid response");
  expect(screen.getByLabelText("Automatic fallback participation")).toHaveValue("last");
  expect(screen.getByRole("button", { name: "Save fallback settings" })).toBeEnabled();
});
