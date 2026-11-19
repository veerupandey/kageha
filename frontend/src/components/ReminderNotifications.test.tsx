import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ReminderNotifications } from "./ReminderNotifications";
const fixture = vi.hoisted(() => ({ openSession: vi.fn(), api: vi.fn() }));
vi.mock("../store", () => ({ useAppStore: (selector: (s: unknown) => unknown) => selector({ openSession: fixture.openSession }) }));
vi.mock("../api/client", () => ({ api: fixture.api }));
const item = { id: "a".repeat(32), title: "Reminder", body: "Interview tomorrow at 10 AM", origin_session_id: "parent", session_id: "child", status: "success" };
beforeEach(() => { fixture.openSession.mockReset(); fixture.api.mockReset(); fixture.api.mockResolvedValue({ notifications: [item] }); });
afterEach(cleanup);
it("shows reminders already received while the browser was closed", async () => {
  render(<ReminderNotifications />);
  fireEvent.click(await screen.findByRole("button", { name: "Reminders · 1 unread" }));
  expect(screen.getByText(item.body)).toBeTruthy();
});
it("opens the originating chat and persists acknowledgment", async () => {
  render(<ReminderNotifications />);
  fireEvent.click(await screen.findByRole("button", { name: "Reminders · 1 unread" }));
  fireEvent.click(screen.getByRole("button", { name: "Open chat" }));
  await waitFor(() => expect(fixture.openSession).toHaveBeenCalledWith("parent"));
  await waitFor(() => expect(fixture.api).toHaveBeenCalledWith(`/api/notifications/${item.id}/read`, { method: "POST" }));
  await waitFor(() => expect(screen.queryByLabelText("Reminder notifications")).toBeNull());
});
it("retains the reminder when acknowledgment fails", async () => {
  fixture.api.mockImplementation((url: string) => url.endsWith("/read") ? Promise.reject(new Error("offline")) : Promise.resolve({ notifications: [item] }));
  render(<ReminderNotifications />);
  fireEvent.click(await screen.findByRole("button", { name: "Reminders · 1 unread" }));
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.getByText(item.body)).toBeTruthy();
});
