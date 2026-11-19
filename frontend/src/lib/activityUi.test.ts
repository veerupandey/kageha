import { describe, expect, it } from "vitest";
import {
  activityTarget,
  activityUrl,
  friendlyActivityLabel,
  friendlyPlanStepText,
  isBrowserToolName,
  isMachineShellToolName,
  isTimelineMilestone,
  isTraceStep,
  wantsLiveBrowser,
  wantsLiveMachine,
} from "./activityUi";

describe("activity navigation", () => {
  it("routes browser and computer activity", () => {
    expect(activityTarget("browser_navigate Opening page…")).toBe("browser");
    expect(activityTarget("browser_open url=https://example.com")).toBe("browser");
    expect(activityTarget("Opening page…")).toBe("browser");
    expect(activityTarget("bash apt-get update")).toBe("machine");
    expect(activityTarget("install_python_packages requests")).toBe("machine");
    expect(activityTarget("computer_click Clicking…")).toBe("computer");
    expect(activityTarget("Loaded skill: computer_use")).toBeNull();
    expect(activityTarget("machine_ready Linux computer ready")).toBe("machine");
    expect(activityTarget("machine_starting Starting computer")).toBe("machine");
    expect(activityTarget("/machine install deps")).toBe("machine");
    expect(activityTarget("task machine blocked")).toBe("machine");
    expect(activityTarget("Reading file…")).toBeNull();
    expect(activityTarget("web_search latest news")).toBeNull();
  });

  it("does not treat ordinary coding or URL research as machine", () => {
    expect(activityTarget("Open https://example.com and summarize it")).toBeNull();
    expect(activityTarget("run tests")).toBeNull();
    expect(activityTarget("edit the repo")).toBeNull();
  });

  it("detects owned-browser tools and composer intent", () => {
    expect(isBrowserToolName("browser_open")).toBe(true);
    expect(isBrowserToolName("browser_click")).toBe(true);
    expect(isBrowserToolName("web_fetch")).toBe(false);
    expect(isMachineShellToolName("bash")).toBe(true);
    expect(isMachineShellToolName("browser_open")).toBe(false);
    expect(wantsLiveBrowser("browse to https://example.com and click Learn more")).toBe(true);
    expect(wantsLiveBrowser("research the latest news about matcha")).toBe(false);
  });

  it("opens Live for isolated-desktop asks without /machine", () => {
    expect(wantsLiveMachine("Start a Linux desktop and control it in the right panel")).toBe(true);
    expect(wantsLiveMachine("Use the isolated desktop as your main computer")).toBe(true);
    expect(wantsLiveMachine("/machine open firefox")).toBe(true);
    expect(wantsLiveMachine("/machine status")).toBe(false);
    expect(wantsLiveMachine("run tests")).toBe(false);
    expect(wantsLiveMachine("what is a linux desktop")).toBe(false);
  });

  it("extracts a page URL from tool arguments", () => {
    expect(activityUrl('{"url":"https://example.com/path"}')).toBe(
      "https://example.com/path",
    );
  });

  it("keeps trace rows for prompt cache, todos, and verification", () => {
    expect(isTraceStep({ label: "Prompt cache 85%", kind: "model" })).toBe(true);
    expect(isTraceStep({ label: "Todos 2/5", kind: "todo_board" })).toBe(true);
    expect(isTraceStep({ label: "Checked · pass", kind: "verification" })).toBe(true);
    expect(isTraceStep({ label: "Thinking…", kind: "thinking_started" })).toBe(false);
    expect(isTraceStep({ label: "Searching code…", kind: "tool_started" })).toBe(false);
  });

  it("keeps only relevant timeline milestones", () => {
    expect(isTimelineMilestone({ label: "Thinking…", kind: "thinking_started" })).toBe(false);
    expect(isTimelineMilestone({ label: "Using zai/glm-5.3", kind: "model_selected" })).toBe(false);
    expect(isTimelineMilestone({ label: "Planning (plan)…", kind: "planning_started" })).toBe(false);
    expect(isTimelineMilestone({ label: "Accepted", kind: "accepted" })).toBe(false);
    expect(isTimelineMilestone({ label: "Searching code…", kind: "tool_started" })).toBe(false);
    expect(isTimelineMilestone({ label: "Plan ready · 3 steps", kind: "planned" })).toBe(true);
    expect(isTimelineMilestone({ label: "Waiting for approval…", kind: "approval_required" })).toBe(
      true,
    );
  });

  it("does not keep the chat chip on Planning after the plan is ready", () => {
    expect(friendlyActivityLabel("Planning…")).toBe("Planning…");
    expect(friendlyActivityLabel("Working…")).toBe("Working…");
    expect(friendlyActivityLabel("Still working…")).toBe("Still working…");
    expect(friendlyActivityLabel("Plan ready · 6 steps")).toBe("Plan ready · 6 steps");
    expect(friendlyActivityLabel("[kageha] plan ready (llm, 6 plan items)")).toBe("Starting…");
  });

  it("shows the task instead of executor instructions in plan steps", () => {
    expect(friendlyPlanStepText(
      "Do exactly this follow-up with the minimum tools, answer in chat " +
      "(no .md file unless the user asked to save one), then stop: " +
      "you can start your own browser",
    )).toBe("you can start your own browser");
    expect(friendlyPlanStepText(
      "Execute this single action with the minimum tools needed, then report the result in chat: open the browser",
    )).toBe("open the browser");
    expect(friendlyPlanStepText("Run the frontend tests")).toBe("Run the frontend tests");
  });
});


  it("surfaces context pressure notices instead of treating them as noise", () => {
    // These must reach the user watching the run, not disappear into
    // "Working…" the way checkpoint/thinking noise does — the whole point
    // of a context reminder is to be seen.
    expect(
      isTimelineMilestone({ label: "Context 82% full", kind: "context_reminder" }),
    ).toBe(true);
    expect(
      isTraceStep({ label: "Context 82% full", kind: "context_reminder" }),
    ).toBe(true);
    expect(
      isTimelineMilestone({ label: "Compacting 3 tool result(s)…", kind: "context_compact" }),
    ).toBe(true);
    expect(
      isTraceStep({ label: "Compacting 3 tool result(s)…", kind: "context_compact" }),
    ).toBe(true);
  });
