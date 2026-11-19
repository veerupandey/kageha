import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { PersistenceSettings } from "./PersistenceSettings";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../api/client", () => ({ api: mocks.api }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it("saves request-wide limits with minutes converted to seconds", async () => {
  let saved: unknown;
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return { policy: { enabled: true, max_duration_seconds: 86400, max_model_calls: 5000 } };
    saved = JSON.parse(options.body); return { policy: saved };
  });
  render(<PersistenceSettings />);
  fireEvent.change(await screen.findByLabelText("Persistence time limit in minutes"), { target: { value: "60" } });
  fireEvent.change(screen.getByLabelText("Persistence model call limit"), { target: { value: "250" } });
  fireEvent.click(screen.getByRole("button", { name: "Save persistence settings" }));
  expect(await screen.findByRole("status")).toHaveTextContent("Applies to new requests");
  expect(saved).toEqual({ enabled: true, max_duration_seconds: 3600, max_model_calls: 250 });
});

it("rejects limits above the ceiling and keeps failed saves editable", async () => {
  mocks.api.mockImplementation(async (_path: string, options?: { body: string }) => {
    if (!options) return { policy: { enabled: true, max_duration_seconds: 86400, max_model_calls: 5000 } };
    throw new Error("Settings service unavailable");
  });
  render(<PersistenceSettings />);
  const calls = await screen.findByLabelText("Persistence model call limit");
  fireEvent.change(calls, { target: { value: "100001" } });
  expect(screen.getByRole("button", { name: "Save persistence settings" })).toBeDisabled();
  fireEvent.change(calls, { target: { value: "800" } });
  fireEvent.click(screen.getByRole("button", { name: "Save persistence settings" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Settings service unavailable");
  expect(calls).toHaveValue(800);
  expect(screen.getByRole("button", { name: "Save persistence settings" })).toBeEnabled();
});
