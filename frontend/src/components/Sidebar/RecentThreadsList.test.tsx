import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { SessionSummary } from "../../api/types";
import { useAppStore } from "../../store";
import { RecentThreadsList } from "./RecentThreadsList";
import { emptyRun } from "../../store/helpers";
import { THREAD_PAGE } from "./threadPaging";
import * as apiClient from "../../api/client";

type IoCallback = IntersectionObserverCallback;

let ioCallback: IoCallback | null = null;
const initialStore = useAppStore.getState();

function session(i: number, archived = false): SessionSummary {
  return {
    session_id: `sess-${String(i).padStart(3, "0")}`,
    title: `Thread ${i}`,
    archived,
  };
}

describe("RecentThreadsList scroll paging", () => {
  beforeEach(() => {
    ioCallback = null;
    vi.stubGlobal(
      "IntersectionObserver",
      class {
        constructor(cb: IoCallback) {
          ioCallback = cb;
        }
        observe() {}
        disconnect() {}
        unobserve() {}
        takeRecords() {
          return [];
        }
      },
    );
    useAppStore.setState({ personas: [], runs: {}, loadProjects: vi.fn().mockResolvedValue(undefined) });
  });

  afterEach(() => {
    cleanup();
    useAppStore.setState(initialStore, true);
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it.each(["Fork session", "Export session"])("offers %s for the selected thread", (label) => {
    const forkSession = vi.fn().mockResolvedValue(undefined);
    const exportSession = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({ forkSession, exportSession });
    render(<RecentThreadsList sessions={[session(1)]} activeSessionId={null} onOpen={vi.fn()} />);
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 1/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: label }));
    expect(label === "Fork session" ? forkSession : exportSession).toHaveBeenCalledWith("sess-001");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("opens the lifecycle dialog when archiving waits for SessionEnd", async () => {
    const archiveSession=vi.fn().mockResolvedValue({session_id:"sess-001",lifecycle_status:"closing"});
    const priorRefresh=useAppStore.getState().refreshSessions;
    const fetchSession=vi.spyOn(apiClient,"api").mockImplementation(async (path) => {
      if (path === "/api/sessions/sess-001") return {session_id:"sess-001",lifecycle_status:"closing",lifecycle:{status:"closing",turn_id:"close-1",archive:true},pending_approval:{approval_id:"approve-1",turn_id:"close-1",detail:"Run SessionEnd hook"}} as never;
      throw new Error(`Unexpected route: ${path}`);
    });
    useAppStore.setState({archiveSession,refreshSessions:vi.fn().mockResolvedValue(undefined)});
    render(<RecentThreadsList sessions={[session(1)]} activeSessionId={null} onOpen={vi.fn()} />);
    fireEvent.contextMenu(screen.getByRole("button",{name:/^Thread 1/}));
    fireEvent.click(screen.getByRole("menuitem",{name:"Archive"}));
    expect(await screen.findByRole("dialog",{name:"Session lifecycle for Thread 1"})).toBeInTheDocument();
    expect(await screen.findByText("Run SessionEnd hook")).toBeVisible();
    expect(archiveSession).toHaveBeenCalledWith("sess-001",true);
    fetchSession.mockRestore();
    act(() => useAppStore.setState({refreshSessions:priorRefresh}));
  });

  it("renames the right-clicked thread and keeps the current conversation selected", async () => {
    const sessions = [session(1), session(2)];
    const request = vi.spyOn(apiClient, "api").mockResolvedValue({ session_id: "sess-002", title: "Saved name" });
    const openSession = vi.fn();
    useAppStore.setState({
      sessions, sessionId: "sess-001", sessionTitle: "Thread 1", openSession,
      refreshSessions: vi.fn().mockResolvedValue(undefined),
    });
    render(<RecentThreadsList sessions={sessions} activeSessionId="sess-001" onOpen={openSession} />);
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 2/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
    const name = screen.getByRole("textbox", { name: "Thread name" });
    expect(name).toHaveValue("Thread 2");
    fireEvent.change(name, { target: { value: "  Saved name  " } });
    fireEvent.click(screen.getByRole("button", { name: "Save name" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(request).toHaveBeenCalledWith("/api/sessions/sess-002", {
      method: "PATCH", body: JSON.stringify({ title: "Saved name" }),
    });
    expect(useAppStore.getState().sessions[1].title).toBe("Saved name");
    expect(useAppStore.getState().sessionTitle).toBe("Thread 1");
    expect(openSession).not.toHaveBeenCalled();
  });

  it("requires confirmation before deleting the selected thread", async () => {
    const deleteSession = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({ deleteSession });
    render(<RecentThreadsList sessions={[session(1), session(2)]} activeSessionId="sess-001" onOpen={vi.fn()} />);
    const askToDelete = () => {
      fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 2/ }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Delete" }));
    };
    askToDelete();
    expect(screen.getByRole("dialog", { name: "Delete thread" })).toHaveTextContent("Thread 2");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(deleteSession).not.toHaveBeenCalled();
    askToDelete();
    fireEvent.click(screen.getByRole("button", { name: "Delete thread" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(deleteSession).toHaveBeenCalledExactlyOnceWith("sess-002");
  });

  it("routes a blocked delete to the session's lifecycle dialog", async () => {
    const deleteSession = vi.fn().mockRejectedValue(new apiClient.ApiError("Close the session first", 409, {
      code: "session_close_required",
    }));
    useAppStore.setState({ deleteSession });
    const request = vi.spyOn(apiClient, "api").mockResolvedValue({ lifecycle_status: "open" });
    render(<RecentThreadsList sessions={[session(1)]} activeSessionId={null} onOpen={vi.fn()} />);
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 1/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete thread" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Close the session first");
    fireEvent.click(screen.getByRole("button", { name: "Session lifecycle" }));
    expect(await screen.findByRole("dialog", { name: "Session lifecycle for Thread 1" })).toBeVisible();
    expect(request).toHaveBeenCalledWith("/api/sessions/sess-001");
  });

  it.each([false, true])("toggles pinning when the current pinned flag is %s", async (pinned) => {
    const pinSession = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({ pinSession });
    render(<RecentThreadsList sessions={[{ ...session(1), pinned }]} activeSessionId={null} onOpen={vi.fn()} />);
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 1/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: pinned ? "Unpin" : "Pin" }));
    expect(pinSession).toHaveBeenCalledExactlyOnceWith("sess-001", !pinned);
  });

  it("keeps the project picker above the menu backdrop and assigns the chosen project", async () => {
    const setSessionProject = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({
      projects: [{ id: "learning", name: "Learning", color: "blue" }],
      setSessionProject,
    });
    render(<RecentThreadsList sessions={[session(1)]} activeSessionId={null} onOpen={vi.fn()} />);
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 1/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Add to project" }));
    const learning = await screen.findByRole("button", { name: "Learning" });
    const picker = learning.closest("[data-radix-popper-content-wrapper]")?.firstElementChild;
    const menu = screen.getByRole("menu");
    expect(picker).toHaveStyle({ zIndex: "10000" });
    expect(menu).toHaveClass("z-[9999]");
    fireEvent.keyDown(screen.getByPlaceholderText("Search or create project..."), { key: "Escape" });
    expect(screen.queryByRole("button", { name: "Learning" })).not.toBeInTheDocument();
    expect(menu).toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "Add to project" }));
    fireEvent.click(await screen.findByRole("button", { name: "Learning" }));
    await waitFor(() => expect(setSessionProject).toHaveBeenCalledWith("sess-001", "learning"));
    await waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
  });

  it("removes a thread from its project directly from the menu", () => {
    const setSessionProject = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({ setSessionProject });
    render(<RecentThreadsList sessions={[{ ...session(1), project_id: "learning" }]} activeSessionId={null} onOpen={vi.fn()} />);
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 1/ }));
    expect(screen.getByRole("menuitem", { name: "Change project" })).toBeVisible();
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove from project" }));
    expect(setSessionProject).toHaveBeenCalledExactlyOnceWith("sess-001", null);
  });

  it("supports keyboard navigation and restores focus to the options button", () => {
    render(<RecentThreadsList sessions={[session(1)]} activeSessionId={null} onOpen={vi.fn()} />);
    const options = screen.getByRole("button", { name: "Options for Thread 1" });
    options.focus();
    fireEvent.click(options);
    expect(screen.getByRole("menuitem", { name: "Fork session" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Export session" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "End" });
    expect(screen.getByRole("menuitem", { name: "Delete" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Home" });
    expect(screen.getByRole("menuitem", { name: "Fork session" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(options).toHaveFocus();
  });

  it("reveals the copied conversation after a fork, but keeps the view on failure", async () => {
    const onForked = vi.fn();
    const forkSession = vi.fn().mockResolvedValue(undefined);
    useAppStore.setState({ forkSession });
    render(<RecentThreadsList sessions={[session(1)]} activeSessionId={null} onOpen={vi.fn()} onForked={onForked} />);
    const fork = () => {
      fireEvent.click(screen.getByRole("button", { name: "Options for Thread 1" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Fork session" }));
    };
    fork();
    await waitFor(() => expect(onForked).toHaveBeenCalledOnce());
    forkSession.mockRejectedValueOnce(new Error("Stop the running turn"));
    fork();
    await act(async () => {});
    expect(onForked).toHaveBeenCalledOnce();
  });

  it("shows the first page then grows when the sentinel intersects", () => {
    const sessions = Array.from({ length: THREAD_PAGE + 5 }, (_, i) =>
      session(i + 1),
    );
    render(
      <div className="ka-sidebar-threads">
        <RecentThreadsList
          sessions={sessions}
          activeSessionId={null}
          onOpen={vi.fn()}
        />
      </div>,
    );

    expect(screen.getByText("Thread 1")).toBeInTheDocument();
    expect(screen.getByText(`Thread ${THREAD_PAGE}`)).toBeInTheDocument();
    expect(screen.queryByText(`Thread ${THREAD_PAGE + 1}`)).not.toBeInTheDocument();

    expect(ioCallback).toBeTruthy();
    act(() => {
      ioCallback?.(
        [{ isIntersecting: true } as IntersectionObserverEntry],
        {} as IntersectionObserver,
      );
    });

    expect(screen.getByText(`Thread ${THREAD_PAGE + 5}`)).toBeInTheDocument();
  });

  it("shows archived threads on demand and allows restoring them", async () => {
    const archiveSession = vi.fn().mockResolvedValue({ session_id: "sess-002", archived: false });
    useAppStore.setState({ archiveSession });
    render(
      <RecentThreadsList
        sessions={[session(1), session(2, true), session(3)]}
        activeSessionId={null}
        onOpen={vi.fn()}
      />,
    );
    expect(screen.getByText("Thread 1")).toBeInTheDocument();
    expect(screen.getByText("Thread 3")).toBeInTheDocument();
    expect(screen.queryByText("Thread 2")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Show archived threads" }));
    expect(screen.queryByText("Thread 1")).not.toBeInTheDocument();
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Thread 2/ }));
    await act(async () => fireEvent.click(screen.getByRole("menuitem", { name: "Unarchive" })));
    expect(archiveSession).toHaveBeenCalledExactlyOnceWith("sess-002", false);
  });

  it("does not show Running from a stale local flag once the server reports no running turn", () => {
    const run = { ...emptyRun("sess-001", "web-sess-001"), sending: true, status: "running" as const,
      statusLabel: "Reconnecting…", startedAt: 1_000,
      transport: { mode: "polling" as const, lastSequence: 9, lastFrameAt: 0, reconnectCount: 1, turnId: "t1" } };
    useAppStore.setState({ runs: { "sess-001": run },
      serverTurns: { "sess-001": { turnId: "", status: "success", running: false, observedAt: 2_000 } } });
    render(<RecentThreadsList sessions={[{ ...session(1), status: "running", active_turn: { turn_id: "t1", status: "running" } }]}
      activeSessionId={null} onOpen={vi.fn()} />);
    expect(screen.getByText("Thread 1")).toBeInTheDocument();
    expect(screen.queryByText("Running")).not.toBeInTheDocument();
  });
  it("shows a turn waiting for the credential unlock as running", () => {
    useAppStore.setState({ runs: {}, serverTurns: {} });
    render(<RecentThreadsList sessions={[{ ...session(1), status: "waiting_for_unlock",
      active_turn: { turn_id: "t1", status: "waiting_for_unlock", phase: "waiting_for_unlock" } }]}
      activeSessionId={null} onOpen={vi.fn()} />);
    expect(screen.getByText("Running")).toHaveAttribute("title", "Waiting for credential unlock");
  });
  it("keeps subagent sessions in the parent task view", () => {
    render(
      <RecentThreadsList
        sessions={[session(1), { ...session(2), is_subagent: true }, session(3)]}
        activeSessionId={null}
        onOpen={vi.fn()}
      />,
    );
    expect(screen.getByText("Thread 1")).toBeInTheDocument();
    expect(screen.getByText("Thread 3")).toBeInTheDocument();
    expect(screen.queryByText("Thread 2")).not.toBeInTheDocument();
  });
});
