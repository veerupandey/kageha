import type { ChatMessage, SessionRun } from "../api/types";
import { applyTranscriptToMessage } from "../lib/transcript";
import { terminalRunState } from "../lib/runStatus";
import type { LocalTurn, ServerTurn } from "../lib/turnTruth";

/** The fields of a local run that `turnIndicator` and `staleLocalRun` read. */
export function localTurn(run: SessionRun | undefined | null): LocalTurn | null {
  if (!run) return null;
  return {
    status: run.status,
    sending: run.sending,
    waitingApproval: run.waitingApproval,
    startedAt: run.startedAt,
    turnId: run.transport?.turnId || (!run.sending ? run.messages.findLast(m => m.role === "assistant" && m.turnId)?.turnId : undefined),
  };
}

/** True while this tab's own request stream for the run is still open. */
export function hasLiveRequestStream(run: SessionRun | undefined | null): boolean {
  return Boolean(run?.abort && !run.abort.signal.aborted);
}

type SettledSnapshot = {
  status?: string;
  turns?: Array<{ id?: string; turn_id?: string; status?: string }>;
  messages?: Array<{ role?: string; text?: string; turn_id?: string }>;
};

/**
 * Ends a local run the server reports has no running turn (Requirement 18.4):
 * the composer, header and transcript stop showing it as running. The final
 * status is the turn's own status in `snapshot` when listed, else the
 * report's; the saved answer of that turn fills an empty streaming bubble.
 */
export function settleRunFromServer(run: SessionRun, report: ServerTurn, snapshot: SettledSnapshot = {}): SessionRun {
  const turnId = String(run.transport?.turnId || "");
  const turn = turnId ? snapshot.turns?.find((item) => (item.turn_id || item.id) === turnId) : undefined;
  const rawStatus = String(turn?.status || (turnId && snapshot.turns ? "" : report.status) || snapshot.status || "").toLowerCase();
  const terminal = terminalRunState(rawStatus);
  // A report says the turn is not running, so a "running" projection is unconfirmed.
  const settled = terminal.status === "running"
    ? { status: "interrupted" as const, label: "Completion unconfirmed" }
    : terminal;
  const saved = turnId
    ? [...(snapshot.messages || [])].reverse().find((message) => message.role === "assistant" && message.turn_id === turnId)?.text
    : undefined;
  const messages = run.messages.map((message): ChatMessage => {
    if (!message.streaming || message.role !== "assistant") return message;
    if (turnId && message.turnId && message.turnId !== turnId) return message;
    const text = saved !== undefined ? String(saved) : message.text;
    return {
      ...applyTranscriptToMessage(message, { type: "finish", assembled: text }),
      streaming: false,
      resultStatus: rawStatus || message.resultStatus,
      statusLabel: settled.label,
      failureCause: message.failureCause || (settled.status === "interrupted" ? settled.label : undefined),
    };
  });
  return {
    ...run,
    messages,
    sending: false,
    abort: null,
    status: settled.status,
    statusLabel: settled.label,
    waitingApproval: settled.status === "waiting_approval" ? run.waitingApproval : false,
    pendingApproval: settled.status === "waiting_approval" ? run.pendingApproval : null,
  };
}
