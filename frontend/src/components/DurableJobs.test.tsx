// Presentation/restore cases retain legacy transport coverage; native streaming is exercised in sessionEvents.test.ts.
vi.mock("../lib/workerConnection", async () => import("../lib/legacyWorkerConnection"));
import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { DurableJobs, JobQuestion, useDurableJobs, type DurableJob } from "./DurableJobs";
import { DelegatedWork } from "./DelegatedWork";

const openSession = vi.hoisted(() => vi.fn());
const receiveJobReply = vi.hoisted(() => vi.fn());
const state = vi.hoisted(() => ({ sessionId: "parent" }));
vi.mock("../store", () => ({ useAppStore: (selector: (state: unknown) => unknown) => selector({ openSession, receiveJobReply, sessionId: state.sessionId }) }));
beforeEach(() => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value), removeItem: (key: string) => data.delete(key) });
});
afterEach(() => { state.sessionId = "parent"; cleanup(); vi.unstubAllGlobals(); vi.clearAllMocks(); vi.useRealTimers(); });
const job: DurableJob = { id: "a", label: "Narration", kind: "agent", state: "waiting_input", session_id: "child", created_at: 1, started_at: 2, revision: 2, can_cancel: true, attachable: true, artifacts: ["/output/audio.wav"], error: { code: "approval_required", message: "Run render" }, result: {} };

describe("durable jobs", () => {
  it("shows a recoverable update error without inventing an empty contribution count", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("Refreshing specialist updates took too long")));
    await act(async () => { render(<DelegatedWork />); });
    expect(screen.getByRole("alert")).toHaveTextContent("Refreshing specialist updates took too long");
    expect(screen.queryByText("Specialist contributions")).not.toBeInTheDocument();
    expect(screen.queryByText(/0 contributions/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Show agents" })).not.toBeInTheDocument();
    vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ jobs: [] }) } as Response);
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByRole("region")).not.toBeInTheDocument();
  });

  it("preserves known contributions when a later update fails", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [{ ...job, state: "succeeded" }] }) })));
    await act(async () => { render(<DelegatedWork />); });
    vi.mocked(fetch).mockRejectedValue(new Error("Updates unavailable"));
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(screen.getByText(/1 contribution ·/)).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("Updates unavailable");
  });

  it("makes every contribution reachable after all specialists finish", async () => {
    const jobs = Array.from({ length: 6 }, (_, i) => ({ ...job, id: `done-${i}`, label: `Specialist ${i}`, state: "succeeded", result: { message: `Finding ${i}` } }));
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs }) })));
    await act(async () => { render(<DelegatedWork onInspectWorker={vi.fn()} />); });
    fireEvent.click(screen.getByRole("button", { name: "Show agents" }));
    expect(screen.queryByText("Finding 5")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "View all contributions (6)" }));
    expect(screen.getByText("Finding 5")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Show fewer contributions" }));
    expect(screen.queryByText("Finding 5")).not.toBeInTheDocument();
  });

  it("keeps completed contributions collapsed without hiding failures", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [{ ...job, state: "failed", error: { message: "Could not finish" } }] }) })));
    await act(async () => { render(<DelegatedWork />); });
    expect(screen.getByText("Review specialist update")).toBeVisible();
    expect(screen.getByRole("button", { name: "Show agents" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByText(/Narration · Stopped before completing/)).toBeVisible();
    expect(screen.queryByText("completed")).not.toBeInTheDocument();
  });
  it("shows a worker's choices and sends the selected answer to that worker", async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("fetch", fetch);
    render(<JobQuestion job={{ ...job, question: { question: "What is the Project UUID?", options: ["Paste the UUID", "Read it from the console"] } }} />);
    expect(screen.getByRole("textbox", { name: "Answer Narration" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Read it from the console" }));
    await waitFor(() => expect(fetch).toHaveBeenCalledWith("/api/jobs/a/message", expect.objectContaining({ body: expect.any(String) })));
    expect(await screen.findByText("Answer sent · continuing")).toBeInTheDocument();
  });
  it("keeps a typed answer if sending fails", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false }));
    render(<JobQuestion job={{ ...job, question: { question: "What is the Project UUID?" } }} />);
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "project-id" } });
    fireEvent.click(screen.getByRole("button", { name: "Send answer" }));
    await screen.findByRole("alert");
    expect(screen.getByRole("textbox")).toHaveValue("project-id");
  });
  it("shows progress in the conversation and polls while lifecycle revision stays unchanged", async () => {
    vi.useFakeTimers();
    const inspect = vi.fn();
    let label = "Comparing the supplied examples";
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [{ ...job, state: "running", objective: "Fill the Deepgram application", artifacts: [], error: {}, progress: { label: "Unknown vendor tool", activity: "tool_running", agent_update: { summary: label }, steps: 2, updated_at: Date.now() / 1000 } }] }) })));
    await act(async () => { render(<DelegatedWork onInspectWorker={inspect} />); });
    expect(screen.getByRole("region", { name: "Delegated work" })).toBeInTheDocument();
    expect(screen.getByText("Narration · Comparing the supplied examples")).toBeInTheDocument();
    label = "Checking the evidence against the criteria";
    await act(async () => { await vi.advanceTimersByTimeAsync(2100); });
    expect(screen.getByText("Narration · Checking the evidence against the criteria")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Show agents" }));
    fireEvent.click(screen.getByRole("button", { name: "View Narration details" }));
    expect(inspect).toHaveBeenCalledWith(expect.objectContaining({ session_id: "child" }));
    expect(openSession).not.toHaveBeenCalled();
  });
  it("shows unanswered questions in the main task view and keeps the coordinator separate", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [
      { ...job, question: { question: "Where did you hear about us?", options: ["Search", "Referral"] } },
      { ...job, id: "coordinator", label: "Continue parent task", session_id: "parent", state: "waiting_input", spec: { continuation: true }, question: { question: "Where did you hear about us?", options: ["Search", "Referral"] } },
    ] }) })));
    render(<DelegatedWork />);
    await screen.findByRole("button", { name: "Search" });
    expect(screen.getByRole("button", { name: "Show agents" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByText("1 specialist helping")).toBeInTheDocument();
    expect(screen.getByText("Kageha needs your input")).toBeInTheDocument();
    expect(screen.queryByText("Continue parent task")).not.toBeInTheDocument();
  });
  it("lets the main agent handle a worker question before asking the user", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [
      { ...job, question: { question: "Can I use the existing email?", options: ["Yes", "No"] } },
      { ...job, id: "coordinator", label: "Continue parent task", session_id: "parent", state: "running", spec: { continuation: true } },
    ] }) })));
    render(<DelegatedWork />);
    await screen.findByText("Kageha is reviewing Narration’s update.");
    expect(screen.queryByRole("button", { name: "Yes" })).not.toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });
  it("offers a way back to the parent from a worker conversation", async () => {
    state.sessionId = "child";
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [{ ...job, state: "running", parent_session: "parent" }] }) })));
    render(<DelegatedWork />);
    fireEvent.click(await screen.findByText("Back to main conversation"));
    expect(openSession).toHaveBeenCalledWith("parent");
  });
  it("shows actual state and supports child controls and artifact links", async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("fetch", fetch);
    render(<DurableJobs jobs={[job]} error="" />);
    expect(screen.getByText("0 of 1 jobs complete")).toBeInTheDocument();
    expect(screen.getByText("waiting input")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "audio.wav" })).toHaveAttribute("href", "/api/jobs/a/artifacts/0");
    fireEvent.click(screen.getByText("Open session")); expect(openSession).toHaveBeenCalledWith("child");
    fireEvent.click(screen.getByText("Approve once"));
    await waitFor(() => expect(fetch).toHaveBeenCalledWith("/api/jobs/a/approve", expect.objectContaining({ body: '{"approved":true}' })));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Keep the full script" } });
    fireEvent.click(screen.getByText("Send"));
    await waitFor(() => expect(fetch).toHaveBeenCalledWith("/api/jobs/a/message", expect.objectContaining({ body: '{"message":"Keep the full script"}' })));
    fireEvent.click(screen.getByText("Cancel"));
    await waitFor(() => expect(fetch).toHaveBeenCalledWith("/api/jobs/a/cancel", expect.anything()));
  });
  it("restores jobs from the durable API on reconnect", async () => {
    vi.stubGlobal("fetch", vi.fn(async (url: string) => ({ ok: true, json: async () => url.includes("/events") ? { cursor: 8, events: [{ sequence: 8 }] } : { jobs: [job] } })));
    function Panel() { const data = useDurableJobs("parent"); return <DurableJobs {...data} />; }
    const first = render(<Panel />); await screen.findByText("Narration"); first.unmount();
    render(<Panel />); await screen.findByText("Narration");
    expect(screen.getAllByText("Narration")).toHaveLength(1);
  });
});


it("preserves the background speaker and parent-relative file paths", async () => {
  const speaker = { id: "researcher", name: "Mira", avatar: "M" };
  const paths = ["_contexts/0123456789abcdef01234567/artifacts/report.md"];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [{ ...job,
    state: "succeeded", artifacts: ["/absolute/internal/report.md"],
    result: { job_reply_id: "reply", message: "Findings", speaker, chat_reply: { attachments: paths } },
  }] }) })));
  function Harness() { useDurableJobs("parent"); return null; }
  render(<Harness />);
  await waitFor(() => expect(receiveJobReply).toHaveBeenCalledWith("parent", "reply", "Findings", paths, speaker));
});

it("restores an unanswered question draft without carrying it into another question", () => {
  const first = render(<JobQuestion job={{ ...job, question: { question: "Which dataset?" } }} />);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Use evaluation-v2" } });
  first.unmount();
  const second = render(<JobQuestion job={{ ...job, question: { question: "Which dataset?" } }} />);
  expect(screen.getByRole("textbox")).toHaveValue("Use evaluation-v2");
  second.unmount();
  render(<JobQuestion job={{ ...job, question: { question: "Which model?" } }} />);
  expect(screen.getByRole("textbox")).toHaveValue("");
});

it("renders permission prompt with immediately visible action buttons", async () => {
  const fetchMock = vi.fn().mockResolvedValue({ ok: true });
  vi.stubGlobal("fetch", fetchMock);
  const permissionJob: DurableJob = {
    ...job,
    state: "waiting_input",
    error: {
      code: "approval_required",
      approval_id: "test-aid-123",
      message: "rm -f /tmp/test\necho done",
    },
    question: undefined,
  };
  render(<JobQuestion job={permissionJob} />);

  expect(screen.getByText("Permission needed for this operation")).toBeInTheDocument();
  const approveBtn = screen.getByRole("button", { name: "Approve once" });
  const sessionBtn = screen.getByRole("button", { name: "Approve for session" });
  const declineBtn = screen.getByRole("button", { name: "Decline" });
  expect(approveBtn).toBeInTheDocument();
  expect(sessionBtn).toBeInTheDocument();
  expect(declineBtn).toBeInTheDocument();

  // Test copy button presence
  const copyBtn = screen.getByRole("button", { name: "Copy command" });
  expect(copyBtn).toBeInTheDocument();

  fireEvent.click(approveBtn);
  await waitFor(() =>
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/jobs/a/approve",
      expect.objectContaining({
        body: JSON.stringify({ approved: true, approval_id: "test-aid-123" }),
      })
    )
  );
  expect(await screen.findByText("Decision recorded")).toBeInTheDocument();
});

it("supports approving permissions for the entire session", async () => {
  const fetchMock = vi.fn(async () => ({ ok: true }));
  vi.stubGlobal("fetch", fetchMock);
  const permissionJob: DurableJob = {
    ...job,
    state: "waiting_input",
    error: {
      code: "approval_required",
      approval_id: "test-aid-session",
      message: "Approval required for execute_command: npm test",
    },
    question: undefined,
  };
  render(<JobQuestion job={permissionJob} />);

  const sessionBtn = screen.getByRole("button", { name: "Approve for session" });
  fireEvent.click(sessionBtn);
  await waitFor(() =>
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/jobs/a/approve",
      expect.objectContaining({
        body: JSON.stringify({ approved: true, approval_id: "test-aid-session", scope: "session" }),
      })
    )
  );
  expect(await screen.findByText("Decision recorded")).toBeInTheDocument();
});

it("allows dismissing finished specialist contributions and remembers dismissal", async () => {
  const jobs = [{ ...job, id: "done-1", label: "Specialist 1", state: "succeeded", result: { message: "All done" } }];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs }) })));
  let unmount: () => void;
  await act(async () => {
    const res = render(<DelegatedWork />);
    unmount = res.unmount;
  });
  expect(await screen.findByRole("region", { name: "Delegated work" })).toBeInTheDocument();
  const dismissBtn = screen.getByRole("button", { name: "Dismiss specialist contributions" });
  await act(async () => {
    fireEvent.click(dismissBtn);
  });
  expect(screen.queryByRole("region", { name: "Delegated work" })).not.toBeInTheDocument();
  unmount!();

  // Re-rendering with same sessionId should remain dismissed
  await act(async () => {
    render(<DelegatedWork />);
  });
  expect(screen.queryByRole("region", { name: "Delegated work" })).not.toBeInTheDocument();
});

it("automatically un-dismisses when new active work arrives", async () => {
  const jobs = [{ ...job, id: "done-1", label: "Specialist 1", state: "succeeded" }];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs }) })));
  let unmount: () => void;
  await act(async () => {
    const res = render(<DelegatedWork />);
    unmount = res.unmount;
  });
  expect(await screen.findByRole("region", { name: "Delegated work" })).toBeInTheDocument();
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Dismiss specialist contributions" }));
  });
  expect(screen.queryByRole("region", { name: "Delegated work" })).not.toBeInTheDocument();
  unmount!();

  // Now a new active job arrives for the session
  const runningJobs = [
    { ...job, id: "done-1", label: "Specialist 1", state: "succeeded" },
    { ...job, id: "active-2", label: "Specialist 2", state: "running" },
  ];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: runningJobs }) })));
  await act(async () => {
    render(<DelegatedWork />);
  });
  expect(await screen.findByRole("region", { name: "Delegated work" })).toBeInTheDocument();
  expect(screen.getByText("1 specialist helping")).toBeInTheDocument();
});

it("keeps finished agents discoverable while other agents are working", async () => {
 vi.stubGlobal("fetch", vi.fn(async () => ({ ok:true, json:async()=>({jobs:[{...job,id:"one",label:"Research",state:"running"},{...job,id:"two",label:"Review",state:"running"},{...job,id:"three",label:"Checks",state:"succeeded"}]}) })));
 await act(async()=>{render(<DelegatedWork onInspectWorker={vi.fn()} />);});
 fireEvent.click(screen.getByRole("button",{name:"Show agents"}));
 const completed=screen.getByRole("button",{name:"Completed (1)"});fireEvent.click(completed);
 expect(completed).toHaveAttribute("aria-pressed","true");
 expect(screen.getByRole("button",{name:"View Checks’s contribution"})).toBeInTheDocument();
 expect(screen.queryByRole("button",{name:"View Research details"})).not.toBeInTheDocument();
});

it("summarizes parallel workers by their actual states and keeps failures visible", async () => {
 vi.stubGlobal("fetch", vi.fn(async () => ({ ok:true, json:async()=>({jobs:[
  {...job,id:"one",label:"Research",state:"running"},
  {...job,id:"two",label:"Review",state:"queued"},
  {...job,id:"three",label:"Checks",state:"failed",error:{message:"Checks failed"}},
 ]}) })));
 await act(async()=>{render(<DelegatedWork onInspectWorker={vi.fn()} />);});
 expect(screen.getByText("Research · Working • Review · Queued")).toBeVisible();
 fireEvent.click(screen.getByRole("button",{name:"Show agents"}));
 expect(screen.getByRole("button",{name:"View Research details"})).toBeVisible();
 expect(screen.getByRole("button",{name:"View Review details"})).toBeVisible();
 expect(screen.getByRole("button",{name:"View Checks’s contribution"})).toBeVisible();
});

it("shows live agent progress while the details remain collapsed", async () => {
 const inspect = vi.fn();
 vi.stubGlobal("fetch", vi.fn(async () => ({ ok:true, json:async()=>({jobs:[
  {...job,id:"one",label:"Long assignment",title:"Implementation",state:"running",progress:{label:"running",steps:2,updated_at:1,current_tool:{id:"tool",name:"write_file",status:"running",label:"write file"}}},
  {...job,id:"two",label:"Checks",state:"failed",result:{failure_detail:"Model response timed out"}},
 ]}) })));
 await act(async()=>{render(<DelegatedWork onInspectWorker={inspect} />);});
 expect(screen.getByRole("button",{name:"Show agents"})).toHaveAttribute("aria-expanded","false");
 const progress = screen.getByRole("list",{name:"Agent progress"});
 expect(progress).toHaveTextContent("Implementation · Working");
 expect(progress).toHaveTextContent("Using write file");
 expect(progress).toHaveTextContent("Checks · Failed");
 expect(progress).toHaveTextContent("Model response timed out");
 fireEvent.click(screen.getByRole("button",{name:"Inspect Implementation: Working"}));
 expect(inspect).toHaveBeenCalledWith(expect.objectContaining({id:"one"}));
});

it("keeps a conflicting handoff visible after the worker succeeds", async () => {
 const conflict = {...job,state:"succeeded",handoff:{preview_id:"p",target:"project",status:"conflict",files:[],conflicts:["src/file.py"]}};
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>({jobs:[conflict]})})));
 await act(async()=>{render(<DelegatedWork onInspectWorker={vi.fn()} />);});
 expect(screen.getByText("Review specialist update")).toBeVisible();
 expect(screen.getByText(/Changes conflict with the destination/)).toBeVisible();
 fireEvent.click(screen.getByRole("button",{name:"Show agents"}));
 expect(screen.getByRole("region",{name:"Code handoff"})).toHaveTextContent("src/file.py");
});

it("shows the exact operation before an approval decision", () => { render(<JobQuestion job={{...job,error:{approval_id:"exact-approval"},question:{question:"Use MCP tool guarded on server approval-fixture"}}} />);expect(screen.getByText("Use MCP tool guarded on server approval-fixture")).toBeVisible(); });

it("allows minimizing and expanding the active agent dock", async () => {
  const jobs = [
    { ...job, id: "w1", label: "Worker 1", state: "running", progress: { label: "running", steps: 1, updated_at: 1 } },
    { ...job, id: "w2", label: "Worker 2", state: "running", progress: { label: "running", steps: 2, updated_at: 1 } },
  ];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs }) })));
  await act(async () => { render(<DelegatedWork />); });

  expect(screen.getByRole("list", { name: "Agent progress" })).toBeInTheDocument();
  const minBtn = screen.getByRole("button", { name: "Minimize agent dock" });
  expect(minBtn).toBeInTheDocument();

  // Minimize
  await act(async () => { fireEvent.click(minBtn); });
  expect(screen.queryByRole("list", { name: "Agent progress" })).not.toBeInTheDocument();
  const expandBtn = screen.getByRole("button", { name: "Expand agent dock" });
  expect(expandBtn).toBeInTheDocument();

  // Expand back
  await act(async () => { fireEvent.click(expandBtn); });
  expect(screen.getByRole("list", { name: "Agent progress" })).toBeInTheDocument();
});

it("allows minimizing the active dock from the list footer button", async () => {
  const jobs = [
    { ...job, id: "w1", label: "Worker 1", state: "running" },
    { ...job, id: "w2", label: "Worker 2", state: "running" },
  ];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs }) })));
  await act(async () => { render(<DelegatedWork />); });

  expect(screen.getByRole("list", { name: "Agent progress" })).toBeInTheDocument();
  const listMinBtn = screen.getByRole("button", { name: "Minimize" });
  await act(async () => { fireEvent.click(listMinBtn); });
  expect(screen.queryByRole("list", { name: "Agent progress" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Expand agent dock" })).toBeInTheDocument();
});

it("un-minimizes when clicking the main header button", async () => {
  const jobs = [
    { ...job, id: "w1", label: "Worker 1", state: "running" },
    { ...job, id: "w2", label: "Worker 2", state: "running" },
  ];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs }) })));
  await act(async () => { render(<DelegatedWork />); });

  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Minimize agent dock" })); });
  expect(screen.queryByRole("list", { name: "Agent progress" })).not.toBeInTheDocument();

  // Click the header "Show agents" button to restore
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Show agents" })); });
  expect(screen.getByRole("list", { name: "Agent progress" })).toBeInTheDocument();
});

it("automatically un-minimizes the dock when an agent requires user input", async () => {
  const runningJobs = [
    { ...job, id: "w1", label: "Worker 1", state: "running" },
  ];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: runningJobs }) })));
  let unmount: () => void;
  await act(async () => {
    const res = render(<DelegatedWork />);
    unmount = res.unmount;
  });

  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Minimize agent dock" })); });
  expect(screen.queryByRole("list", { name: "Agent progress" })).not.toBeInTheDocument();
  unmount!();

  // Now a job needs user input
  const questionJobs = [
    { ...job, id: "w1", label: "Worker 1", state: "waiting_input", question: { question: "Choose deployment target?", options: ["Staging", "Prod"] } },
  ];
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: questionJobs }) })));
  await act(async () => {
    render(<DelegatedWork />);
  });

  expect(screen.getByText("Worker 1 needs your input")).toBeInTheDocument();
  expect(screen.getByText("Choose deployment target?")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Minimize agent dock" })).toBeInTheDocument();
});
