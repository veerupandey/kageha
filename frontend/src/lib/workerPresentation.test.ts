import { describe, expect, it } from "vitest";
import type { DurableJob } from "../components/DurableJobs";
import { workerActivity, workerEnvironment, workerName, workerStatus } from "./workerPresentation";
const job: DurableJob = { id: "agent", kind: "agent", label: "Evidence reviewer", state: "running", session_id: "child", created_at: 1, started_at: 1, revision: 1, can_cancel: true, attachable: true, artifacts: [], error: {}, result: {} };
describe("recorded agent presentation", () => {
  it.each(["browser_fill", "Running web_fetch", "computer_click", "shell_write_file"])("does not infer intent or resources from %s", label => {
    const legacy = { ...job, progress: { label, steps: 1, updated_at: 1 } };
    expect(workerActivity(legacy)).toBe("Working on the assigned task");
    expect(workerEnvironment(legacy)).toBe("Own context · tools available when needed");
  });
  it("uses model-authored progress while displaying recorded retry state first", () => {
    const progress = { label: "Recorded event", steps: 1, updated_at: 1, agent_update: { summary: "Comparing the supplied evidence" } };
    expect(workerActivity({ ...job, progress })).toBe("Comparing the supplied evidence");
    const retry = { ...job, progress: { ...progress, activity: "retry_wait", summary: "Waiting to retry the model" } };
    expect(workerActivity(retry)).toBe("Waiting before trying again");
    expect(workerStatus(retry)).toBe("Retrying");
  });
  it("does not claim a queued tool has executed", () => {
    const progress = { label: "Tool", steps: 1, updated_at: 1, current_tool: { id: "call", name: "browser_click", status: "pending" } };
    expect(workerActivity({ ...job, progress })).toBe("Preparing browser click");
  });
  it("does not confuse a heartbeat with task progress or verified completion", () => {
    expect(workerStatus({ ...job, health: { worker: "lease_expired", observed_at: 2 } })).toBe("Connection interrupted");
    expect(workerStatus({ ...job, state: "succeeded", result: { message: "Verified!" } })).toBe("Finished");
    expect(workerStatus({ ...job, state: "succeeded", verification: { status: "verified" } })).toBe("Checks passed");
  });
  it("keeps the supplied agent identity without interpreting its text", () => {
    const label = "Research · stage (two) https://example.test a deliberately long identity that remains intact";
    expect(workerName({ ...job, label })).toBe(label);
  });
});
