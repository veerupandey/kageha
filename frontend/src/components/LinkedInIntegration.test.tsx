import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { LinkedInIntegration } from "./LinkedInIntegration";
import { api } from "../api/client";

vi.mock("../api/client", () => ({ api: vi.fn() }));
const base = "/api/integrations/linkedin";
const empty = { connected: false, configured: false, name: "", email: "", write_agents: [] as string[], can_publish: false, needs_reconnect: false, agents: [] as string[], pending: false, authorization_error: "", redirect_uri: "" };
let connection = { ...empty };
beforeEach(() => {
  connection = { ...empty };
  vi.mocked(api).mockReset();
  vi.mocked(api).mockImplementation(async (path, opts) => {
    if (path === "/api/agents") return { agents: [{ id: "main", name: "Main Agent" }] } as never;
    if (path === base && opts?.method === "DELETE") return empty as never;
    if (path === base) return connection as never;
    if (path === base + "/actions") return { actions: [] } as never;
    if (path === base + "/setup") return { client_id: "", secret_configured: false, redirect_uri: "", source: "environment", configured: false, publish: false, api_version: "202609" } as never;
    if (path === base + "/access") return { ...connection, ...JSON.parse(opts?.body as string) } as never;
    if (path === base + "/page") return { ...connection, page_url: "https://www.linkedin.com/company/143940845/admin/" } as never;
    if (path === base + "/connect") return { authorization_url: "https://accounts.google.com/o/oauth2/v2/auth?state=test" } as never;
    if (path === base + "/cancel") return connection as never;
    throw new Error("Unexpected route");
  });
});

describe("LinkedIn integration", () => {
  it("saves the existing page and shows its admin link", async () => {
    connection = { ...empty, connected: true, configured: true };
    render(<LinkedInIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^LinkedIn / }));
    fireEvent.change(await screen.findByLabelText("Existing LinkedIn page"), { target: { value: "https://www.linkedin.com/company/143940845/" } });
    fireEvent.click(screen.getByRole("button", { name: "Save LinkedIn page" }));
    expect(await screen.findByRole("link", { name: /Open page admin/ })).toHaveAttribute("href", "https://www.linkedin.com/company/143940845/admin/");
    expect(api).toHaveBeenCalledWith(base + "/page", { method: "PUT", body: JSON.stringify({ page_url: "https://www.linkedin.com/company/143940845/" }) });
  });
  it("shows setup guidance when LinkedIn OAuth is not configured", async () => {
    render(<LinkedInIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^LinkedIn / }));
    expect(await screen.findByText("LinkedIn sign-in needs setup")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Connect with LinkedIn" })).not.toBeInTheDocument();
  });

  it("provides a fallback sign-in link when popups are blocked and allows cancellation", async () => {
    connection = { ...empty, configured: true };
    const popup = vi.spyOn(window, "open").mockReturnValue(null);
    render(<LinkedInIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^LinkedIn / }));
    fireEvent.click(await screen.findByRole("button", { name: "Connect with LinkedIn" }));
    expect(await screen.findByRole("link", { name: "Open LinkedIn sign-in" })).toHaveAttribute("href", "https://accounts.google.com/o/oauth2/v2/auth?state=test");
    fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(screen.queryByText("Finish connecting on LinkedIn")).not.toBeInTheDocument());
    expect(api).toHaveBeenCalledWith(base + "/cancel", { method: "POST" });
    popup.mockRestore();
  });

  it("saves agent assignments and disconnects", async () => {
    connection = { ...empty, connected: true, configured: true, email: "reader@example.com" };
    render(<LinkedInIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^LinkedIn / }));
    fireEvent.click(await screen.findByLabelText("Main Agent"));
    fireEvent.click(screen.getByRole("button", { name: "Save LinkedIn access" }));
    await screen.findByText(/LinkedIn access saved/);
    expect(api).toHaveBeenCalledWith(base + "/access", { method: "PUT", body: JSON.stringify({ agents: ["main"], write_agents: [] }) });
    fireEvent.click(screen.getByRole("button", { name: "Disconnect LinkedIn" }));
    await screen.findByText(/LinkedIn disconnected from Kageha/);
    expect(screen.queryByText("reader@example.com")).not.toBeInTheDocument();
  });

  it("shows connection errors without claiming success", async () => {
    vi.mocked(api).mockRejectedValue(new Error("Unavailable"));
    render(<LinkedInIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^LinkedIn / }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Unavailable");
  });
  it("marks expired access for reconnect and keeps disconnect available", async () => {
    connection = { ...empty, configured: true, needs_reconnect: true };
    render(<LinkedInIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^LinkedIn integration/ }));
    expect(await screen.findByText(/access expired or was revoked/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect with LinkedIn" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Disconnect LinkedIn" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Check LinkedIn connection" })).toBeDisabled();
  });

  it("publishes only after the owner approves a prepared draft", async () => {
    connection = { ...empty, configured: true, connected: true, can_publish: true };
    let published = false;
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (path, options) => {
      if (path === base + "/actions") return { actions: [{ id: "draft_123", agent: "main", text: "My draft", status: published ? "done" : "review" }] } as never;
      if (path === base + "/actions/draft_123") { published = true; return {} as never; }
      return original(path, options);
    });
    render(<LinkedInIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^LinkedIn integration/ }));
    expect(await screen.findByText("My draft")).toBeInTheDocument();
    expect(published).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Approve and publish" }));
    await screen.findByText("LinkedIn post published.");
    expect(api).toHaveBeenCalledWith(base + "/actions/draft_123", { method: "POST" });
    expect(screen.queryByRole("button", { name: "Approve and publish" })).not.toBeInTheDocument();
  });

});
