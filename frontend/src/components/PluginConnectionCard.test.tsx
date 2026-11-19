import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { PluginConnectionCard } from "./PluginConnectionCard";
import { api } from "../api/client";
vi.mock("../api/client", () => ({ api: vi.fn() }));
const ready = { id: "calendar:google", status: "ready", fields: [], oauth: true, requires_client: true, client_configured: true, callback_uri: "https://kageha.example/api/plugin-connections/callback", tools: ["list_events"], activation_required: false };
afterEach(() => { cleanup(); vi.resetAllMocks(); vi.useRealTimers(); });
describe("shared plugin setup", () => {
  it("requires trust and sends masked secrets only to configuration", async () => {
    vi.mocked(api).mockResolvedValue({ ...ready, status: "needs_setup", oauth: false, requires_client: false, activation_required: true, fields: [{ key: "API_KEY", title: "API key", description: "Service key", type: "string", sensitive: true, required: true }] });
    render(<PluginConnectionCard id={ready.id} />);
    const connect = await screen.findByRole("button", { name: "Set up and verify" });
    expect(connect).toBeDisabled();
    const input = screen.getByLabelText(/API key/);
    expect(input).toHaveAttribute("type", "password");
    fireEvent.change(input, { target: { value: "private-input" } });
    fireEvent.click(screen.getByRole("checkbox", { name: /trust this plugin/ }));
    fireEvent.click(connect);
    await waitFor(() => expect(api).toHaveBeenCalledWith(expect.stringContaining("/configure"), expect.objectContaining({ method: "POST", body: expect.stringContaining('"execution_approved":true') })));
    expect(api).toHaveBeenCalledWith(expect.stringContaining("/configure"), expect.objectContaining({ body: expect.stringContaining("private-input") }));
    expect(api).toHaveBeenCalledWith(expect.stringContaining("/connect"), { method: "POST" });
  });
  it("shows exact callback prerequisites and read-first Calendar permissions", async () => {
    vi.mocked(api).mockResolvedValue({ ...ready, status: "administrator_setup_required", client_configured: false, adapter: "Kageha Calendar API", write_access: false });
    render(<PluginConnectionCard id={ready.id} />);
    expect(await screen.findByRole("button", { name: "Connect account" })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: "Allow scheduling and changing events" })).not.toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "Application setup" }));
    expect(screen.getByText(ready.callback_uri)).toBeInTheDocument();
    expect(screen.getByLabelText("Client secret")).toHaveAttribute("type", "password");
  });
  it("resumes only when polling confirms Ready", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(api).mockResolvedValue({ ...ready, status: "connecting", authorization_url: "https://provider.example/authorize" });
    const resumed = vi.fn();
    render(<PluginConnectionCard id={ready.id} onReady={resumed} />);
    expect(await screen.findByRole("link", { name: "Continue sign-in" })).toHaveAttribute("rel", "noopener noreferrer");
    expect(resumed).not.toHaveBeenCalled();
    vi.mocked(api).mockResolvedValue(ready);
    await waitFor(() => expect(resumed).toHaveBeenCalledTimes(1), { timeout: 3000 });
  });
  it("disconnects through the lifecycle endpoint and permits deferral", async () => {
    vi.mocked(api).mockResolvedValue(ready);
    const cancel = vi.fn();
    render(<PluginConnectionCard id={ready.id} onCancel={cancel} />);
    fireEvent.click(await screen.findByRole("button", { name: "Disconnect" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith(expect.stringContaining("/disconnect"), { method: "POST" }));
    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(cancel).toHaveBeenCalledTimes(1);
  });
});
