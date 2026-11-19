import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useAppStore } from "../../store";
import { SidebarHeader } from "./SidebarHeader";

afterEach(cleanup);

it("imports a selected ZIP, shows progress, and allows selecting the same file again", async () => {
  let finish!: () => void;
  const importSession = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
  const onImported = vi.fn();
  useAppStore.setState({ importSession });
  render(<SidebarHeader onNewThread={vi.fn()} onImported={onImported} />);
  const file = new File(["archive"], "session.zip", { type: "application/zip" });
  const input = screen.getByLabelText("Session archive");
  fireEvent.change(input, { target: { files: [file] } });
  expect(importSession).toHaveBeenCalledWith(file);
  expect(screen.getByRole("button", { name: "Importing session…" })).toBeDisabled();
  finish();
  await waitFor(() => expect(onImported).toHaveBeenCalledOnce());
  expect(screen.getByRole("button", { name: "Import session" })).toBeEnabled();
  fireEvent.change(input, { target: { files: [file] } });
  expect(importSession).toHaveBeenCalledTimes(2);
  finish();
  await waitFor(() => expect(onImported).toHaveBeenCalledTimes(2));
});

it("stays on the current view when import fails and restores the import control", async () => {
  const importSession = vi.fn().mockRejectedValue(new Error("Invalid archive"));
  const onImported = vi.fn();
  useAppStore.setState({ importSession });
  render(<SidebarHeader onNewThread={vi.fn()} onImported={onImported} />);
  fireEvent.change(screen.getByLabelText("Session archive"), {
    target: { files: [new File(["bad"], "session.zip")] },
  });
  await waitFor(() => expect(screen.getByRole("button", { name: "Import session" })).toBeEnabled());
  expect(onImported).not.toHaveBeenCalled();
});
