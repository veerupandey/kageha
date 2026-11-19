import "@testing-library/jest-dom/vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { IntegrationsSettings } from "./IntegrationsSettings";
import { api } from "../api/client";
vi.mock("../api/client", () => ({ api: vi.fn() }));
vi.mock("./GmailIntegration", () => ({ GmailIntegration: () => null }));
vi.mock("./LinkedInIntegration", () => ({ LinkedInIntegration: () => null }));
vi.mock("./SocialIntegration", () => ({ SocialIntegration: () => null }));
vi.mock("./ShopifyIntegration", () => ({ ShopifyIntegration: () => null }));
const base = "/api/integrations/github";
const disconnected = { connected: false, login: "", repositories: [], agents: [], browser_auth_available: false };
let connection = { ...disconnected };
beforeEach(() => {
  connection = { ...disconnected };
  vi.mocked(api).mockReset();
  vi.mocked(api).mockImplementation(async (path, opts) => {
    if (path === "/api/agents") return { agents: [{ id: "main", name: "Main Agent" }] } as never;
    if (path === base && opts?.method === "DELETE") return disconnected as never;
    if (path === base) return connection as never;
    if (path === base + "/connect") return { ...connection, connected: true, login: "octocat" } as never;
    if (path.startsWith(base + "/repositories")) return { repositories: [{ full_name: "octocat/demo", private: true }], has_more: false } as never;
    if (path === base + "/access") return { ...connection, connected: true, login: "octocat", ...JSON.parse(opts?.body as string) } as never;
    throw new Error("Unexpected route: " + path);
  });
});
describe("GitHub integrations", () => {
  it("makes GitHub sign-in primary and keeps token connection in a disclosure", async () => {
    connection = { ...disconnected, browser_auth_available: true };
    render(<IntegrationsSettings />);
    fireEvent.click(screen.getByRole("button", { name: /^GitHub / }));
    expect(await screen.findByRole("button", { name: "Connect with GitHub" })).toBeEnabled();
    expect(screen.queryByLabelText("Fine-grained personal access token")).not.toBeInTheDocument();
    const alternative = screen.getByRole("button", { name: "Connect another way" });
    fireEvent.click(alternative);
    expect(alternative).toHaveAttribute("aria-expanded", "true");
    fireEvent.change(screen.getByLabelText("Fine-grained personal access token"), { target: { value: "secret" } });
    fireEvent.click(alternative);
    fireEvent.click(alternative);
    expect(screen.getByLabelText("Fine-grained personal access token")).toHaveValue("");
  });
  it("connects, clears the secret, and saves repository and agent access", async () => {
    render(<IntegrationsSettings />);
    fireEvent.click(screen.getByRole("button", { name: /^GitHub / }));
    const token = await screen.findByLabelText("Fine-grained personal access token");
    fireEvent.change(token, { target: { value: "github_pat_secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Connect token" }));
    await screen.findByText("octocat");
    expect(screen.queryByDisplayValue("github_pat_secret")).not.toBeInTheDocument();
    fireEvent.click(await screen.findByLabelText("octocat/demoPrivate"));
    fireEvent.click(screen.getByLabelText("Main Agent"));
    fireEvent.click(screen.getByRole("button", { name: "Save access" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith(base + "/access", { method: "PUT", body: JSON.stringify({ repositories: ["octocat/demo"], agents: ["main"] }) }));
    expect(await screen.findByText(/Access saved/)).toBeInTheDocument();
  });
  it("loads repositories only when the connected GitHub section is first opened", async () => {
    connection = { ...disconnected, connected: true, login: "octocat" };
    render(<IntegrationsSettings />);
    await screen.findByText("Connected");
    const calls = () => vi.mocked(api).mock.calls.filter(([path]) => path.startsWith(base + "/repositories"));
    expect(calls()).toHaveLength(0);

    const section = screen.getByRole("button", { name: "GitHub integration" });
    fireEvent.click(section);
    await screen.findByLabelText("octocat/demoPrivate");
    fireEvent.click(section);
    fireEvent.click(section);
    expect(calls()).toHaveLength(1);
  });
  it("disconnects and removes access controls", async () => {
    connection = { ...disconnected, connected: true, login: "octocat" };
    render(<IntegrationsSettings />);
    fireEvent.click(screen.getByRole("button", { name: /^GitHub / }));
    fireEvent.click(await screen.findByRole("button", { name: "Disconnect" }));
    await screen.findByText(/Disconnected from Kageha/);
    expect(screen.queryByRole("button", { name: "Save access" })).not.toBeInTheDocument();
  });
  it("displays server errors without claiming connection success", async () => {
    vi.mocked(api).mockRejectedValue(new Error("GitHub unavailable"));
    render(<IntegrationsSettings />);
    fireEvent.click(screen.getByRole("button", { name: /^GitHub / }));
    expect(await screen.findByRole("alert")).toHaveTextContent("GitHub unavailable");
    expect(screen.queryByText("Connected")).not.toBeInTheDocument();
  });
});
