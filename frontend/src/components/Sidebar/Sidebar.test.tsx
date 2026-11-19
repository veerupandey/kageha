import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from "../../store";
import { Sidebar } from "./Sidebar";

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("workspace navigation", () => {
  it.each([false, true])("keeps Extensions in Settings when collapsed=%s", (collapsed) => {
    vi.stubGlobal("localStorage", { getItem: () => null, setItem: vi.fn() });
    useAppStore.setState({
      sessions: [], personas: [], teams: [], projects: [],
      meta: { features: { jobs: true, extensions: true, attach: true } },
      personaEditor: null, loadProjects: vi.fn().mockResolvedValue(undefined),
    });
    render(<Sidebar open collapsed={collapsed} />);

    const workspace = screen.getByRole("region", { name: "Workspace" });
    fireEvent.click(within(workspace).getByRole("button", { name: /^Workspace/ }));
    expect(within(workspace).queryByRole("button", { name: "Extensions" })).not.toBeInTheDocument();
    expect(within(workspace).getByRole("button", { name: "Jobs" })).toBeInTheDocument();
    expect(within(workspace).getByRole("button", { name: "Attach" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Settings" })).toBeInTheDocument();
    if (collapsed) {
      const rail = screen.getByRole("complementary", { name: "Collapsed sidebar" });
      expect(within(rail).queryByTitle("Extensions")).not.toBeInTheDocument();
      expect(within(rail).getByTitle("Jobs")).toBeInTheDocument();
    }
  });
});

describe("minimizing the sidebar", () => {
  it.each([390, 768, 820, 1023, 1024, 1180, 1440])(
    "uses the visible navigation layout at %ipx",
    (width) => {
      vi.stubGlobal("matchMedia", () => ({ matches: width <= 1023 }));
      useAppStore.setState({
        sessions: [], personas: [], teams: [], projects: [], meta: null,
        personaEditor: null, loadProjects: vi.fn().mockResolvedValue(undefined),
      });
      const onClose = vi.fn();
      const onToggleCollapse = vi.fn();
      render(<Sidebar open collapsed={false} onClose={onClose} onToggleCollapse={onToggleCollapse} />);

      fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));

      expect(onClose).toHaveBeenCalledTimes(width <= 1023 ? 1 : 0);
      expect(onToggleCollapse).toHaveBeenCalledTimes(width <= 1023 ? 0 : 1);
    },
  );
});

describe("opening sidebar threads", () => {
  it("closes the drawer and leaves resource panels before session loading completes", () => {
    const openSession = vi.fn(() => new Promise<void>(() => {}));
    const onClose = vi.fn();
    const onAgentSelect = vi.fn();
    useAppStore.setState({
      sessions: [{ session_id: "saved-thread", title: "Saved conversation" }],
      sessionId: null, personas: [], teams: [], projects: [], meta: null,
      selectedProjectId: null, personaEditor: null, openSession,
      loadProjects: vi.fn().mockResolvedValue(undefined),
    });
    render(<Sidebar open collapsed={false} onClose={onClose} onAgentSelect={onAgentSelect} />);

    fireEvent.click(screen.getByRole("button", { name: /^Saved conversation/ }));

    expect(openSession).toHaveBeenCalledWith("saved-thread");
    expect(onClose).toHaveBeenCalledOnce();
    expect(onAgentSelect).toHaveBeenCalledWith("command-center");
  });
});
