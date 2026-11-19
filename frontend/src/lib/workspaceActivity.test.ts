import { expect, it } from "vitest";
import { workspaceActivity } from "./workspaceActivity";
import type { ChatMessage } from "../api/types";
const tool = (status: string): ChatMessage => ({id: status, role: "assistant", text: "", toolCards: [{id: "call", name: "browser_fill", status}]});
const input = {messages: [tool("running")], runStatus: "running" as const, statusLabel: "Waiting for model…", liveSurface: "browser" as const, machineActive: false, machineShellRunning: false, machineShellLine: ""};
it("shows only actually running tools from the current turn", () => {
  expect(workspaceActivity(input)).toMatchObject({kind: "browser", running: true});
  expect(workspaceActivity({...input, messages: [tool("completed")]})).toEqual({kind: "Agent", summary: "Waiting for model…", running: true});
  expect(workspaceActivity({
    ...input,
    statusLabel: "Thinking…",
    messages: [{...tool("completed"), streaming: true, statusDetail: "Trying fallback model fixture"}],
  }).summary).toBe("Trying another model…");
  expect(workspaceActivity({...input, messages: [tool("running"), {id: "new", role: "user", text: "Next task"}]}).kind).toBe("Agent");
});
it.each(["idle", "success", "error", "interrupted", "cancelled"] as const)("does not revive a historical running tool when the task is %s", runStatus => {
  expect(workspaceActivity({...input, runStatus})).toEqual({kind: "Browser", summary: "Browser ready", running: false});
});
it("keeps approval waiting distinct from running input", () => {
  expect(workspaceActivity({...input, runStatus: "waiting_approval"})).toEqual({kind: "Agent", summary: "Waiting for approval", running: false});
});
it("uses the independently running terminal's current line", () => {
  expect(workspaceActivity({...input, runStatus: "idle", machineShellRunning: true, machineShellLine: "pwd"})).toEqual({kind: "Terminal", summary: "$ pwd", running: true});
});
