import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ModelRoleControls } from "./ModelRoleControls";
import { useAppStore } from "../store";
import type { AgentPersona } from "../api/types";

function persona(id: string, name: string): AgentPersona {
  return { schema_version: 1, id, name };
}

/** Reset the persona/session slice of the store between tests to avoid pollution. */
function resetPersonaState() {
  useAppStore.setState({
    sessionId: null,
    personas: [],
    activePersonaId: "main",
  });
}

describe("ModelRoleControls persona picker (Requirement 2.5)", () => {
  afterEach(() => {
    cleanup();
    resetPersonaState();
  });

  it("is enabled when no session has started", () => {
    useAppStore.setState({
      sessionId: null,
      personas: [persona("main", "Main"), persona("brand", "Brand Expert")],
      activePersonaId: "main",
    });

    render(<ModelRoleControls />);

    const select = screen.getByTitle("Select the agent for this session");
    expect(select).not.toBeDisabled();
  });

  it("shows a bound-agent chip once a session has started", () => {
    useAppStore.setState({
      sessionId: "session-123",
      personas: [persona("main", "Main"), persona("brand", "Brand Expert")],
      activePersonaId: "main",
    });

    render(<ModelRoleControls />);

    expect(screen.getByTitle("This thread is bound to this agent")).toHaveTextContent("Main");
    expect(screen.queryByTitle("Select the agent for this session")).not.toBeInTheDocument();
  });

  it("renders one option per persona in the store, with matching text and value", () => {
    useAppStore.setState({
      sessionId: null,
      personas: [persona("main", "Main"), persona("brand", "Brand Expert")],
      activePersonaId: "main",
    });

    render(<ModelRoleControls />);

    const select = screen.getByTitle(
      "Select the agent for this session",
    ) as HTMLSelectElement;
    const options = Array.from(select.options);
    expect(options).toHaveLength(2);
    expect(options[0]).toHaveValue("main");
    expect(options[0]).toHaveTextContent("Main");
    expect(options[1]).toHaveValue("brand");
    expect(options[1]).toHaveTextContent("Brand Expert");
  });

  it("calls setActivePersonaId (reflected in store state) when a new option is selected while unlocked", () => {
    useAppStore.setState({
      sessionId: null,
      personas: [persona("main", "Main"), persona("brand", "Brand Expert")],
      activePersonaId: "main",
    });

    render(<ModelRoleControls />);

    const select = screen.getByTitle("Select the agent for this session");
    fireEvent.change(select, { target: { value: "brand" } });

    expect(useAppStore.getState().activePersonaId).toBe("brand");
  });
});
