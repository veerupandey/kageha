import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { AskHumanBlock } from "./AskHumanBlock";

const mocks = vi.hoisted(() => ({
  state: {
    pendingApproval: null as unknown,
    submitAskHumanAnswer: vi.fn(),
    deferAskHuman: vi.fn(),
  },
}));
vi.mock("../../store", () => ({ useAppStore: (selector: (state: typeof mocks.state) => unknown) => selector(mocks.state) }));
afterEach(() => { cleanup(); mocks.state.pendingApproval = null; });
const block = { type: "ask_human" as const, id: "old-block", approvalId: "old-question", question: "Choose a color", options: ["Red", "Blue"], status: "pending" as const };

it("does not offer answers for an old question when a different question is pending", () => {
  mocks.state.pendingApproval = { approval_id: "current-question", action: "ask_human", question: "Choose a color", options: ["Red", "Blue"], risk_class: "hitl" };
  render(<AskHumanBlock block={block} messageId="a1" />);
  expect(screen.getByText("Choose a color")).toBeInTheDocument();
  expect(screen.queryByRole("group", { name: "Answer the agent's question" })).not.toBeInTheDocument();
});

it("offers answers only for the live pending question", () => {
  mocks.state.pendingApproval = { approval_id: "old-question", action: "ask_human", question: "Choose a color", options: ["Red", "Blue"], risk_class: "hitl" };
  render(<AskHumanBlock block={block} messageId="a1" />);
  expect(screen.getByRole("form", { name: "Answer the agent's question" })).toBeInTheDocument();
});

it("does not revive answer controls for a historical pending block after resolution", () => {
  render(<AskHumanBlock block={block} messageId="a1" />);
  expect(screen.queryByRole("button", { name: "Send answer" })).not.toBeInTheDocument();
});
