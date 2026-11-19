import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { api } from "../api/client";
import { GoogleCalendarIntegration, GoogleDriveIntegration } from "./GoogleCalendarIntegration";

vi.mock("../api/client", () => ({ api: vi.fn() }));
const base = "/api/integrations/google-calendar";
const disconnected = { connected: false, configured: true, pending: false, agents: ["main"], email: "" };
beforeEach(() => {
  vi.mocked(api).mockReset();
  vi.mocked(api).mockImplementation(async (path, opts) => {
    if (path === "/api/agents") return { agents: [{ id: "main", name: "Main Agent" }] } as never;
    if (path === base + "/setup") return { client_id: "client", redirect_uri: "", configured: true, secret_configured: true } as never;
    if (path === base + "/connect") return { authorization_url: "https://accounts.google.com/o/oauth2/v2/auth?state=fixture" } as never;
    if (path === base || path === base + "/cancel") return disconnected as never;
    throw new Error(`Unexpected route: ${path} ${opts?.method ?? "GET"}`);
  });
});

it("offers Google authorization and a fallback link when popups are blocked", async () => {
  const popup = vi.spyOn(window, "open").mockReturnValue(null);
  render(<GoogleCalendarIntegration />);
  fireEvent.click(screen.getByRole("button", { name: "Google Calendar integration" }));
  fireEvent.click(await screen.findByRole("button", { name: "Connect Google Calendar" }));
  expect(await screen.findByRole("link", { name: "Open Google sign-in" })).toHaveAttribute("href", "https://accounts.google.com/o/oauth2/v2/auth?state=fixture");
  fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
  await waitFor(() => expect(screen.queryByText("Finish connecting on Google.")).not.toBeInTheDocument());
  expect(api).toHaveBeenCalledWith(base + "/cancel", { method: "POST" });
  popup.mockRestore();
});

it("saves the separate calendar OAuth configuration", async () => {
  render(<GoogleCalendarIntegration />);
  fireEvent.click(screen.getByRole("button", { name: "Google Calendar integration" }));
  fireEvent.click(await screen.findByText("Google OAuth setup"));
  expect((await screen.findByLabelText("Authorized redirect URI") as HTMLInputElement).value).toContain("/api/integrations/google-calendar/callback");
  fireEvent.change(screen.getByLabelText("Google Client Secret"), { target: { value: "fixture-secret" } });
  fireEvent.click(screen.getByRole("button", { name: "Save Google setup" }));
  await waitFor(() => expect(api).toHaveBeenCalledWith(base + "/setup", expect.objectContaining({ method: "PUT", body: expect.stringContaining("fixture-secret") })));
});

it("connects Google Drive with a separate metadata-only setup", async () => {
  const drive = "/api/integrations/google-drive";
  vi.mocked(api).mockImplementation(async (path) => {
    if (path === "/api/agents") return { agents: [{ id: "main", name: "Main Agent" }] } as never;
    if (path === drive + "/setup") return { client_id: "drive-client", redirect_uri: "", configured: true, secret_configured: true } as never;
    if (path === drive + "/connect") return { authorization_url: "https://accounts.google.com/o/oauth2/v2/auth?state=drive" } as never;
    if (path === drive) return disconnected as never;
    throw new Error(`Unexpected route: ${path}`);
  });
  const popup = vi.spyOn(window, "open").mockReturnValue(null);
  render(<GoogleDriveIntegration />);
  fireEvent.click(screen.getByRole("button", { name: "Google Drive integration" }));
  expect(await screen.findByText(/Read file lists and metadata/)).toBeInTheDocument();
  fireEvent.click(await screen.findByRole("button", { name: "Connect Google Drive" }));
  expect(await screen.findByRole("link", { name: "Open Google sign-in" })).toHaveAttribute("href", "https://accounts.google.com/o/oauth2/v2/auth?state=drive");
  expect(api).toHaveBeenCalledWith(drive + "/connect", { method: "POST" });
  popup.mockRestore();
});
