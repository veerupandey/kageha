import { terminalRunState } from "./runStatus";

/**
 * What the server last reported about a chat's turn (design section 16).
 * `running` mirrors `native_chat_turns.status IN ('running','waiting_for_unlock')`,
 * reported as `active_turn` by the sessions API and as `turn.running` by the
 * replay endpoint. `observedAt` is when the request that produced it started.
 */
export interface ServerTurn {
  turnId: string;
  status: string;
  running: boolean;
  observedAt: number;
}

type ActiveTurn = { turn_id?: string; status?: string; phase?: string } | null | undefined;

/** The turn status in a sessions API row or detail (`active_turn`, `status`). */
export function serverTurnFromSession(
  data: { active_turn?: ActiveTurn; active?: boolean; status?: string; turn_status?: string } | null | undefined,
  observedAt: number,
): ServerTurn | null {
  if (!data || typeof data !== "object") return null;
  const active = data.active_turn;
  if (active && typeof active === "object") {
    return {
      turnId: String(active.turn_id || ""),
      status: String(active.status || "running"),
      running: true,
      observedAt,
    };
  }
  // A row without `active_turn` says nothing about the turn (an older server).
  if (!("active_turn" in data)) return null;
  return {
    turnId: "",
    status: String(data.turn_status || data.status || "idle"),
    running: false,
    observedAt,
  };
}

/** The `turn {turn_id, status, native_status, running}` of a replay page. */
export function serverTurnFromReplay(
  turn: { turn_id?: string; status?: string; native_status?: string; running?: boolean } | null | undefined,
  observedAt: number,
): ServerTurn | null {
  if (!turn || typeof turn !== "object" || typeof turn.running !== "boolean") return null;
  return {
    turnId: String(turn.turn_id || ""),
    status: String(turn.native_status || turn.status || (turn.running ? "running" : "idle")),
    running: turn.running,
    observedAt,
  };
}

/** The local run fields that may affect the indicator. */
export interface LocalTurn {
  status: string;
  sending: boolean;
  waitingApproval: boolean;
  /** When this tab sent the turn; later server reports are authoritative. */
  startedAt?: number;
  /** Set once the server admitted the turn. */
  turnId?: string;
}

const SETTLED = new Set(["cancelled", "success", "error", "interrupted", "idle"]);

/** A server status shown for a chat with no running turn; never "running". */
function idleStatus(status: string): string {
  const value = String(status || "").toLowerCase();
  if (!value || value === "running" || value === "waiting_for_unlock") return "idle";
  const state = terminalRunState(value).status;
  return state === "running" ? "idle" : state === "waiting_approval" ? "waiting_approval" : state;
}

/**
 * The status a chat shows: "running", "waiting_approval", "connecting" or a
 * settled status. "running" comes only from the server. A local flag may show
 * "connecting" while the server has not yet reported on the turn this tab sent,
 * and a local Stop or finish settles a chat the server still reports running.
 */
export function turnIndicator(server: ServerTurn | null | undefined, run: LocalTurn | null | undefined): string {
  const fresh = server && (!run?.startedAt || server.observedAt >= run.startedAt) ? server : null;
  const localBusy = Boolean(run && (run.sending || run.status === "running"));
  const localSettled = Boolean(run && !localBusy && !run.waitingApproval && SETTLED.has(run.status));
  if (fresh?.running) {
    if (fresh.status === "waiting_approval" || run?.waitingApproval) return "waiting_approval";
    // The run this tab finished or stopped is settled even before the next report.
    if (localSettled && run!.status !== "idle" &&
        (!fresh.turnId || !run!.turnId || fresh.turnId === run!.turnId)) return run!.status;
    return "running";
  }
  if (fresh) {
    // A turn sent from this tab that the server has not admitted yet.
    if (localBusy && run?.startedAt && !run.turnId) return "connecting";
    if (run?.waitingApproval && !localBusy) return "waiting_approval";
    return idleStatus(fresh.status);
  }
  if (run?.waitingApproval) return "waiting_approval";
  if (localBusy) return "connecting";
  if (run && run.status !== "idle") return run.status;
  return server ? idleStatus(server.status) : "idle";
}

/** True when the server reports this chat's turn as running. */
export function serverSaysRunning(server: ServerTurn | null | undefined): boolean {
  return Boolean(server?.running);
}

/**
 * True when a fresh server report proves the local run's turn is no longer
 * running: the report is newer than the local send and the turn was admitted
 * (or was restored without a local send).
 */
export function staleLocalRun(server: ServerTurn | null | undefined, run: LocalTurn | null | undefined): boolean {
  if (!server || server.running || !run) return false;
  if (!(run.sending || run.status === "running")) return false;
  if (run.startedAt && server.observedAt < run.startedAt) return false;
  if (run.startedAt && !run.turnId) return false;
  // A report about a different turn does not settle this one.
  if (server.turnId && run.turnId && server.turnId !== run.turnId) return false;
  return true;
}
