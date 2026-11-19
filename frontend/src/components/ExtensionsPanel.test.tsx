import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { ExtensionsPanel } from "./ExtensionsPanel";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../api/client", () => ({ api: mocks.api }));
vi.mock("../store", () => ({ useAppStore: (selector: (state: { sessionId: null }) => unknown) => selector({ sessionId: null }) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

function setup() {
  mocks.api.mockImplementation(async (path: string) => {
    if (path === "/api/plugins") return { plugins: [{
      name: "demo", status: "installed", unsupported: ["Example unsupported component"],
      skills: [{ name: "demo:review", description: "Review guidance" }],
      mcp: [{ name: "demo:docs", transport: "stdio", enabled: false }], hooks: [],
    }] };
    if (path === "/api/extensions/skills") return { enabled: true, skills: [
      { name: "demo:review", origin: "plugin", plugin: "demo", plugin_enabled: false, can_pin: false, editable: false },
      { name: "personal", origin: "standalone", plugin: "", can_pin: true, editable: true },
      { name: "built-in", origin: "builtin", plugin: "", can_pin: false, editable: false },
    ] };
    if (path === "/api/mcp") return { servers: [
      { name: "demo:docs", origin: "plugin", plugin: "demo", enabled: false, editable: false, transport: "stdio" },
      { name: "personal", origin: "standalone", plugin: "", enabled: true, editable: true, transport: "http" },
    ] };
    if (path === "/api/extensions/mods") return { mods: [{ mod: "demo:mod.py", plugin: "demo", enabled: false, callbacks: ["render_ui"] }] };
    if (path === "/api/hooks") return { hooks: [], events: ["preToolUse"] };
    throw new Error(`Unexpected path: ${path}`);
  });
}

it("shows skill ownership and opens the owning package", async () => {
  setup(); render(<ExtensionsPanel />);
  fireEvent.click(screen.getByRole("tab", { name: "Skills" }));
  const owner = await screen.findByRole("button", { name: "Plugin: demo · Disabled" });
  expect(screen.getByText("Built-in")).toBeInTheDocument();
  expect(screen.getByText("Standalone")).toBeInTheDocument();
  expect(screen.getAllByRole("button", { name: "Pin for this workspace" })).toHaveLength(1);
  fireEvent.click(owner);
  expect(await screen.findByRole("textbox", { name: "Plugin name" })).toHaveValue("demo");
  expect(screen.getByRole("tab", { name: "Plugins" })).toHaveAttribute("aria-selected", "true");
  const detail = await screen.findByRole("region", { name: "Plugin details: demo" });
  expect(within(detail).getByText(/demo:review/)).toBeInTheDocument();
  expect(within(detail).getByText(/demo:docs/)).toBeInTheDocument();
  expect(within(detail).getByText("Unsupported: Example unsupported component")).toBeInTheDocument();
});

it("keeps package connections read-only while standalone connections remain editable", async () => {
  setup(); render(<ExtensionsPanel />);
  fireEvent.click(screen.getByRole("tab", { name: "Connections" }));
  const owner = await screen.findByRole("button", { name: "Plugin: demo" });
  expect(screen.getAllByRole("button", { name: "Remove" })).toHaveLength(1);
  expect(screen.getAllByRole("button", { name: "Test connection" }).filter((button) => button.hasAttribute("disabled"))).toHaveLength(1);
  expect(screen.getByText(/Configuration does not imply a live connection/)).toBeInTheDocument();
  fireEvent.click(owner);
  expect(await screen.findByRole("textbox", { name: "Plugin name" })).toHaveValue("demo");
  expect(await screen.findByRole("region", { name: "Plugin details: demo" })).toBeInTheDocument();
});

it("shows disabled mods and links to their package", async () => {
  setup(); render(<ExtensionsPanel />);
  fireEvent.click(screen.getByRole("tab", { name: "Hooks & Mods" }));
  expect(await screen.findByText("demo:mod.py · Disabled")).toBeInTheDocument();
  expect(screen.getByText("render_ui")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Plugin: demo" }));
  expect(await screen.findByRole("textbox", { name: "Plugin name" })).toHaveValue("demo");
  expect(await screen.findByRole("region", { name: "Plugin details: demo" })).toBeInTheDocument();
});

it("supports keyboard navigation between extension views", async () => {
  setup(); render(<ExtensionsPanel />);
  const plugins = screen.getByRole("tab", { name: "Plugins" });
  plugins.focus();
  fireEvent.keyDown(plugins, { key: "ArrowRight" });
  const skills = screen.getByRole("tab", { name: "Skills" });
  expect(skills).toHaveFocus();
  expect(skills).toHaveAttribute("aria-selected", "true");
  await screen.findByText("demo:review");
  fireEvent.keyDown(skills, { key: "End" });
  expect(screen.getByRole("tab", { name: "Hooks & Mods" })).toHaveFocus();
  await screen.findByText("demo:mod.py · Disabled");
});
