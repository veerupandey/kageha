import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { PersonaEditorDialog } from "./PersonaEditorDialog";
import { useAppStore } from "../../store";
import type { AgentPersona } from "../../api/types";

function persona(overrides: Partial<AgentPersona> = {}): AgentPersona {
  return {
    schema_version: 1,
    id: "matcha-expert",
    name: "Matcha Expert",
    builtin: false,
    ...overrides,
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as Response;
}

/** Reset the persona slice of the store between tests to avoid pollution. */
function resetPersonaState() {
  useAppStore.setState({ personas: [], personasError: null });
}

interface FetchHandlers {
  createResponse?: () => Response;
  draftResponse?: () => Response | Promise<Response>;
  updateResponse?: () => Response;
  deleteResponse?: () => Response;
}

describe("PersonaEditorDialog create/edit/delete flows (Requirement 7.2)", () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    cleanup();
    resetPersonaState();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  function mockFetchImpl(handlers: FetchHandlers = {}) {
    fetchMock.mockImplementation(async (url: string, options: RequestInit = {}) => {
      const method = (options.method || "GET").toUpperCase();

      if (url === "/api/slash-catalog") {
        return jsonResponse({ commands: [] });
      }
      if (url.includes("/knowledge/upload") && method === "POST") {
        return jsonResponse({ ok: true, filename: "policy.md", kb_id: "agent-matcha-expert" });
      }
      if (url.includes("/knowledge") && method === "GET") {
        return jsonResponse({ kb_id: "agent-matcha-expert", engine: "zvec", sources: [] });
      }
      if (url === "/api/agents" && method === "GET") {
        return jsonResponse({ agents: [persona()] });
      }
      if (url === "/api/agents/draft" && method === "POST") {
        return handlers.draftResponse
          ? handlers.draftResponse()
          : jsonResponse({ draft: { description: "Generated description", system_prompt: "Generated instructions" }, saved: false });
      }
      if (url === "/api/agents" && method === "POST") {
        return handlers.createResponse
          ? handlers.createResponse()
          : jsonResponse({ agent: persona() });
      }
      if (url.startsWith("/api/agents/") && method === "PUT") {
        return handlers.updateResponse
          ? handlers.updateResponse()
          : jsonResponse({ agent: persona({ name: "Updated Name" }) });
      }
      if (url.startsWith("/api/agents/") && method === "DELETE") {
        return handlers.deleteResponse
          ? handlers.deleteResponse()
          : jsonResponse({ ok: true, id: "matcha-expert", deleted: true });
      }
      throw new Error(`Unhandled fetch in test: ${method} ${url}`);
    });
  }

  it("create flow: submits the form, POSTs to /api/agents, and calls onSaved with the created persona", async () => {
    mockFetchImpl();
    const onSaved = vi.fn();

    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={onSaved} />);

    fireEvent.change(screen.getByLabelText("Agent id"), {
      target: { value: "matcha-expert" },
    });
    fireEvent.change(screen.getByLabelText("Agent name"), {
      target: { value: "Matcha Expert" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create agent" }));

    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));

    const postCall = fetchMock.mock.calls.find(
      ([url, opts]) => url === "/api/agents" && (opts as RequestInit)?.method === "POST",
    );
    expect(postCall).toBeTruthy();
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.id).toBe("matcha-expert");
    expect(body.name).toBe("Matcha Expert");
    expect(body.knowledge.inherit_workspace).toBe(true);
    expect(body.knowledge.retriever.kind).toBe("zvec");
    expect(onSaved).toHaveBeenCalledWith(expect.objectContaining({ id: "matcha-expert" }));
  });

  it("create flow: shows an inline PersonaExistsError message on duplicate id and does not call onSaved", async () => {
    mockFetchImpl({
      createResponse: () =>
        jsonResponse(
          { error: "Persona already exists", error_type: "PersonaExistsError" },
          409,
        ),
    });
    const onSaved = vi.fn();

    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={onSaved} />);

    fireEvent.change(screen.getByLabelText("Agent id"), {
      target: { value: "matcha-expert" },
    });
    fireEvent.change(screen.getByLabelText("Agent name"), {
      target: { value: "Matcha Expert" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create agent" }));

    expect(await screen.findByText(/PersonaExistsError/i)).toBeInTheDocument();
    expect(onSaved).not.toHaveBeenCalled();
  });

  it("edit flow: submits an updated name via PUT /api/agents/{id} and calls onSaved", async () => {
    mockFetchImpl();
    const onSaved = vi.fn();
    const existing = persona({ id: "matcha-expert", name: "Matcha Expert" });

    render(
      <PersonaEditorDialog mode="edit" persona={existing} onClose={vi.fn()} onSaved={onSaved} />,
    );

    fireEvent.change(screen.getByLabelText("Agent name"), {
      target: { value: "Matcha Grandmaster" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));

    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));

    const putCall = fetchMock.mock.calls.find(
      ([url, opts]) =>
        url === "/api/agents/matcha-expert" && (opts as RequestInit)?.method === "PUT",
    );
    expect(putCall).toBeTruthy();
    const body = JSON.parse((putCall![1] as RequestInit).body as string);
    expect(body.name).toBe("Matcha Grandmaster");
  });

  it("edit flow keeps model chains: lists stay lists and single names stay strings", async () => {
    mockFetchImpl();
    const onSaved = vi.fn();
    const existing = persona({
      model_preferences: { executor: ["cheap", "backup"], planner: "strong", reviewer: ["judge"] },
    });

    render(
      <PersonaEditorDialog mode="edit" persona={existing} onClose={vi.fn()} onSaved={onSaved} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));

    const putCall = fetchMock.mock.calls.find(
      ([url, opts]) =>
        url === "/api/agents/matcha-expert" && (opts as RequestInit)?.method === "PUT",
    );
    const body = JSON.parse((putCall![1] as RequestInit).body as string);
    expect(body.model_preferences).toMatchObject({
      executor: ["cheap", "backup"],
      planner: "strong",
      reviewer: "judge",
    });
  });

  it("delete flow: confirms, DELETEs /api/agents/{id}, then calls onDeleted and onClose", async () => {
    mockFetchImpl();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const onDeleted = vi.fn();
    const onClose = vi.fn();
    const existing = persona({ id: "matcha-expert", name: "Matcha Expert", builtin: false });

    render(
      <PersonaEditorDialog
        mode="edit"
        persona={existing}
        onClose={onClose}
        onSaved={vi.fn()}
        onDeleted={onDeleted}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Delete agent" }));

    await waitFor(() => expect(onDeleted).toHaveBeenCalledWith("matcha-expert"));
    expect(onClose).toHaveBeenCalled();

    const deleteCall = fetchMock.mock.calls.find(
      ([url, opts]) =>
        url === "/api/agents/matcha-expert" && (opts as RequestInit)?.method === "DELETE",
    );
    expect(deleteCall).toBeTruthy();
  });

  it("builtin persona has no Delete button", async () => {
    mockFetchImpl();
    const builtinPersona = persona({ id: "main", name: "Main", builtin: true });

    render(
      <PersonaEditorDialog mode="edit" persona={builtinPersona} onClose={vi.fn()} onSaved={vi.fn()} />,
    );

    // Let the on-mount /api/slash-catalog fetch resolve before asserting.
    await waitFor(() =>
      expect(fetchMock.mock.calls.some(([url]) => url === "/api/slash-catalog")).toBe(true),
    );

    expect(screen.queryByRole("button", { name: "Delete agent" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("Instructions")).toBeDisabled();
    expect(screen.getByLabelText("Agent name")).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: /Use only this agent’s knowledge/ })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: /Use only this agent’s knowledge/ })).not.toBeChecked();
    expect(screen.queryByLabelText("Extra knowledge base ids")).not.toBeInTheDocument();
    expect(screen.queryByText("Access & model preferences")).not.toBeInTheDocument();
    expect(screen.getByText(/Add PDF, markdown, image, audio, or video/)).toBeInTheDocument();
  });

  it("create flow hides the knowledge upload control until the agent exists", async () => {
    mockFetchImpl();
    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={vi.fn()} />);
    await waitFor(() =>
      expect(fetchMock.mock.calls.some(([url]) => url === "/api/slash-catalog")).toBe(true),
    );
    expect(screen.queryByText(/Add PDF, markdown, image, audio, or video/)).not.toBeInTheDocument();
  });

  it("edit flow shows the knowledge library upload control", async () => {
    mockFetchImpl();
    render(
      <PersonaEditorDialog
        mode="edit"
        persona={persona()}
        onClose={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(screen.getByText(/Add PDF, markdown, image, audio, or video/)).toBeInTheDocument(),
    );
  });

  it("create flow: can save manual instructions without generation", async () => {
    mockFetchImpl();
    const onSaved = vi.fn();
    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={onSaved} />);
    fireEvent.change(screen.getByLabelText("Agent name"), { target: { value: "Designer" } });
    fireEvent.change(screen.getByLabelText("Instructions"), { target: { value: "My exact instructions" } });
    fireEvent.click(screen.getByRole("button", { name: "Create agent" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));
    const postCall = fetchMock.mock.calls.find(
      ([url, opts]) => url === "/api/agents" && (opts as RequestInit)?.method === "POST",
    );
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.system_prompt).toBe("My exact instructions");
  });

  it("AI help produces an editable draft without saving, then saves only reviewed edits", async () => {
    mockFetchImpl();
    const onSaved = vi.fn();
    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={onSaved} />);
    fireEvent.change(screen.getByLabelText("Agent name"), { target: { value: "Designer" } });
    fireEvent.change(screen.getByLabelText("Instructions"), { target: { value: "My rough notes" } });
    expect(fetchMock.mock.calls.some(([url]) => url === "/api/agents/draft")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Help me write instructions" }));
    await waitFor(() => expect(screen.getByLabelText("Instructions")).toHaveValue("Generated instructions"));
    expect(onSaved).not.toHaveBeenCalled();
    expect(fetchMock.mock.calls.some(([url, opts]) => url === "/api/agents" && opts?.method === "POST")).toBe(false);
    expect(screen.getByRole("status")).toHaveTextContent("Nothing has been saved yet");
    const draftCall = fetchMock.mock.calls.find(([url]) => url === "/api/agents/draft");
    expect(JSON.parse(draftCall![1].body)).toEqual({ name: "Designer", description: "", system_prompt: "My rough notes" });
    fireEvent.change(screen.getByLabelText("Instructions"), { target: { value: "My reviewed and edited draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Create agent" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));
    const saveCall = fetchMock.mock.calls.find(([url, opts]) => url === "/api/agents" && opts?.method === "POST");
    expect(JSON.parse(saveCall![1].body).system_prompt).toBe("My reviewed and edited draft");
  });

  it("keeps the draft editable only after generation completes and never auto-saves", async () => {
    let resolveDraft!: (response: Response) => void;
    mockFetchImpl({ draftResponse: () => new Promise((resolve) => { resolveDraft = resolve; }) });
    const onSaved = vi.fn();
    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={onSaved} />);
    fireEvent.change(screen.getByLabelText("Agent name"), { target: { value: "Designer" } });
    fireEvent.click(screen.getByRole("button", { name: "Help me write instructions" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Drafting instructions…" })).toBeDisabled());
    expect(screen.getByLabelText("Instructions")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Create agent" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Close" })).toBeDisabled();
    await act(async () => resolveDraft(jsonResponse({ draft: { description: "Draft", system_prompt: "Editable instructions" } })));
    expect(screen.getByLabelText("Instructions")).toBeEnabled();
    expect(screen.getByLabelText("Instructions")).toHaveValue("Editable instructions");
    expect(onSaved).not.toHaveBeenCalled();
    expect(fetchMock.mock.calls.some(([url, opts]) => url === "/api/agents" && opts?.method === "POST")).toBe(false);
  });

  it("AI drafting failure preserves the user's instructions and does not save", async () => {
    mockFetchImpl({ draftResponse: () => jsonResponse({ error: "Model unavailable" }, 502) });
    const onSaved = vi.fn();
    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={onSaved} />);
    fireEvent.change(screen.getByLabelText("Agent name"), { target: { value: "Designer" } });
    fireEvent.change(screen.getByLabelText("Instructions"), { target: { value: "My notes" } });
    fireEvent.click(screen.getByRole("button", { name: "Help me write instructions" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("Model unavailable"));
    expect(screen.getByLabelText("Instructions")).toHaveValue("My notes");
    expect(onSaved).not.toHaveBeenCalled();
  });

  it("create flow: slugs the id from the name unless the id field was edited", async () => {
    mockFetchImpl();
    const onSaved = vi.fn();

    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={onSaved} />);

    fireEvent.change(screen.getByLabelText("Agent name"), {
      target: { value: "Matcha Expert" },
    });
    expect(screen.getByLabelText("Agent id")).toHaveValue("matcha-expert");
    fireEvent.click(screen.getByRole("button", { name: "Create agent" }));

    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));
    const postCall = fetchMock.mock.calls.find(
      ([url, opts]) => url === "/api/agents" && (opts as RequestInit)?.method === "POST",
    );
    const body = JSON.parse((postCall![1] as RequestInit).body as string);
    expect(body.id).toBe("matcha-expert");
  });

  it("dedupes pinned skills that share the same id", async () => {
    fetchMock.mockImplementation(async (url: string) => {
      if (url === "/api/slash-catalog") {
        return jsonResponse({
          commands: [
            { id: "computer", label: "/computer", kind: "skill", title: "computer_use", description: "slash" },
            { id: "computer_use", label: "/computer_use", kind: "skill", title: "computer_use", description: "catalog" },
            { id: "web_search", label: "/web_search", kind: "skill", title: "web_search" },
          ],
        });
      }
      throw new Error(`Unhandled fetch in test: ${url}`);
    });

    render(<PersonaEditorDialog mode="create" onClose={vi.fn()} onSaved={vi.fn()} />);

    await waitFor(() => {
      expect(screen.getByText("computer_use")).toBeInTheDocument();
      expect(screen.getByText("web_search")).toBeInTheDocument();
    });
    expect(screen.getAllByText("computer_use")).toHaveLength(1);
  });
});
