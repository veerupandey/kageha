import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { PluginsPanel } from "./PluginsPanel";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../../api/client", () => ({ api: mocks.api }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
const preview = {
  preview_id: "preview-1", name: "demo", source: "https://example.test/demo", commit: "abcd", package_sha256: "1234",
  skills: [{ name: "demo-skill", sha256: "5678" }], mcp: [{ name: "demo:server", transport: "http", endpoint: "https://example.test/mcp", enabled: false }],
  unsupported: ["Managed app connectors are unsupported"], installable: true,
};

it("reviews supported app connections, specialists and commands", async () => {
  mocks.api.mockImplementation(async (path: string) => path === "/api/plugins/preview"
    ? { ...preview, apps: [{ name: "calendar", integration: "google-calendar", capabilities: ["read"] }],
        agents: [{ id: "plugin.demo.reviewer", name: "demo: reviewer" }],
        commands: [{ name: "/plugin-demo-review", description: "Review a document" }] }
    : { plugins: [] });
  render(<PluginsPanel />);
  fireEvent.change(screen.getByLabelText("Plugin name"), { target: { value: "demo" } });
  fireEvent.click(screen.getByRole("button", { name: "Review plugin" }));
  expect(await screen.findByText(/calendar · google-calendar · read · account setup required/)).toBeInTheDocument();
  expect(screen.getByText(/demo: reviewer · plugin.demo.reviewer/)).toBeInTheDocument();
  expect(screen.getByText(/\/plugin-demo-review · Review a document/)).toBeInTheDocument();
});

it("shows disabled hook details and only enables them after the user acts", async () => {
  let enabled = false;
  const hook = { event: "PreToolUse", matcher: "shell", command: "echo fixture" };
  mocks.api.mockImplementation(async (path: string, options?: { body: string }) => {
    if (path === "/api/plugins/hooks") { expect(JSON.parse(options!.body)).toEqual({ name: "demo", enabled: true }); enabled = true; return { ok: true }; }
    return { plugins: [{ name: "demo", status: "installed", hooks: [hook], hooks_enabled: enabled }] };
  });
  render(<PluginsPanel />);
  expect(await screen.findByText("Hook: PreToolUse · shell · disabled")).toBeInTheDocument();
  expect(screen.getByText("echo fixture")).toBeInTheDocument();
  expect(mocks.api).not.toHaveBeenCalledWith("/api/plugins/hooks", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "Enable demo hooks" }));
  expect(await screen.findByText("Hook: PreToolUse · shell · enabled")).toBeInTheDocument();
});

it("shows pinned hook resource contents before activation", async () => {
  const hook = { event: "PreToolUse", matcher: "^write_.*$", matcher_type: "regex", status_message: "Checking files", command: "node ${PLUGIN_ROOT}/scripts/check.mjs", resources: { "scripts/check.mjs": "console.log('reviewed script')" } };
  mocks.api.mockResolvedValue({ plugins: [{ name: "demo", status: "installed", hooks: [hook], hooks_enabled: false }] });
  render(<PluginsPanel />);
  expect(await screen.findByText("Review hook resource: scripts/check.mjs")).toBeInTheDocument();
  expect(screen.getByText("Matcher type: regular expression")).toBeInTheDocument();
  expect(screen.getByText("Status: Checking files")).toBeInTheDocument();
  fireEvent.click(screen.getByText("Review hook resource: scripts/check.mjs"));
  expect(screen.getByText("console.log('reviewed script')")).toBeVisible();
  expect(mocks.api).not.toHaveBeenCalledWith("/api/plugins/hooks", expect.anything());
});

it("binds an app to explicitly selected MCP tools and the reviewed revision", async () => {
  let bridge: Record<string, unknown> | undefined;
  mocks.api.mockImplementation(async (path: string, options?: { method?: string; body?: string }) => {
    if (path === "/api/mcp") return { servers: [{ name: "work-mcp", enabled: true, revision: 7, tools: [{ name: "search", description: "Search work" }, { name: "write", description: "Write work" }] }] };
    if (path === "/api/plugins/app-bridge") { bridge = JSON.parse(options!.body!); return { ok: true }; }
    return { plugins: [{ name: "demo", status: "installed", package_sha256: "package-hash", apps: [{ name: "work", connector_id: "connector-id", integration: null, capabilities: [], bridge: bridge ? { server: "work-mcp", status: "configured", tools: ["search"] } : { status: "needs_setup" } }] }] };
  });
  render(<PluginsPanel />);
  fireEvent.click(await screen.findByRole("button", { name: "Configure work connection" }));
  fireEvent.change(await screen.findByLabelText("Connection for work"), { target: { value: "work-mcp" } });
  fireEvent.click(screen.getByRole("checkbox", { name: /search/ }));
  fireEvent.click(screen.getByRole("button", { name: "Save work connection" }));
  await waitFor(() => expect(bridge).toEqual({ plugin: "demo", app: "work", connector_id: "connector-id", package_sha256: "package-hash", server: "work-mcp", expected_server_revision: 7, tools: ["search"] }));
  expect(await screen.findByText(/MCP work-mcp · configured/)).toBeInTheDocument();
});

it("requires reviewing the exact package before installation", async () => {
  let installed = false;
  mocks.api.mockImplementation(async (path: string, options?: { body: string }) => {
    if (path === "/api/plugins/preview") {
      expect(JSON.parse(options!.body)).toEqual({ name: "demo" });
      return preview;
    }
    if (path === "/api/plugins/install") {
      expect(JSON.parse(options!.body)).toEqual({ preview_id: "preview-1", approved_summary: preview });
      installed = true;
      return { name: "demo" };
    }
    return { plugins: installed ? [{ name: "demo", status: "installed", skills: [{ name: "demo-skill" }], mcp: [{ name: "demo:server", transport: "http", enabled: false }], unsupported: preview.unsupported }] : [] };
  });
  render(<PluginsPanel />);
  await screen.findByText("No plugins installed.");
  fireEvent.change(screen.getByLabelText("Plugin name"), { target: { value: "demo" } });
  fireEvent.click(screen.getByRole("button", { name: "Review plugin" }));
  expect(await screen.findByText("Managed app connectors are unsupported")).toBeInTheDocument();
  expect(screen.getByText(/Package SHA-256/)).toHaveTextContent("1234");
  expect(screen.getByText(/demo:server · http/)).toHaveTextContent("requires setup");
  expect(mocks.api).not.toHaveBeenCalledWith("/api/plugins/install", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "Install reviewed package" }));
  expect(await screen.findByText("demo installed. Review its component status below.")).toBeInTheDocument();
  expect(screen.getByText("demo · installed")).toBeInTheDocument();
  expect(screen.getByText("Unsupported: Managed app connectors are unsupported")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Enable" })).not.toBeInTheDocument();
});

it("does not install unsupported packages and allows cancellation", async () => {
  mocks.api.mockImplementation(async (path: string) => path === "/api/plugins/preview"
    ? { ...preview, installable: false } : { plugins: [] });
  render(<PluginsPanel />);
  fireEvent.change(screen.getByLabelText("Plugin name"), { target: { value: "demo" } });
  fireEvent.click(screen.getByRole("button", { name: "Review plugin" }));
  expect(await screen.findByText("This package has no installable components.")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Install reviewed package" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("region", { name: "Review demo" })).not.toBeInTheDocument());
  expect(mocks.api).not.toHaveBeenCalledWith("/api/plugins/install", expect.anything());
});

it("opens a catalog entry for review without installing it", async () => {
  mocks.api.mockImplementation(async (path: string) => {
    if (path === "/api/plugins/catalog") return { status: "available", plugins: [{ name: "demo", description: "Useful tools" }] };
    if (path === "/api/plugins/preview") return preview;
    return { plugins: [] };
  });
  render(<PluginsPanel />);
  fireEvent.click(screen.getByRole("button", { name: "Browse plugin catalog" }));
  fireEvent.click(await screen.findByRole("button", { name: "Review demo" }));
  expect(await screen.findByRole("heading", { name: "Review demo" })).toBeInTheDocument();
  expect(mocks.api).not.toHaveBeenCalledWith("/api/plugins/install", expect.anything());
});

it("keeps the reviewed package visible if installation fails", async () => {
  mocks.api.mockImplementation(async (path: string) => {
    if (path === "/api/plugins/preview") return preview;
    if (path === "/api/plugins/install") throw new Error("Preview expired. Review again.");
    return { plugins: [] };
  });
  render(<PluginsPanel />);
  fireEvent.change(screen.getByLabelText("Plugin name"), { target: { value: "demo" } });
  fireEvent.click(screen.getByRole("button", { name: "Review plugin" }));
  fireEvent.click(await screen.findByRole("button", { name: "Install reviewed package" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Preview expired. Review again.");
  expect(screen.getByRole("region", { name: "Review demo" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Install reviewed package" })).toBeEnabled();
});

it("reviews the original preview before resuming an incomplete installation", async () => {
  let installed = false;
  mocks.api.mockImplementation(async (path: string, options?: { body: string }) => {
    if (path === "/api/plugins/preview?id=preview-1") return preview;
    if (path === "/api/plugins/install") {
      expect(JSON.parse(options!.body)).toEqual({ preview_id: "preview-1", approved_summary: preview });
      installed = true;
      return { ok: true };
    }
    return { plugins: [{ name: "demo", status: installed ? "installed" : "installing", preview_id: "preview-1", owner: "operator" }] };
  });
  render(<PluginsPanel />);
  expect(await screen.findByText("demo · installation interrupted or in progress")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Review and resume" }));
  expect(await screen.findByText(/This installation was interrupted/)).toBeInTheDocument();
  expect(mocks.api).not.toHaveBeenCalledWith("/api/plugins/install", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "Install reviewed package" }));
  expect(await screen.findByText("demo · installed")).toBeInTheDocument();
});

it("does not offer to resume a chat-owned installation from settings", async () => {
  mocks.api.mockResolvedValue({ plugins: [{ name: "demo", status: "installing", preview_id: "preview-1", owner: "another-chat" }] });
  render(<PluginsPanel />);
  expect(await screen.findByText("Return to the chat that started this installation to continue it.")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Review and resume" })).not.toBeInTheDocument();
});

it("does not treat a failed list refresh as a failed installation", async () => {
  let listCalls = 0;
  mocks.api.mockImplementation(async (path: string) => {
    if (path === "/api/plugins/preview") return preview;
    if (path === "/api/plugins/install") return { ok: true };
    if (++listCalls > 1) throw new Error("List unavailable");
    return { plugins: [] };
  });
  render(<PluginsPanel />);
  await screen.findByText("No plugins installed.");
  fireEvent.change(screen.getByLabelText("Plugin name"), { target: { value: "demo" } });
  fireEvent.click(screen.getByRole("button", { name: "Review plugin" }));
  fireEvent.click(await screen.findByRole("button", { name: "Install reviewed package" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("The package was installed, but the plugin list could not be refreshed.");
  expect(screen.getByRole("status")).toHaveTextContent("demo installed");
  expect(screen.queryByRole("button", { name: "Install reviewed package" })).not.toBeInTheDocument();
});

it("shows recorded MCP configuration without claiming live connectivity", async () => {
  mocks.api.mockResolvedValue({ plugins: [{ name: "demo", status: "installed", mcp: [
    { name: "working", transport: "http", configuration_status: "configured", enabled: true, recorded_connected: true },
    { name: "failed", transport: "http", configuration_status: "configured", enabled: true, recorded_connected: false },
    { name: "disabled", transport: "http", configuration_status: "configured", enabled: false, recorded_connected: false },
    { name: "missing", transport: "http", configuration_status: "removed", enabled: null, recorded_connected: null },
    { name: "other", transport: "http", configuration_status: "replaced", enabled: null, recorded_connected: null },
  ] }] });
  render(<PluginsPanel />);
  expect(await screen.findByText("MCP: working · enabled · last recorded connection succeeded")).toBeInTheDocument();
  expect(screen.getByText("MCP: failed · enabled · last recorded connection unavailable")).toBeInTheDocument();
  expect(screen.getByText("MCP: disabled · disabled · setup required")).toBeInTheDocument();
  expect(screen.getByText("MCP: missing · configuration removed")).toBeInTheDocument();
  expect(screen.getByText("MCP: other · configuration replaced")).toBeInTheDocument();
  expect(screen.queryByText("MCP: working · connected")).not.toBeInTheDocument();
});

it("shows the full disabled stdio command and each argument before installation", async () => {
  const longArg = `@vendor/${"package-name-".repeat(50)}server`;
  const stdio = { ...preview, mcp: [{ name: "demo-local", transport: "stdio", command: "npx", args: ["-y", longArg], enabled: false, setup_required: true }] };
  mocks.api.mockImplementation(async (path: string) => path === "/api/plugins/preview" ? stdio : { plugins: [] });
  render(<PluginsPanel />);
  fireEvent.change(screen.getByLabelText("Plugin name"), { target: { value: "demo" } });
  fireEvent.click(screen.getByRole("button", { name: "Review plugin" }));
  expect(await screen.findByText("demo-local · stdio · disabled, requires setup")).toBeInTheDocument();
  expect(screen.getByText("Command:")).toHaveTextContent("npx");
  expect(screen.getByRole("list", { name: "Arguments for demo-local" })).toHaveTextContent(longArg);
  expect(screen.getByText("This command is not run during installation.")).toBeInTheDocument();
  expect(mocks.api).not.toHaveBeenCalledWith("/api/plugins/install", expect.anything());
});
