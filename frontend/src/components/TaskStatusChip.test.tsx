import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useAppStore } from "../store";
import { TaskStatusChip } from "./TaskStatusChip";

const original = useAppStore.getState();

function reset() {
  useAppStore.setState({
    taskControl: original.taskControl,
    taskControlReason: original.taskControlReason,
    taskFailureKind: original.taskFailureKind,
    taskFailureChange: original.taskFailureChange,
    taskFailureCount: original.taskFailureCount,
    taskStageId: original.taskStageId,
    taskStageStatus: original.taskStageStatus,
    taskChronicTool: original.taskChronicTool,
    taskChronicCount: original.taskChronicCount,
    taskStatusPanelOpen: original.taskStatusPanelOpen,
    runStatus: original.runStatus,
  });
}

describe("TaskStatusChip", () => {
  afterEach(() => {
    cleanup();
    reset();
  });

  it("renders nothing when the loop is simply continuing", () => {
    useAppStore.setState({ taskControl: "continue" });
    render(<TaskStatusChip />);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("renders nothing once a run has settled to stop_success and gone idle", () => {
    useAppStore.setState({ taskControl: "stop_success", runStatus: "idle" });
    render(<TaskStatusChip />);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("surfaces a blocked control state (huddle) with its reason as the tooltip", () => {
    useAppStore.setState({
      taskControl: "huddle",
      taskControlReason: "tool_error/access_blocked failed twice",
      taskFailureCount: 2,
    });
    render(<TaskStatusChip />);
    const button = screen.getByRole("button", { name: /Diagnosing a blocker/ });
    expect(button).toHaveAttribute("title", "tool_error/access_blocked failed twice");
    expect(screen.getByText("×2")).toBeInTheDocument();
  });

  it("opens the detail popover on click and shows the classified failure", () => {
    useAppStore.setState({
      taskControl: "huddle",
      taskControlReason: "tool_error/access_blocked failed twice",
      taskFailureKind: "access_blocked",
      taskFailureChange: "Use an available logged-in browser; otherwise ask the user to sign in",
    });
    render(<TaskStatusChip />);
    fireEvent.click(screen.getByRole("button", { name: /Diagnosing a blocker/ }));
    expect(screen.getByRole("dialog", { name: "Task status detail" })).toBeInTheDocument();
    expect(screen.getByText("Access blocked")).toBeInTheDocument();
    expect(
      screen.getByText(/Use an available logged-in browser/),
    ).toBeInTheDocument();
  });

  it("closes the popover on Escape", () => {
    useAppStore.setState({ taskControl: "repair", taskControlReason: "2 critical defects" });
    render(<TaskStatusChip />);
    fireEvent.click(screen.getByRole("button", { name: /Repairing/ }));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("compact mode (mobile Tools sheet) is a read-only indicator with no popover", () => {
    useAppStore.setState({ taskControl: "huddle", taskControlReason: "stuck" });
    render(<TaskStatusChip compact />);
    const button = screen.getByRole("button", { name: "Status" });
    expect(button).not.toHaveAttribute("aria-expanded");
    fireEvent.click(button);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("shows a distinct 'Stuck on <tool>' chip when the same tool has halted 2+ turns in a row", () => {
    useAppStore.setState({
      taskControl: "switch_tool",
      taskControlReason: "guardrail halted browser_evaluate",
      taskChronicTool: "browser_evaluate",
      taskChronicCount: 3,
    });
    render(<TaskStatusChip />);
    const button = screen.getByRole("button", { name: /Stuck on browser_evaluate/ });
    expect(button).toHaveAttribute(
      "title",
      "browser_evaluate has halted 3 turns in a row — same class of failure repeating",
    );
    expect(screen.getByText("×3")).toBeInTheDocument();
  });

  it("a single (non-chronic) halt still shows the ordinary control label, not 'Stuck on'", () => {
    useAppStore.setState({
      taskControl: "switch_tool",
      taskChronicTool: "browser_evaluate",
      taskChronicCount: 1,
    });
    render(<TaskStatusChip />);
    expect(screen.getByRole("button", { name: /Switching approach/ })).toBeInTheDocument();
    expect(screen.queryByText(/Stuck on/)).toBeNull();
  });

  it("clears the chronic banner once the loop settles back to continue", () => {
    useAppStore.setState({
      taskControl: "continue",
      taskChronicTool: "browser_evaluate",
      taskChronicCount: 3,
    });
    render(<TaskStatusChip />);
    // The chip itself is hidden for "continue", and the stale chronic state
    // must not linger for the next control event either.
    expect(screen.queryByRole("button")).toBeNull();
    expect(useAppStore.getState().taskChronicTool).toBe("");
    expect(useAppStore.getState().taskChronicCount).toBe(0);
  });
});
