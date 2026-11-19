import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

const { state } = vi.hoisted(() => ({ state: { messages: [] as Array<Record<string, unknown>>, error: "", clearError: vi.fn() } }));
vi.mock("../../store", () => ({ useAppStore: (selector: (value: typeof state) => unknown) => selector(state) }));
vi.mock("../MessageList", () => ({ MessageList: ({ messages }: { messages: typeof state.messages }) => <div>{messages.filter(m => !m.streaming).map(m => <span key={String(m.id)}>{String(m.failureCause || "")}</span>)}</div> }));
vi.mock("../ApprovalBanner", () => ({ ApprovalBanner: () => null }));
vi.mock("../DelegatedWork", () => ({ DelegatedWork: () => null }));
vi.mock("./MiniComposer", () => ({ MiniComposer: () => null }));
import { ConversationPanel } from "./ConversationPanel";

afterEach(cleanup);

it("shows a delivery warning once when it is already in the conversation", () => {
  state.error = "Delivery could not be confirmed.";
  state.messages = [{ id: "a", role: "assistant", streaming: false, failureCause: state.error }];
  const view = render(<ConversationPanel />);
  expect(view.getAllByText(state.error)).toHaveLength(1);
  expect(view.queryByRole("button", { name: "Dismiss" })).toBeNull();
});

it("keeps a separate upload error visible", () => {
  state.error = "Upload timed out. Files are kept.";
  state.messages = [{ id: "a", role: "assistant", failureCause: "Earlier connection warning." }];
  const view = render(<ConversationPanel />);
  expect(view.getByRole("alert")).toHaveTextContent(state.error);
  expect(view.getByRole("button", { name: "Dismiss" })).toBeEnabled();
});

it("does not hide an error behind a still-streaming message", () => {
  state.error = "Connection interrupted.";
  state.messages = [{ id: "a", role: "assistant", streaming: true, failureCause: state.error }];
  const view = render(<ConversationPanel />);
  expect(view.getByRole("alert")).toHaveTextContent(state.error);
});
