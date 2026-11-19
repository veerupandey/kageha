import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { AgentPersona } from "../../api/types";
import { AgentsList } from "./AgentsList";

function persona(overrides: Partial<AgentPersona> = {}): AgentPersona {
  return {
    schema_version: 1,
    id: "matcha-expert",
    name: "Matcha Expert",
    avatar: "🍵",
    builtin: false,
    ...overrides,
  };
}

describe("AgentsList Hyperagent-style selection", () => {
  afterEach(() => {
    cleanup();
  });

  it("clicking a persona row selects that agent", () => {
    const onSelectPersona = vi.fn();
    render(
      <AgentsList
        agents={[]}
        onSelect={vi.fn()}
        personas={[persona({ id: "main", name: "Default", builtin: true }), persona()]}
        activePersonaId="main"
        onSelectPersona={onSelectPersona}
      />,
    );

    fireEvent.click(screen.getByTitle("Matcha Expert"));
    expect(onSelectPersona).toHaveBeenCalledWith(
      expect.objectContaining({ id: "matcha-expert", name: "Matcha Expert" }),
    );
  });

  it("marks the active persona row and shows configure on Default", () => {
    render(
      <AgentsList
        agents={[]}
        onSelect={vi.fn()}
        personas={[
          persona({ id: "main", name: "Default", avatar: "", builtin: true }),
          persona(),
        ]}
        activePersonaId="matcha-expert"
        onEditPersona={vi.fn()}
      />,
    );

    expect(screen.getByTitle("Matcha Expert").closest(".ka-sidebar-item")).toHaveClass("active");
    expect(screen.getByRole("button", { name: "Edit Default" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit Matcha Expert" })).toBeInTheDocument();
  });
});
