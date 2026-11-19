import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { WorkerCommunication } from "./WorkerCommunication";
import { workerEnvironment } from "../lib/workerPresentation";
import type { DurableJob } from "./DurableJobs";
const job: DurableJob = { id: "worker", label: "Researcher", kind: "agent", state: "running", session_id: "child", created_at: 1, started_at: 1, revision: 1, can_cancel: true, attachable: true, artifacts: [], error: {}, result: {} };
beforeEach(() => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value), removeItem: (key: string) => data.delete(key) });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
describe("worker communication", () => {
  it("returns keyboard focus to the message button when composing is cancelled", () => {
    render(<WorkerCommunication job={job} />);
    fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
    expect(screen.getByRole("textbox")).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("button", { name: "Message Researcher" })).toHaveFocus();
  });
  it("distinguishes the conversation from desktop ownership", () => {
    expect(workerEnvironment({ ...job, spec: { machine_active: true, machine_owner_session: "parent" } })).toBe("Own context · tools available when needed");
    expect(workerEnvironment({ ...job, spec: { machine_active: true }, progress: { label: "Recorded action", surface: "browser", steps: 1, updated_at: 1 } })).toBe("Own context · shared computer");
    expect(workerEnvironment({ ...job, spec: { isolation: "worktree" } })).toBe("Own context · isolated checkout");
  });
  it("sends guidance to the selected worker without opening its conversation", async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: true }); vi.stubGlobal("fetch", fetch);
    render(<WorkerCommunication job={job} />);
    fireEvent.click(screen.getByText("Message Researcher"));
    fireEvent.change(screen.getByRole("textbox", { name: "Message Researcher" }), { target: { value: "Use the parent’s attached desktop" } });
    fireEvent.click(screen.getByText("Send message"));
    await waitFor(() => expect(fetch).toHaveBeenCalledWith("/api/jobs/worker/message", expect.objectContaining({ body: expect.any(String) })));
    expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ message: "Use the parent’s attached desktop", message_id: expect.any(String) });
    expect(await screen.findByRole("status")).toHaveTextContent("Message queued");
  });
  it("preserves guidance after a failed send", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false }));
    render(<WorkerCommunication job={job} />);
    fireEvent.click(screen.getByText("Message Researcher"));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "Check the existing tab" } });
    fireEvent.click(screen.getByText("Send message"));
    await screen.findByRole("alert");
    expect(screen.getByRole("textbox")).toHaveValue("Check the existing tab");
  });
  it("shows who sent guidance and whether the worker received it", () => {
    render(<WorkerCommunication job={{ ...job, communication: [
      { id: "a", body: "Check the form", sender: "parent", created_at: 1, delivered_at: 2 },
      { id: "b", body: "Use this email", sender: "user", created_at: 3, delivered_at: null },
    ] }} />);
    fireEvent.click(screen.getByText("Messages · 2"));
    expect(screen.getByText("Kageha → Researcher")).toBeInTheDocument();
    expect(screen.getByText("You → Researcher")).toBeInTheDocument();
    expect(screen.getByText(/Delivered/)).toBeInTheDocument();
    expect(screen.getByText(/Queued ·/)).toBeInTheDocument();
  });
  it("does not offer guidance to a finished worker", () => {
    render(<WorkerCommunication job={{ ...job, state: "succeeded" }} />);
    expect(screen.queryByText("Message Researcher")).not.toBeInTheDocument();
  });
});

it("distinguishes acknowledgement from delivery and shows the worker's explanation", () => {
  render(<WorkerCommunication job={{ ...job, communication: [{ id: "ack", sender: "user", body: "Use the updated constraints", created_at: 1, delivered_at: 2, acknowledged_at: 3, acknowledgement: "I revised the comparison scope" }] }} />);
  fireEvent.click(screen.getByText("Messages · 1"));
  expect(screen.getByText(/Acknowledged/)).toBeInTheDocument();
  expect(screen.getByText("Researcher: I revised the comparison scope")).toBeInTheDocument();
});

it("reuses the same guidance ID after a failed response", async () => {
  const fetch = vi.fn().mockResolvedValueOnce({ ok: false }).mockResolvedValueOnce({ ok: true });
  vi.stubGlobal("fetch", fetch);
  render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByText("Message Researcher"));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Use the revised criteria" } });
  fireEvent.click(screen.getByText("Send message"));
  await screen.findByRole("alert");
  fireEvent.click(screen.getByText("Send message"));
  await screen.findByRole("status");
  const first = JSON.parse(fetch.mock.calls[0][1].body);
  const retry = JSON.parse(fetch.mock.calls[1][1].body);
  expect(first.message_id).toBeTruthy();
  expect(retry).toEqual(first);
});

it("restores unsent text after closing and reopening without sending automatically", () => {
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  const first = render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Keep the night-time examples" } });
  first.unmount();
  render(<WorkerCommunication job={job} />);
  expect(screen.getByRole("textbox")).toHaveValue("Keep the night-time examples");
  expect(fetch).not.toHaveBeenCalled();
});

it("keeps retry identity through a remount and clears the draft only after acceptance", async () => {
  const fetch = vi.fn().mockResolvedValueOnce({ ok: false }).mockResolvedValueOnce({ ok: true }); vi.stubGlobal("fetch", fetch);
  const first = render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Use the revised scope" } });
  fireEvent.click(screen.getByRole("button", { name: "Send message" }));
  await screen.findByRole("alert");
  const sent = JSON.parse(fetch.mock.calls[0][1].body);
  first.unmount();
  const second = render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Send message" }));
  await screen.findByRole("status");
  expect(JSON.parse(fetch.mock.calls[1][1].body)).toEqual(sent);
  second.unmount();
  render(<WorkerCommunication job={job} />);
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
});

it("does not mix drafts when the selected specialist changes", () => {
  const view = render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Private draft for first worker" } });
  view.rerender(<WorkerCommunication job={{ ...job, id: "other", session_id: "other-chat", label: "Niko" }} />);
  expect(screen.queryByDisplayValue("Private draft for first worker")).not.toBeInTheDocument();
  view.rerender(<WorkerCommunication job={job} />);
  expect(screen.getByRole("textbox")).toHaveValue("Private draft for first worker");
});

it("keeps a finished agent's unsent draft available to recover or discard", () => {
  const first = render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Saved but unsent" } });
  first.unmount();
  render(<WorkerCommunication job={{ ...job, state: "succeeded" }} />);
  expect(screen.getByRole("textbox", { name: "Unsent message to Researcher" })).toHaveValue("Saved but unsent");
  fireEvent.click(screen.getByRole("button", { name: "Discard draft" }));
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
});

it("reconciles a saved send with the server receipt after a lost response", async () => {
  const fetch = vi.fn().mockResolvedValue({ ok: false }); vi.stubGlobal("fetch", fetch);
  const first = render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Already accepted" } });
  fireEvent.click(screen.getByRole("button", { name: "Send message" }));
  await screen.findByRole("alert");
  const sent = JSON.parse(fetch.mock.calls[0][1].body);
  first.unmount();
  const second = render(<WorkerCommunication job={{ ...job, communication: [{ id: sent.message_id, body: sent.message, sender: "user", created_at: 1, delivered_at: null }] }} />);
  second.unmount();
  render(<WorkerCommunication job={job} />);
  expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("keeps editing usable when persistent storage is blocked", () => {
  vi.stubGlobal("localStorage", { getItem() { throw new Error("blocked"); }, setItem() { throw new Error("blocked"); } });
  render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Still editable" } });
  expect(screen.getByRole("textbox")).toHaveValue("Still editable");
});

it("does not let a delayed response erase text typed after reopening the panel", async () => {
  let accept!: (value: { ok: boolean }) => void;
  vi.stubGlobal("fetch", vi.fn(() => new Promise(resolve => { accept = resolve; })));
  const first = render(<WorkerCommunication job={job} />);
  fireEvent.click(screen.getByRole("button", { name: "Message Researcher" }));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "First message" } });
  fireEvent.click(screen.getByRole("button", { name: "Send message" }));
  first.unmount();
  const second = render(<WorkerCommunication job={job} />);
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "Newer unsent draft" } });
  await act(async () => { accept({ ok: true }); });
  await waitFor(() => expect(screen.getByRole("textbox")).toHaveValue("Newer unsent draft"));
  second.unmount();
  render(<WorkerCommunication job={job} />);
  expect(screen.getByRole("textbox")).toHaveValue("Newer unsent draft");
});
