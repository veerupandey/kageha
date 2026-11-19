import type { ChatMessage, RunStatus } from "../api/types";
import type { LiveSurface } from "../store/types";
import { friendlyActivityLabel, modelActivityLabel } from "./activityUi";
import { collectMessageToolCards } from "./messageArtifacts";
import { toolChipKind, toolChipSummary } from "./transcript";

export interface WorkspaceActivity {
  kind: string;
  summary: string;
  running: boolean;
}

/** Only current-turn work can be presented as executing. */
export function workspaceActivity(input: {
  messages: ChatMessage[];
  runStatus: RunStatus;
  statusLabel: string;
  machineActive: boolean;
  liveSurface: LiveSurface;
  machineShellRunning: boolean;
  machineShellLine: string;
  waitingForAnswer?: boolean;
}): WorkspaceActivity {
  if (input.machineShellRunning && input.machineShellLine) {
    const line = input.machineShellLine;
    return { kind: "Terminal", summary: line.startsWith("$") ? line : `$ ${line}`, running: true };
  }
  if (input.runStatus === "running") {
    let currentDetail: string | undefined;
    for (let i = input.messages.length - 1; i >= 0; i--) {
      const message = input.messages[i];
      if (message.role === "user") break;
      if (message.role !== "assistant") continue;
      if (message.streaming) currentDetail ??= message.statusDetail;
      const card = collectMessageToolCards(message).findLast(card =>
        ["running", "pending", "requested"].includes(card.status || ""));
      if (card) return {
        kind: toolChipKind(card.name).replace(/^(Using|Running|Creating)\s+/i, ""),
        summary: toolChipSummary(card),
        running: true,
      };
    }
    const label = friendlyActivityLabel(input.statusLabel);
    return { kind: "Agent", summary: (label === "Thinking…" ? modelActivityLabel(currentDetail) ?? currentDetail : undefined) || label || "Working…", running: true };
  }
  if (input.runStatus === "waiting_approval") return {
    kind: "Agent",
    summary: input.waitingForAnswer ? "Waiting for your answer" : "Waiting for approval",
    running: false,
  };
  const kind = input.machineActive || input.liveSurface === "computer" ? "Computer"
    : input.liveSurface === "browser" ? "Browser" : "Host";
  return { kind, summary: `${kind} ready`, running: false };
}
