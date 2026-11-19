import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "../api/client";
import { McpSettings } from "./McpSettings";

vi.mock("../api/client", () => ({
  api: vi.fn(async (path: string, opts?: RequestInit) => {
    if (path === "/api/mcp" && (opts?.method || "GET") === "GET") {
      return {
        servers: [{ name: "filesystem", origin: "standalone", plugin: "", enabled: true, editable: true, transport: "stdio", command: "npx", args: ["-y", "demo"] }],
      };
    }
    if (path === "/api/mcp" && opts?.method === "POST") {
      const body = JSON.parse(String(opts.body || "{}")) as { name?: string; json?: string };
      if (body.json) {
        return { servers: [{ name: "imported", origin: "standalone", plugin: "", enabled: true, editable: true, transport: "stdio", command: "npx" }] };
      }
      return {
        servers: [
          { name: "filesystem", origin: "standalone", plugin: "", enabled: true, editable: true, transport: "stdio", command: "npx", args: ["-y", "demo"] },
          { name: body.name || "new", origin: "standalone", plugin: "", enabled: true, editable: true, transport: "stdio", command: "npx" },
        ],
      };
    }
    if (path === "/api/mcp/test") {
      return { servers: [{ ok: true, tools: 3 }] };
    }
    return { ok: true };
  }),
}));

describe("McpSettings", () => {
  afterEach(() => {
    cleanup();
  });

  it("lists saved servers and can add another", async () => {
    render(<McpSettings />);
    expect(await screen.findByText("filesystem")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("MCP server name"), { target: { value: "docs" } });
    fireEvent.change(screen.getByLabelText("MCP command"), {
      target: { value: "npx -y @modelcontextprotocol/server-filesystem /tmp" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/mcp",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({
            name: "docs",
            transport: "stdio",
            command: "npx",
            args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
            url: "",
          }),
        }),
      ),
    );
    expect(await screen.findByText("docs")).toBeInTheDocument();
  });

  it("tests a remote HTTP server", async () => {
    render(<McpSettings />);
    await screen.findByText("filesystem");
    fireEvent.click(screen.getByRole("button", { name: /HTTP/ }));
    fireEvent.change(screen.getByLabelText("MCP server name"), { target: { value: "prime" } });
    fireEvent.change(screen.getByLabelText("MCP server URL"), {
      target: { value: "https://api.example.com/mcp" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Test" }));
    await waitFor(() =>
      expect(vi.mocked(api)).toHaveBeenCalledWith(
        "/api/mcp/test",
        expect.objectContaining({ method: "POST" }),
      ),
    );
    expect(await screen.findByText(/Connection works/)).toBeInTheDocument();
  });

  it("configures a trusted protected mailbox and clears its environment after saving", async () => {
    render(<McpSettings />);
    await screen.findByText("filesystem");
    fireEvent.change(screen.getByLabelText("MCP server name"), { target: { value: "outlook" } });
    fireEvent.change(screen.getByLabelText("MCP command"), { target: { value: "node /tmp/adapter.mjs" } });
    fireEvent.change(screen.getByLabelText("Protected mailbox tool"), { target: { value: "verification_mail" } });
    fireEvent.change(screen.getByLabelText("MCP environment JSON"), { target: { value: '{"TOKEN":"fixture"}' } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(vi.mocked(api)).toHaveBeenCalledWith("/api/mcp", expect.objectContaining({
      body: JSON.stringify({name:"outlook",transport:"stdio",command:"node",args:["/tmp/adapter.mjs"],url:"",
        verification_mail_tool:"verification_mail",read_only_tools:["verification_mail"],env:{TOKEN:"fixture"}}),
    })));
    expect(screen.getByLabelText("MCP environment JSON")).toHaveValue("");
  });

  it("hands off OAuth to the user before enabling a plugin connection", async () => {
    const server = { name: "plugin-github-github", origin: "plugin", plugin: "github", enabled: false, editable: true, transport: "http", url: "https://example.test/mcp", oauth: { client_id: "fixture" } };
    vi.mocked(api).mockClear();
    vi.mocked(api).mockImplementation(async (path, opts) => {
      if (path === "/api/mcp/oauth/start") return { status: "authorization_required", authorization_url: "https://auth.example.test/authorize?state=fixture" } as never;
      if (path === "/api/mcp/oauth/status") return { status: "ready", pending: false } as never;
      if (path === "/api/mcp" && opts?.method === "POST") return { servers: [{ ...server, enabled: true }] } as never;
      if (path === "/api/mcp") return { servers: [server] } as never;
      return { ok: true } as never;
    });
    const popup = vi.spyOn(window, "open").mockReturnValue(null);
    render(<McpSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Connect plugin-github-github" }));
    expect(await screen.findByRole("link", { name: "Open authorization" })).toHaveAttribute("href", "https://auth.example.test/authorize?state=fixture");
    expect(api).not.toHaveBeenCalledWith("/api/mcp", expect.objectContaining({ method: "POST" }));
    await waitFor(() => expect(api).toHaveBeenCalledWith("/api/mcp", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: server.name, enabled: true }) })), { timeout: 5000 });
    popup.mockRestore();
  });

  it("offers both anonymous enablement and OAuth for plugin HTTP servers without a client", async () => {
    const server = { name: "plugin-example-api", origin: "plugin", plugin: "example", enabled: false, editable: true, transport: "http", url: "https://example.test/mcp" };
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === "/api/mcp/oauth/start") return { status: "authorization_required", authorization_url: "https://auth.example.test/authorize" } as never;
      if (path === "/api/mcp") return { servers: [server] } as never;
      return { ok: true } as never;
    });
    const popup = vi.spyOn(window, "open").mockReturnValue(null);
    render(<McpSettings />);
    expect(await screen.findByRole("button", { name: "Enable plugin-example-api" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Connect plugin-example-api" }));
    expect(await screen.findByRole("link", { name: "Open authorization" })).toHaveAttribute("href", "https://auth.example.test/authorize");
    expect(api).toHaveBeenCalledWith("/api/mcp/oauth/start", expect.objectContaining({ method: "POST", body: JSON.stringify({ name: server.name }) }));
    popup.mockRestore();
  });
});
