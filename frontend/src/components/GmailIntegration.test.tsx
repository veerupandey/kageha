import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { GmailIntegration } from "./GmailIntegration";
import { api } from "../api/client";

vi.mock("../api/client", () => ({ api: vi.fn() }));
const base = "/api/integrations/gmail";
const empty = { connected: false, configured: false, email: "", agents: [] as string[], pending: false, authorization_error: "", redirect_uri: "" };
let connection = { ...empty };
beforeEach(() => {
  connection = { ...empty };
  vi.mocked(api).mockReset();
  vi.mocked(api).mockImplementation(async (path, opts) => {
    if (path === "/api/agents") return { agents: [{ id: "main", name: "Main Agent" }] } as never;
    if (path === base && opts?.method === "DELETE") return empty as never;
    if (path === base) return connection as never;
    if (path === base + "/setup") return { client_id: "", secret_configured: false, redirect_uri: "", source: "environment", configured: false } as never;
    if (path === base + "/access") {
      connection = { ...connection, agents: JSON.parse(opts?.body as string).agents };
      return connection as never;
    }
    if (path === base + "/connect") return { authorization_url: "https://accounts.google.com/o/oauth2/v2/auth?state=test" } as never;
    if (path === base + "/cancel") return connection as never;
    throw new Error("Unexpected route");
  });
});

describe("Gmail integration", () => {
  it("shows setup guidance when Google OAuth is not configured", async () => {
    render(<GmailIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^Gmail / }));
    expect(await screen.findByText("Google sign-in needs setup")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Connect with Google" })).not.toBeInTheDocument();
  });

  it("provides a fallback sign-in link when popups are blocked and allows cancellation", async () => {
    connection = { ...empty, configured: true };
    const popup = vi.spyOn(window, "open").mockReturnValue(null);
    render(<GmailIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^Gmail / }));
    fireEvent.click(await screen.findByRole("button", { name: "Connect with Google" }));
    expect(await screen.findByRole("link", { name: "Open Google sign-in" })).toHaveAttribute("href", "https://accounts.google.com/o/oauth2/v2/auth?state=test");
    fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(screen.queryByText("Finish connecting on Google")).not.toBeInTheDocument());
    expect(api).toHaveBeenCalledWith(base + "/cancel", { method: "POST" });
    popup.mockRestore();
  });

  it("preserves automatic main access and disconnects", async () => {
    connection = { ...empty, connected: true, configured: true, email: "reader@example.com", agents: ["main"] };
    render(<GmailIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^Gmail / }));
    expect(await screen.findByLabelText("Main Agent")).toBeChecked();
    expect(screen.getByLabelText("Main Agent")).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Save Gmail access" }));
    await screen.findByText(/Gmail access saved/);
    expect(api).toHaveBeenCalledWith(base + "/access", { method: "PUT", body: JSON.stringify({ agents: ["main"], expected_agents: ["main"] }) });
    fireEvent.click(screen.getByRole("button", { name: "Disconnect Gmail" }));
    await screen.findByText(/Gmail disconnected from Kageha/);
    expect(screen.queryByText("reader@example.com")).not.toBeInTheDocument();
  });

  it("opens a connected legacy account without an assignment list", async () => {
    connection = { ...empty, connected: true, configured: true, email: "reader@example.com" };
    Reflect.deleteProperty(connection, "agents");
    render(<GmailIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^Gmail / }));
    expect(await screen.findByText(/0 agents assigned/)).toBeVisible();
    expect(await screen.findByLabelText("Main Agent")).not.toBeChecked();
  });

  it("shows connection errors without claiming success", async () => {
    vi.mocked(api).mockRejectedValue(new Error("Unavailable"));
    render(<GmailIntegration />);
    fireEvent.click(screen.getByRole("button", { name: /^Gmail / }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Unavailable");
  });
});
