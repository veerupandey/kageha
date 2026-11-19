import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../../api/client";
import { SessionLifecycleDialog } from "./SessionLifecycleDialog";

vi.mock("../../api/client", () => ({ api: vi.fn() }));
afterEach(() => { cleanup();vi.clearAllMocks(); });

it("closes, displays the matching hook approval, and reopens a session", async () => {
  let status="open";
  vi.mocked(api).mockImplementation(async (path,options) => {
    if (path === "/api/sessions/chat/close") { status="closing";return {status} as never; }
    if (path === "/api/approvals") { expect(JSON.parse(String(options?.body))).toEqual({approval_id:"close-approval",approved:true,scope:"once"});status="closed";return {ok:true} as never; }
    if (path === "/api/sessions/chat/reopen") { status="open";return {status} as never; }
    if (path === "/api/sessions/chat") return {lifecycle_status:status,lifecycle:{status,turn_id:"close-turn"},pending_approval:status === "closing" ? {approval_id:"close-approval",turn_id:"close-turn",action:"project_hook",detail:"Run reviewed SessionEnd hook"} : null} as never;
    throw new Error(`Unexpected route: ${path}`);
  });
  const changed=vi.fn(async () => {});
  render(<SessionLifecycleDialog sessionId="chat" title="Example" onDismiss={() => {}} onChanged={changed} />);
  fireEvent.click(await screen.findByRole("button",{name:"Close session"}));
  expect(await screen.findByText("Run reviewed SessionEnd hook")).toBeVisible();
  fireEvent.click(screen.getByRole("button",{name:"Approve once"}));
  expect(await screen.findByText("Status: closed")).toBeVisible();
  fireEvent.click(screen.getByRole("button",{name:"Reopen session"}));
  expect(await screen.findByText("Status: open")).toBeVisible();
  expect(changed).toHaveBeenCalled();
});

it("does not offer approval for another turn", async () => {
  vi.mocked(api).mockResolvedValue({lifecycle_status:"closing",lifecycle:{status:"closing",turn_id:"close-turn"},pending_approval:{approval_id:"other",turn_id:"other-turn",detail:"Unrelated operation"}} as never);
  render(<SessionLifecycleDialog sessionId="chat" title="Example" onDismiss={() => {}} onChanged={async () => {}} />);
  await waitFor(() => expect(screen.getByText("Status: closing")).toBeVisible());
  expect(screen.queryByRole("button",{name:"Approve once"})).not.toBeInTheDocument();
});

it("resumes an interrupted archive with its original lifecycle intent", async () => {
  let status="interrupted";
  vi.mocked(api).mockImplementation(async (path, options) => {
    if (path === "/api/sessions/chat" && options?.method === "PATCH") {
      expect(JSON.parse(String(options.body))).toEqual({archived:true});status="closing";return {lifecycle_status:status} as never;
    }
    if (path === "/api/sessions/chat") return {lifecycle_status:status,lifecycle:{status,turn_id:"archive-turn",archive:true},pending_approval:null} as never;
    throw new Error(`Unexpected route: ${path}`);
  });
  render(<SessionLifecycleDialog sessionId="chat" title="Example" onDismiss={() => {}} onChanged={async () => {}} />);
  fireEvent.click(await screen.findByRole("button",{name:"Resume archive"}));
  await waitFor(() => expect(api).toHaveBeenCalledWith("/api/sessions/chat", expect.objectContaining({method:"PATCH",body:JSON.stringify({archived:true})})));
  expect(await screen.findByText("Status: closing")).toBeVisible();
});
