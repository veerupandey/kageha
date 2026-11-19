import type { TranscriptBlock } from "../../api/types";
import "@testing-library/jest-dom/vitest";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { Transcript } from "./Transcript";

describe("Transcript live status", () => {
  it("formats completed progress updates while the turn is still running", () => {
    render(
      <Transcript
        blocks={[{
          type: "progress",
          id: "progress-1",
          text: "**Still needed**\n\n| Item | Status |\n| --- | --- |\n| Job posting | Missing |",
        }]}
        streaming
        messageId="progress-message"
      />,
    );
    expect(screen.getByText("Still needed").tagName).toBe("STRONG");
    expect(screen.getByRole("table")).toHaveTextContent("Job posting");
  });

  it("keeps the live pulse visible while streaming", () => {
    render(
      <Transcript
        blocks={[]}
        streaming
        liveLabel="Still working…"
        messageId="a1"
      />,
    );
    expect(screen.getByText("Still working…")).toBeInTheDocument();
  });

  it("shows a live row even before transcript blocks exist", () => {
    render(
      <Transcript blocks={[]} streaming liveLabel="Working…" messageId="a1" />,
    );
    expect(screen.getByText("Working…")).toBeInTheDocument();
  });

  it("opens live reasoning and lets the reader collapse it", () => {
    render(
      <Transcript
        blocks={[
          {
            type: "reasoning",
            id: "reason-1",
            streaming: true,
            text: "I will open the rate page and read the postage.",
          },
        ]}
        streaming
        liveLabel="Working…"
        messageId="a1"
      />,
    );
    expect(screen.getByText("Thinking…")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
    expect(screen.getByText("I will open the rate page and read the postage.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
    expect(screen.queryByText("I will open the rate page and read the postage.")).not.toBeInTheDocument();
    expect(screen.queryByText("Working…")).not.toBeInTheDocument();
  });

  it("shows elapsed heartbeat detail while the thought chip is still empty", () => {
    render(
      <Transcript
        blocks={[{ type: "reasoning", id: "reason-1", streaming: true }]}
        streaming
        liveLabel="Thinking…"
        liveDetail="Elapsed: 32s"
        messageId="a1"
      />,
    );
    expect(screen.getByText("Waiting for model… · Elapsed: 32s")).toBeInTheDocument();
    expect(screen.queryByText("Waiting for model tokens…")).not.toBeInTheDocument();
  });

  it("shows action-specific live status while thought text is streaming", () => {
    render(
      <Transcript
        blocks={[
          {
            type: "reasoning",
            id: "reason-1",
            streaming: true,
            text: "Need to surface capture_redirect_evidence via tool_search.",
          },
        ]}
        streaming
        liveLabel="Searching tools"
        messageId="a1"
      />,
    );
    expect(screen.getAllByRole("button")).toHaveLength(1);
    expect(screen.getByText("Searching tools")).toBeVisible();
    expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
    expect(screen.getByTestId("reasoning-body")).toHaveTextContent("Need to surface capture_redirect_evidence");
  });

  it("keeps live thinking scrolled to the latest tokens", () => {
    const { rerender } = render(
      <Transcript
        blocks={[
          {
            type: "reasoning",
            id: "reason-1",
            streaming: true,
            text: "start",
          },
        ]}
        streaming
        messageId="a1"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
    const body = screen.getByTestId("reasoning-body");
    Object.defineProperty(body, "scrollHeight", { configurable: true, value: 400 });
    Object.defineProperty(body, "clientHeight", { configurable: true, value: 192 });
    rerender(
      <Transcript
        blocks={[
          {
            type: "reasoning",
            id: "reason-1",
            streaming: true,
            text: "start\nmore thinking about the page\nand then the next step",
          },
        ]}
        streaming
        messageId="a1"
      />,
    );
    expect(body.scrollTop).toBe(400);
  });

  it("collapses finished thinking behind Thought for", () => {
    render(
      <Transcript
        blocks={[
          {
            type: "reasoning",
            id: "reason-1",
            streaming: false,
            durationMs: 6200,
            text: "I will open the rate page and read the postage.",
          },
        ]}
        messageId="a1"
      />,
    );
    expect(screen.getByText("Thought for 6.2s")).toBeInTheDocument();
    expect(screen.queryByText("I will open the rate page and read the postage.")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("Thought for 6.2s"));
    expect(screen.getByTestId("reasoning-body")).toHaveTextContent(
      "I will open the rate page and read the postage.",
    );
  });

  it("renders a collapsible Running command row with a summary", () => {
    render(
      <Transcript
        blocks={[
          {
            type: "shell",
            id: "sh-1",
            card: {
              id: "sh-1",
              name: "bash",
              status: "running",
              argsPreview:
                'command=uv run ruff check ., description=Fix both lint errors and verify',
            },
          },
        ]}
        streaming
        messageId="a1"
      />,
    );
    expect(screen.getByText("Running command")).toBeInTheDocument();
    expect(screen.getByText("Fix both lint errors and verify")).toBeInTheDocument();
    expect(screen.queryByText("$ uv run ruff check .")).not.toBeInTheDocument();
  });

  it("does not render run telemetry status blocks in chat", () => {
    const { container } = render(
      <Transcript
        blocks={[
          {
            type: "status",
            id: "cache-1",
            label: "Prompt cache 85%",
            detail: ["cache_read=1020/1200 (85%)"],
          },
          {
            type: "status",
            id: "todo-1",
            label: "Todos 2/5",
            detail: ["[ ] inspect page"],
          },
        ]}
        messageId="a1"
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders completed browser tools with a neutral label and completion status", () => {
    render(
      <Transcript
        blocks={[
          {
            type: "tool",
            id: "br-1",
            card: {
              id: "br-1",
              name: "browser_navigate",
              status: "ok",
              durationMs: 70,
              argsPreview: 'url="https://www.canadapost-postescanada.ca/cpc/en/tools/find-a-rate.page"',
            },
          },
        ]}
        messageId="a1"
      />,
    );
    expect(screen.queryByText("Browser")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("Opened page canadapost-postescanada.ca", { exact: false }).closest("summary")!);
    expect(screen.getByText("Browser")).toBeInTheDocument();
    expect(screen.getByText("Done")).toBeInTheDocument();
    expect(screen.getByText("Opened page canadapost-postescanada.ca", { selector: "summary span" })).toBeInTheDocument();
    expect(screen.queryByText("Using zai/glm-5.3")).not.toBeInTheDocument();
  });
});


it("allows an empty live activity row to explain missing provider reasoning", () => {
  render(<Transcript blocks={[{type: "reasoning", id: "empty", streaming: true}]} streaming messageId="empty-test" />);
  const toggle = screen.getByRole("button", {name: "Waiting for model…"});
  expect(toggle).toHaveAttribute("aria-expanded", "false");
  fireEvent.click(toggle);
  expect(toggle).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByText(/No reasoning text received yet/)).toBeInTheDocument();
  fireEvent.click(toggle);
  expect(screen.queryByText(/No reasoning text received yet/)).not.toBeInTheDocument();
});

it("renders subagents breakdown for spawn_subagents tool", () => {
  render(
    <Transcript
      blocks={[
        {
          type: "tool",
          id: "sub-1",
          card: {
            id: "sub-1",
            name: "spawn_subagents",
            status: "running",
            argsPreview:
              'tasks_json=[{"id": "structure", "task": "Research account structure"}, {"id": "retargeting", "task": "Research retargeting"}]',
          },
        },
      ]}
      messageId="sub-test"
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /Running subagents/ }));
  expect(screen.getByText("Subagents (2)")).toBeInTheDocument();
  expect(screen.getByText("Worker 1")).toBeInTheDocument();
  expect(screen.getByText("Research account structure")).toBeInTheDocument();
  expect(screen.getByText("Worker 2")).toBeInTheDocument();
  expect(screen.getByText("Research retargeting")).toBeInTheDocument();
});

it("renders a single specialist without exposing raw inputs by default", () => {
  render(<Transcript messageId="single-specialist" blocks={[{ type: "tool", id: "single",
    card: { id: "single", name: "spawn_subagent", status: "running", argsPreview: "clipped JSON",
      subagentGraph: [{ id: "t1", name: "Core architect", task: "Review the source report" }] } }]} />);
  const button = screen.getByRole("button", { name: /1 specialist.*Running/ });
  expect(button).toHaveTextContent("Core architect");
  fireEvent.click(button);
  expect(screen.getByText("Review the source report")).toBeVisible();
  expect(screen.getByText("Technical details")).toBeVisible();
});

it("labels durable submission as delegated rather than completed", () => {
  render(<Transcript messageId="accepted-specialist" blocks={[{ type: "tool", id: "accepted",
    card: { id: "accepted", name: "spawn_subagent", status: "success",
      subagentGraph: [{ id: "t1", job_id: "job1", name: "Reviewer", task: "Check sources", status: "queued" }] } }]} />);
  expect(screen.getByRole("button", { name: /1 specialist.*Delegated/ })).toHaveTextContent("Reviewer");
});

it("keeps an accepted agent_spawn visible as delegated work", () => {
  render(<Transcript messageId="spawn-specialist" blocks={[{ type: "tool", id: "spawn",
    card: { id: "spawn", name: "agent_spawn", status: "done",
      subagentGraph: [{ id: "job-123", job_id: "job-123", name: "Reviewer", task: "Review the code", status: "queued" }] } }]} />);
  const button = screen.getByRole("button", { name: /1 specialist.*Delegated/ });
  expect(button).toHaveTextContent("Reviewer");
  fireEvent.click(button);
  expect(screen.getByText("Review the code")).toBeVisible();
});

it("keeps every part of a completed reply visible", () => {
  const { container } = render(<Transcript messageId="finished" blocks={[
    { type: "narrative", id: "draft", text: "Earlier attempt" },
    { type: "narrative", id: "answer", text: "Latest answer" },
  ]} />);
  expect(screen.queryByText("Work history")).not.toBeInTheDocument();
  // Adjacent narrative blocks are one text stream and render as one document.
  expect(container.textContent).toContain("Earlier attempt");
  expect(container.textContent).toContain("Latest answer");
  expect(container.querySelectorAll("details")).toHaveLength(0);
});

it("keeps progress replies visible while the task is running", () => {
  const { container } = render(<Transcript messageId="live-progress" streaming blocks={[
    { type: "narrative", id: "first", text: "First progress update" },
    { type: "narrative", id: "second", text: " and a second progress update" },
  ]} />);
  expect(container.textContent).toContain("First progress update and a second progress update");
  expect(screen.queryByText("Work history")).not.toBeInTheDocument();
});


it("combines consecutive thoughts with their total duration and full text", () => {
  render(<Transcript messageId="grouped" blocks={[
    { type: "reasoning", id: "one", text: "First thought", durationMs: 1000 },
    { type: "reasoning", id: "two", text: "Second thought", durationMs: 2000 },
    { type: "reasoning", id: "three", text: "Third thought", durationMs: 1000 },
  ]} />);
  const toggle = screen.getByRole("button", { name: "Thought for 4s" });
  expect(screen.getAllByRole("button")).toHaveLength(1);
  fireEvent.click(toggle);
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("First thought Second thought Third thought");
});

it("groups related completed actions across thoughts while preserving detail order", () => {
  const { container } = render(<Transcript messageId="timeline" blocks={[
    { type: "reasoning", id: "r1", text: "Before the first page" },
    { type: "tool", id: "a", card: { id: "a", name: "browser_navigate", status: "ok", argsPreview: "url=https://example.com" } },
    { type: "reasoning", id: "r2", text: "Before the second page" },
    { type: "tool", id: "b", card: { id: "b", name: "browser_inspect", status: "ok", argsPreview: "url=https://example.com" } },
    { type: "narrative", id: "reply", text: "Progress update" },
    { type: "tool", id: "c", card: { id: "c", name: "browser_navigate", status: "ok" } },
  ]} />);
  const summaries = container.querySelectorAll(".action-summary-card > summary");
  expect(summaries).toHaveLength(2);
  expect(summaries[0]).toHaveTextContent("2 actions");
  expect(screen.queryByRole("button", { name: /Thought/ })).not.toBeInTheDocument();
  fireEvent.click(summaries[0]);
  const detail = summaries[0].parentElement!;
  expect(detail.textContent!.indexOf("Before the first page")).toBeLessThan(detail.textContent!.indexOf("Before the second page"));
  expect(screen.getByText("Progress update")).toBeInTheDocument();
});

it("keeps different actions and failures separate and shows the failure reason", () => {
  const { container } = render(<Transcript messageId="failure" blocks={[
    { type: "tool", id: "a", card: { id: "a", name: "browser_inspect", status: "ok" } },
    { type: "reasoning", id: "r", text: "Try email next" },
    { type: "tool", id: "b", card: { id: "b", name: "file_read", status: "ok" } },
    { type: "tool", id: "c", card: { id: "c", name: "browser_navigate", status: "failed", errorPreview: "Page could not be loaded" } },
    { type: "tool", id: "d", card: { id: "d", name: "browser_inspect", status: "ok" } },
  ]} />);
  expect(container.querySelectorAll(".action-summary-card")).toHaveLength(3);
  expect(screen.queryByText("Page could not be loaded")).not.toBeInTheDocument();
  const failure = screen.getByRole("button", { name: /Failed/ });
  expect(failure).toHaveAttribute("aria-expanded", "false");
  fireEvent.click(failure);
  expect(failure).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByLabelText("Recorded failure preview")).toHaveTextContent("Page could not be loaded");
  expect(screen.getAllByText("Page could not be loaded")).toHaveLength(1);
  expect(screen.queryByText("Recovered")).not.toBeInTheDocument();
});

it("shows a compact failed browser row with its reason and an expandable browser link", () => {
  const openBrowser = vi.fn();
  const target = { browserId: "b1", tabId: "t1" };
  const { container } = render(<Transcript messageId="compact-failure" onOpenBrowser={openBrowser} blocks={[
    { type: "tool", id: "good", card: { id: "good", name: "browser_inspect", status: "done" } },
    { type: "tool", id: "bad", card: { id: "bad", name: "browser_select", status: "failed", browserTarget: target,
      resultPreview: 'Tool failed: {"error_code":"invalid_option","summary":"The option is unavailable."}' } },
  ]} />);
  expect(container.querySelectorAll(".action-summary-card")).toHaveLength(1);
  const failure = container.querySelector(".ka-tool-call--compact")!;
  expect(failure).toHaveAttribute("data-status", "failed");
  expect(failure).toHaveTextContent("Failed");
  expect(failure).not.toHaveTextContent("The option is unavailable.");
  expect(within(failure as HTMLElement).queryByRole("button", { name: /Open browser/ })).not.toBeInTheDocument();
  fireEvent.click(within(failure as HTMLElement).getByRole("button", { name: /Browser.*Failed/ }));
  expect(failure).toHaveTextContent("The option is unavailable.");
  fireEvent.click(within(failure as HTMLElement).getByRole("button", { name: /Open browser/ }));
  expect(openBrowser).toHaveBeenCalledWith(undefined, target);
  expect(screen.getByLabelText("Recorded failure preview")).toHaveTextContent("invalid_option");
});

it("keeps blocked connector recovery visible outside the compact row disclosure", () => {
  render(<Transcript messageId="blocked" blocks={[
    { type: "tool", id: "b", card: { id: "b", name: "integration_read", status: "blocked", connectorOutcome: {
      connector: "gmail", state: "disconnected", operation: "search", recovery: "manage_access", revision: 3, showRecovery: true,
    } } },
  ]} />);
  expect(screen.getByText("Blocked").closest(".ka-tool-call--compact")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Manage Gmail access" })).toBeVisible();
});

it.each(["failed", "denied"])("uses a compact %s row for commands without marking them successful", (status) => {
  const { container } = render(<Transcript messageId="failed-command" blocks={[
    { type: "shell", id: "s", card: { id: "s", name: "shell", status, errorPreview: "Command failed" } },
  ]} />);
  expect(container.querySelector(".ka-tool-call--compact")).toHaveAttribute("data-status", "failed");
  expect(screen.getByText("Failed")).toBeVisible();
  expect(screen.getByText("Command failed")).toBeVisible();
  expect(container.querySelector(".action-summary-card")).toBeNull();
});

it("does not group a completed command with an error preview as a success", () => {
  const { container } = render(<Transcript messageId="command-error-preview" blocks={[
    { type: "shell", id: "s", card: { id: "s", name: "shell", status: "completed", errorPreview: "Command exited unsuccessfully" } },
  ]} />);
  const failure = container.querySelector(".ka-tool-call--compact")!;
  expect(failure).toHaveAttribute("data-status", "failed");
  expect(container.querySelector(".action-summary-card")).toBeNull();
  fireEvent.click(within(failure as HTMLElement).getByRole("button"));
  expect(screen.getByLabelText("Recorded failure preview")).toHaveTextContent("Command exited unsuccessfully");
});

it("keeps thought groups separated by a reply and preserves an open live group", () => {
  const blocks: TranscriptBlock[] = [
    { type: "reasoning", id: "one", text: "Earlier thinking", durationMs: 1000 },
    { type: "narrative", id: "reply", text: "Progress update" },
    { type: "reasoning", id: "two", text: "Current thinking", streaming: true },
  ];
  const { rerender } = render(<Transcript messageId="live-group" streaming blocks={blocks} />);
  fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
  rerender(<Transcript messageId="live-group" streaming blocks={[
    ...blocks,
    { type: "reasoning", id: "three", text: "More thinking", streaming: true },
  ]} />);
  expect(screen.getAllByRole("button")).toHaveLength(2);
  expect(screen.getByRole("button", { name: "Thinking…" })).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("Current thinking More thinking");
  expect(screen.getByText("Progress update")).toBeInTheDocument();
});

it("renders one Markdown document when a thinking placeholder splits a streamed list item", () => {
  const { container } = render(<Transcript messageId="split" streaming blocks={[
    { type: "narrative", id: "a", text: "1. **Your father is/was a reserved, duty" },
    { type: "reasoning", id: "r", streaming: true },
    { type: "narrative", id: "b", text: "-bound man** — respectful but quiet." },
  ]} />);
  const items = container.querySelectorAll("li");
  expect(items).toHaveLength(1);
  expect(items[0].querySelector("strong")).toHaveTextContent("Your father is/was a reserved, duty-bound man");
  expect(container.textContent).not.toContain("**");
});

it("joins narrative fragments separated only by hidden status blocks", () => {
  const { container } = render(<Transcript messageId="status-split" blocks={[
    { type: "narrative", id: "a", text: "Start of **bo" },
    { type: "status", id: "s", label: "Prompt cache 85%" },
    { type: "narrative", id: "b", text: "ld** text" },
  ]} />);
  expect(container.querySelectorAll("p")).toHaveLength(1);
  expect(container.querySelector("strong")).toHaveTextContent("bold");
});

it("keeps narrative on either side of a real tool call separate", () => {
  const { container } = render(<Transcript messageId="tool-split" blocks={[
    { type: "narrative", id: "a", text: "Before the tool." },
    { type: "tool", id: "t", card: { id: "t", name: "web_search", status: "ok" } },
    { type: "narrative", id: "b", text: "After the tool." },
  ]} />);
  expect(screen.getByText("Before the tool.")).toBeInTheDocument();
  expect(screen.getByText("After the tool.")).toBeInTheDocument();
  expect(container.querySelectorAll("p").length).toBeGreaterThanOrEqual(2);
});


it("renders the full completed thought including its ending", () => {
  const thought = "Beginning of thought. " + "Detailed explanation. ".repeat(600) + "Final sentence stays visible.";
  render(<Transcript blocks={[{ type: "reasoning", id: "long", text: thought, durationMs: 21000 }]} messageId="long-thought" />);
  fireEvent.click(screen.getByText("Thought for 21s"));
  expect(screen.getByTestId("reasoning-body").textContent).toBe(thought);
});


it("shows preparation phase before tokens and keeps streamed reasoning in its collapsible", () => {
  const { rerender } = render(<Transcript blocks={[{ type: "reasoning", id: "r", streaming: true }]} streaming liveLabel="Preparing context…" messageId="phase" />);
  expect(screen.queryByText("Thinking…")).not.toBeInTheDocument();
  expect(screen.getByText("Preparing context…")).toBeVisible();
  const toggle = screen.getByRole("button", { name: "Waiting for model…" });
  fireEvent.click(toggle);
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("No reasoning text received yet");
  rerender(<Transcript blocks={[{ type: "reasoning", id: "r", streaming: true, text: "Provider reasoning arrives now." }]} streaming liveLabel="Preparing context…" messageId="phase" />);
  expect(screen.getByRole("button", { name: "Thinking…" })).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("Provider reasoning arrives now.");
});


it("keeps first reasoning tokens collapsed until opened and collapses on completion", () => {
  const { rerender } = render(<Transcript blocks={[{type: "reasoning", id: "auto", streaming: true}]} streaming messageId="auto" />);
  expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
  const draw = (text: string, streaming: boolean) => <Transcript blocks={[{ type: "reasoning", id: "auto", text, streaming, durationMs: streaming ? undefined : 2500 }]} streaming={streaming} messageId="auto" />;
  rerender(draw("First tokens", true));
  expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("First tokens");
  fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
  rerender(draw("First tokens and more", true));
  expect(screen.queryByTestId("reasoning-body")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Thinking…" }));
  rerender(draw("First tokens and more", false));
  expect(screen.getByRole("button", { name: "Thought for 2.5s" })).toHaveAttribute("aria-expanded", "false");
  fireEvent.click(screen.getByRole("button", { name: "Thought for 2.5s" }));
  expect(screen.getByTestId("reasoning-body")).toHaveTextContent("First tokens and more");
});
