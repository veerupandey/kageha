// Presentation/restore cases retain legacy transport coverage; native streaming is exercised in sessionEvents.test.ts.
vi.mock("../lib/workerConnection", async () => import("../lib/legacyWorkerConnection"));
import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { WorkerDetailsPanel } from "./WorkerDetailsPanel";
import type { DurableJob } from "./DurableJobs";
import { streamSessionEvents } from "../api/stream";
vi.mock("../api/stream", () => ({ streamSessionEvents: vi.fn() }));
const openSession = vi.hoisted(() => vi.fn());
vi.mock("../store", () => ({ useAppStore: (selector: (s: unknown) => unknown) => selector({ openSession }) }));
const job: DurableJob = { id: "worker", label: "Researcher", kind: "agent", state: "running", session_id: "child", parent_session: "parent", created_at: 1, started_at: 1, revision: 1, can_cancel: true, attachable: true, artifacts: [], error: {}, result: {}, spec: { machine_active: false }, progress: { label: "Running a tool", summary: "Running a tool", activity: "tool_running", surface: "browser", steps: 3, updated_at: 1 } };
beforeEach(() => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value), removeItem: (key: string) => data.delete(key) });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.clearAllMocks(); });
describe("worker details", () => {
  it("shows that a retained patch needs human review", async () => {
    const handoff: DurableJob["handoff"] = { preview_id:"h2", target:"project", status:"review_required", review_required:true,
      conflicts:[], files:[{path:"src/fix.rs",change:"modified",before_text:"old",after_text:"new"}] };
    const completed: DurableJob = {...job,state:"succeeded",can_cancel:false,handoff};
    vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>({jobs:[completed]})})));
    await act(async()=>{render(<WorkerDetailsPanel initialJob={completed} parentSession="parent" onClose={vi.fn()} />);});
    const panel=screen.getByRole("region",{name:"Code handoff"});
    expect(panel).toHaveTextContent("Human review required");
    expect(panel).toHaveTextContent("The agent cannot apply it");
    expect(panel).toHaveTextContent("src/fix.rs");
  });
  it("shows the sanitized failure detail even when the result message is generic", async () => {
    const failed: DurableJob = { ...job, state:"failed", can_cancel:false,
      result:{ message:"The agent stopped before completing this turn.", failure_detail:"Model response timed out" } };
    vi.stubGlobal("fetch", vi.fn(async () => ({ok:true,json:async()=>({jobs:[failed]})})));
    await act(async()=>{render(<WorkerDetailsPanel initialJob={failed} parentSession="parent" onClose={vi.fn()} />);});
    expect(screen.getByRole("alert")).toHaveTextContent("Model response timed out");
  });
  it("shows a reviewable handoff and its conflict status", async () => {
    const handoff: DurableJob["handoff"] = {preview_id:"h1",target:"project",status:"conflict",conflicts:["src/file.py"],files:[{path:"src/file.py",change:"modified",before_text:"old value",after_text:"new value"}]};
    const completed: DurableJob = {...job,state:"succeeded",can_cancel:false,handoff};
    vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>({jobs:[completed]})})));
    await act(async()=>{render(<WorkerDetailsPanel initialJob={completed} parentSession="parent" onClose={vi.fn()} />);});
    expect(screen.getByRole("region",{name:"Code handoff"})).toHaveTextContent("Conflicts need review");
    expect(screen.getByRole("alert")).toHaveTextContent("src/file.py");
    fireEvent.click(screen.getByText("src/file.py · modified"));
    expect(screen.getByText("old value")).toBeVisible();
    expect(screen.getByText("new value")).toBeVisible();
  });
  it("opens a registered worker artifact from its own session", async () => {
    const completed: DurableJob = {...job,state:"succeeded",can_cancel:false,artifacts:["workspace/outputs/worker code.py"]};
    vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>({jobs:[completed]})})));
    await act(async()=>{render(<WorkerDetailsPanel initialJob={completed} parentSession="parent" onClose={vi.fn()} />);});
    expect(screen.getByRole("link",{name:"worker code.py"})).toHaveAttribute("href","/api/sessions/child/files/workspace/outputs/worker%20code.py");
  });
  it("inspects child tools in place with the parent session feed and stops only the observer", async () => {
    const child: DurableJob = { ...job, progress: { ...job.progress!, turn_id: "child-turn" } };
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [child] }) })));
    let signal: AbortSignal | undefined;
    vi.mocked(streamSessionEvents).mockImplementation(async (_url, receive, observed) => {
      signal = observed;
      receive({ events: [{ sequence: 11, turn_id: "child-turn", kind: "tool_card", tool_card: { id: "read", name: "read_file", status: "completed", args_preview: "artifacts/evidence.txt", result_preview: "Evidence found" } }], live: {} });
      await new Promise<void>(resolve => observed.addEventListener("abort", () => resolve()));
    });
    await act(async () => { render(<WorkerDetailsPanel initialJob={child} parentSession="parent" onClose={vi.fn()} />); });
    expect(screen.getByRole("region", { name: "Agent tool activity" })).toBeInTheDocument();
    expect(streamSessionEvents).toHaveBeenCalledWith("/api/sessions/child/events/stream?turn_id=child-turn&after_sequence=0&parent_session_id=parent&job_id=worker", expect.any(Function), expect.any(AbortSignal));
    expect(screen.getByRole("button", { name: /Reading file evidence.txt/ })).toBeVisible();
    expect(openSession).not.toHaveBeenCalled();
    cleanup();
    expect(signal?.aborted).toBe(true);
    expect(streamSessionEvents).toHaveBeenCalledTimes(1);
    expect(vi.mocked(fetch).mock.calls.every(([, options]) => !options?.method || options.method === "GET")).toBe(true);
  });

  it("keeps messaging visible while assignment details stay collapsed", async () => {
    const assigned: DurableJob = { ...job, objective: "Review lighting coverage", spec: { contract: { expected_output: "Coverage report", acceptance_criteria: ["Include night-time examples"], constraints: [], verification: "required", verification_reason: "" } } };
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [assigned] }) })));
    await act(async () => { render(<WorkerDetailsPanel initialJob={assigned} parentSession="parent" onClose={vi.fn()} />); });
    expect(screen.getByRole("button", { name: "Message Researcher" })).toBeVisible();
    expect(screen.getByText("Include night-time examples")).not.toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
    expect(screen.getByRole("textbox", { name: "Message Researcher" })).toHaveFocus();
    expect(openSession).not.toHaveBeenCalled();
  });
  it("stops only the selected agent and waits for confirmed state", async () => {
    const fetch = vi.fn(async (_url: string, _init?: RequestInit) => ({ ok: true, json: async () => ({ jobs: [job] }) }));
    vi.stubGlobal("fetch", fetch);
    await act(async () => { render(<WorkerDetailsPanel initialJob={job} parentSession="parent" onClose={vi.fn()} />); });
    fireEvent.click(screen.getByRole("button", { name: "Stop agent" }));
    await screen.findByText("Stop requested. Waiting for the agent to finish stopping.");
    expect(fetch).toHaveBeenCalledWith("/api/jobs/worker/cancel", expect.objectContaining({ method: "POST" }));
    expect(fetch.mock.calls.some(([url]) => String(url).includes("/api/chat/cancel"))).toBe(false);
    expect(screen.getByRole("button", { name: "Stopping…" })).toBeDisabled();
  });
  it("keeps stop available after a failed request", async () => {
    vi.stubGlobal("fetch", vi.fn(async (url: string) => ({ ok: !url.endsWith("/cancel"), json: async () => ({ jobs: [job] }) })));
    await act(async () => { render(<WorkerDetailsPanel initialJob={job} parentSession="parent" onClose={vi.fn()} />); });
    fireEvent.click(screen.getByRole("button", { name: "Stop agent" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not stop this agent");
    expect(screen.getByRole("button", { name: "Stop agent" })).toBeEnabled();
  });
  it("does not promise a retry after the worker connection is lost", async () => {
    const disconnected: DurableJob = { ...job, health: { worker: "lease_expired", observed_at: 124 }, progress: { ...job.progress!, retry_at: 123 } };
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [disconnected] }) })));
    await act(async () => { render(<WorkerDetailsPanel initialJob={disconnected} parentSession="parent" onClose={vi.fn()} />); });
    expect(screen.queryByText(/Retry scheduled for/)).not.toBeInTheDocument();
    expect(screen.getByText("Connection interrupted")).toBeInTheDocument();
  });
  it("shows the complete assignment and result in the details panel", async () => {
    const objective = "First assignment line.\n" + "Assignment detail. ".repeat(100);
    const message = "Worker result. ".repeat(1000) + "Final worker sentence.";
    const completed = { ...job, state: "done", objective, result: { message } };
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [completed] }) })));
    let container!: HTMLElement;
    await act(async () => { ({ container } = render(<WorkerDetailsPanel initialJob={completed} parentSession="parent" onClose={vi.fn()} />)); });
    expect(container.textContent).toContain(objective);
    expect(container.textContent).toContain(message);
  });
  it("keeps inspection separate from opening the full worker conversation", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [job] }) })));
    await act(async () => { render(<WorkerDetailsPanel initialJob={job} parentSession="parent" onClose={vi.fn()} />); });
    expect(screen.getByText("Own context · browser in use")).toBeInTheDocument();
    expect(screen.getByText("Running a tool")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Message Researcher" })).toBeInTheDocument();
    expect(openSession).not.toHaveBeenCalled();
    expect(screen.getByText("Activity & technical details").closest("details")).not.toHaveAttribute("open");
  });
  it("closes with Escape and gives the parent conversation control back", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [job] }) })));
    const close = vi.fn();
    await act(async () => { render(<WorkerDetailsPanel initialJob={job} parentSession="parent" onClose={close} />); });
    expect(screen.getByRole("button", { name: "Close worker details" })).toHaveFocus();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(close).toHaveBeenCalledOnce();
    expect(openSession).not.toHaveBeenCalled();
  });
  it("does not present a failed tool step as a successful result", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [] }) })));
    await act(async () => { render(<WorkerDetailsPanel initialJob={{ ...job, activity: [{ id: 1, label: "Finished browser_fill", created_at: 1, failed: true }] }} parentSession="parent" onClose={vi.fn()} />); });
    expect(screen.getByText("A step needs attention")).toBeInTheDocument();
    expect(screen.queryByText("Updated the form")).not.toBeInTheDocument();
  });
  it("renders specialist switcher when multiple workers exist and switches on click", async () => {
    const job2: DurableJob = { ...job, id: "worker-2", label: "Analyst", session_id: "child-2" };
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [job, job2] }) })));
    const select = vi.fn();
    await act(async () => { render(<WorkerDetailsPanel initialJob={job} parentSession="parent" onClose={vi.fn()} onSelectWorker={select} />); });
    expect(screen.getByRole("navigation", { name: "Agent tree" })).toBeInTheDocument();
    const analystBtn = screen.getByRole("treeitem", { name: /Analyst/ });
    expect(analystBtn).toBeInTheDocument();
    fireEvent.click(analystBtn);
    expect(select).toHaveBeenCalledWith(expect.objectContaining({ id: "worker-2" }));
  });
});

it("does not apply a late stop response to a newly selected agent", async () => {
 const first={...job,progress:{...job.progress!,turn_id:"old-turn"}};
 const next={...job,id:"new-worker",label:"New reviewer",session_id:"new-child",progress:{...job.progress!,turn_id:"new-turn"}};
 let reply:((value:unknown)=>void)|undefined;
 vi.stubGlobal("fetch",vi.fn((_url,options)=>options?.method==="POST"?new Promise(resolve=>{reply=resolve;}):Promise.resolve({ok:true,json:async()=>({jobs:[first,next]})})));
 let rerender!:(view:React.ReactNode)=>void;
 await act(async()=>{({rerender}=render(<WorkerDetailsPanel initialJob={first} parentSession="parent" onClose={vi.fn()} />));});
 fireEvent.click(screen.getByRole("button",{name:"Stop agent"}));
 expect(JSON.parse(vi.mocked(fetch).mock.calls.find(([,options])=>options?.method==="POST")![1]!.body as string)).toMatchObject({expected_turn_id:"old-turn"});
 await act(async()=>{rerender(<WorkerDetailsPanel initialJob={next} parentSession="parent" onClose={vi.fn()} />);});
 expect(screen.getByRole("button",{name:"Stop agent"})).toBeEnabled();
 await act(async()=>{reply?.({ok:true});});
 expect(screen.getByRole("button",{name:"Stop agent"})).toBeEnabled();
 expect(screen.queryByText(/Stop requested/)).not.toBeInTheDocument();
});
