import { terminalRunState } from "../lib/runStatus";
import { refreshWorkers } from "../lib/workerConnection";
export { terminalRunState } from "../lib/runStatus";
import { api } from "../api/client";
import { streamSessionEvents } from "../api/stream";
import { serverTurnFromReplay, serverTurnFromSession, type ServerTurn } from "../lib/turnTruth";
import { loopEventDetail } from "../lib/loopEvents";
import type { ChatMessage, PendingApproval, SessionRun } from "../api/types";
import { pendingApprovalFieldsFromPayload } from "../lib/askHumanApproval";
import { applyLiveNarrativeSnapshot, applyStreamFrameToMessage, applyTranscriptToMessage } from "../lib/transcript";
import { activityTarget, isBrowserToolName, isComputerToolName, isMachineShellToolName } from "../lib/activityUi";
import { isMachineDesktopApp } from "../lib/liveSurface";
import { taskStatusFromPayload } from "../lib/taskStatus";
import { attachMachineLivePanel, revealLiveSurface } from "./runTurn";
import { appendActivityStep } from "./activity";
import {
  normalizeComputerFrame,
  acknowledgeSteering,
  normalizeToolCard,
  parseTodoBoardPayload,
  mergeTodoBoard,
  approvalStatusLabel,
  isApprovalSuppressed,
  suppressResolvedApproval,
  shouldIgnoreStaleQuestionApproval,
  syncFromRun,
  touchTransport,
  uid,
} from "./helpers";
import type { AppState } from "./types";

type SetState = (
  partial:
    | Partial<AppState>
    | ((state: AppState) => Partial<AppState> | AppState),
) => void;
type GetState = () => AppState;

export type ReattachDeps = {
  onSettled?: (sessionId: string, status: SessionRun["status"]) => void;
  /** Receives each server report of the turn's status (replay `turn`, session `active_turn`). */
  onServerTurn?: (sessionId: string, turn: ServerTurn | null) => void;
  set: SetState;
  get: GetState;
  updateRun: (
    sessionId: string,
    updater: (run: SessionRun) => SessionRun,
  ) => void;
};

interface EventFrame {
  turn_id?: string;
  sequence?: number;
  kind?: string;
  label?: string;
  detail?: string[];
  interesting?: boolean;
  tool_card?: Record<string, unknown>;
  computer_frame?: Record<string, unknown>;
  payload?: Record<string, unknown>;
}

interface SessionSnapshot {
  requested_turn?: { turn_id: string; status: string; phase: string } | null;
  turns?: { id?: string; status?: string }[];
  messages?: Array<{ role: string; text: string }>;
  pending_approval?: PendingApproval | null;
  active_turn?: { turn_id?: string; status?: string; phase?: string; speaker?: ChatMessage["speaker"] } | null;
  title?: string | null;
  live_message?: string;
  live_message_delta?: string;
  status?: string;
}

export type ReattachStop = (opts?: { cancelled?: boolean }) => void;

export function pendingApprovalFromFrame(
  frame: EventFrame,
  sessionId: string,
): PendingApproval | null {
  if (String(frame.kind || "") !== "approval_required") return null;
  const payload = frame.payload || {};
  const approvalId = String(payload.approval_id || "").trim();
  if (!approvalId) return null;
  return {
    approval_id: approvalId,
    sessionId,
    action: String(payload.action || frame.label || "Approval needed"),
    ...(payload.profile_scoped === true ? {profile_scoped:true} : {}),
    risk_class: payload.risk_class ? String(payload.risk_class) : undefined,
    detail: payload.detail as string | string[] | undefined,
    label: payload.label ? String(payload.label) : frame.label || undefined,
    secret: typeof payload.secret === "boolean" ? payload.secret : undefined,
    ...pendingApprovalFieldsFromPayload(payload),
  };
}

/**
 * Final text for a reattached assistant bubble. Never use the latest
 * session assistant row — a follow-up turn started after Stop may already
 * have completed and would leak into the cancelled bubble.
 */
export function turnScopedFinalText(
  accumulated: string,
  _sessionMessages?: Array<{ role: string; text: string }>,
): string {
  return accumulated;
}

export function applyLiveMessage(
  message: ChatMessage,
  live: string,
): ChatMessage {
  return applyLiveNarrativeSnapshot(message, live);
}

const POLL_MS = 1200;
const POLL_MS_RETRY = 2400;
/** Stream reconnects wait 1, 2, 4 … 30 s (design section 16). */
export const RECONNECT_BASE_MS = 1000;
export const RECONNECT_MAX_MS = 30_000;
export function reconnectDelay(attempt: number): number {
  return Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** Math.min(Math.max(0, attempt), 5));
}
/** Events per replay page; `has_more` / `next_after` page through the rest. */
const REPLAY_PAGE = 500;
const REPLAY_MAX_PAGES = 200;

type ReplayTurn = { turn_id?: string; status?: string; native_status?: string; running?: boolean };

/**
 * Reattach to a turn that is still running on the backend after a page
 * reload / session switch. Replays events emitted since the frontend lost
 * connection, then streams updates with polling as a fallback until completion.
 */
export function reattachToActiveTurn(
  deps: ReattachDeps,
  sessionId: string,
  threadId: string,
  turnId: string,
  opts: {
    pendingApproval?: PendingApproval | null;
    /** Reuse an existing assistant message instead of appending a new one (mid-stream recovery). */
    assistantId?: string;
    /** Keep consuming peer lifecycle events after the normal turn completes. */
    backgroundPeerMonitor?: boolean;
    afterSequence?: number;
    initialSnapshot?: SessionSnapshot;
    /** Page through missed events before streaming (default: mid-stream recovery with a cursor). */
    replayFirst?: boolean;
  } = {},
): ReattachStop {
  const { set, get, updateRun } = deps;
  if (isApprovalSuppressed(sessionId, opts.pendingApproval)) {
    opts = { ...opts, pendingApproval: null };
  }
  const assistantId = opts.assistantId ?? uid("a");
  const reuseMessage = Boolean(opts.assistantId);
  let afterSeq = Number(opts.afterSequence || 0);
  let stopped = false;
  let finalizing = false;
  const requestId = get().runs[sessionId]?.clientRequestId;
  let cancelled = false;
  let peerFinished = false;
  let pollTimer = 0;
  const pollController = new AbortController();
  const obsolete = () => !opts.backgroundPeerMonitor && Boolean(
    (get().runs[sessionId]?.transport?.turnId && get().runs[sessionId]?.transport?.turnId !== turnId)
      || get().runs[sessionId]?.clientRequestId !== requestId,
  );
  let transportMode: "sse" | "polling" = opts.backgroundPeerMonitor ? "polling" : "sse";
  let attempt = 0;
  let reconnectTimer = 0;
  let streamController = new AbortController();

  if (!opts.backgroundPeerMonitor) updateRun(sessionId, (run) => {
    const reconnecting = touchTransport(run, {
      mode: transportMode,
      lastSequence: afterSeq,
      turnId,
      reconnectCount: (run.transport?.reconnectCount || 0) + 1,
    });
    if (reuseMessage) {
      // Mid-stream recovery: patch the existing assistant bubble, don't append.
      // Drop the obsolete SSE AbortController so Stop uses this poller.
      return {
        ...reconnecting,
        sending: true,
        abort: null,
        status: "running",
        statusLabel: "Reconnecting…",
        waitingApproval: Boolean(opts.pendingApproval),
        messages: reconnecting.messages.map((m) =>
          m.id === assistantId
            ? { ...m, streaming: true, statusLabel: "Reconnecting…", turnId,
                ...(m.resultStatus === "delivery_unconfirmed" ? { resultStatus: undefined, failureCause: undefined } : {}) }
            : m,
        ),
      };
    }
    return {
      ...reconnecting,
      sending: true,
      abort: null,
      status: "running",
      statusLabel: "Reconnecting…",
      waitingApproval: Boolean(opts.pendingApproval),
      messages: [
        ...reconnecting.messages,
        {
          id: assistantId,
          role: "assistant",
          text: "",
          streaming: true,
          statusLabel: "Reconnecting…",
          steps: [],
          toolCards: [],
          computerFrames: [],
          blocks: [],
          turnId,
        },
      ],
    };
  });
  if (!opts.backgroundPeerMonitor && sessionId === get().sessionId) {
    set({ runStatus: "running", statusLabel: "Reconnecting…", sending: true });
  }
  if (!opts.backgroundPeerMonitor && opts.pendingApproval && sessionId === get().sessionId) {
    set({ pendingApproval: { ...opts.pendingApproval, sessionId } });
  }

  const applyFrame = (frame: EventFrame) => {
    if (frame.turn_id && frame.turn_id !== turnId) return;
    if (typeof frame.sequence === "number" && frame.sequence > 0 && frame.sequence <= afterSeq) return;
    if (typeof frame.sequence === "number" && frame.sequence > afterSeq) {
      afterSeq = frame.sequence;
      if (!opts.backgroundPeerMonitor) updateRun(sessionId, (run) =>
        touchTransport(run, { mode: transportMode, lastSequence: afterSeq, turnId }),
      );
    }
    const kind = String(frame.kind || "");
    const sourceKind = String(frame.payload?.source_kind || kind);
    if (opts.backgroundPeerMonitor && !sourceKind.startsWith("peer_")) return;
    if (kind === "request_budget" || sourceKind === "request_budget") {
      // Budget updates are telemetry, but their sequence still belongs to the
      // transcript so reconnect and replay continue after the latest event.
      updateRun(sessionId, (run) => ({
        ...run,
        messages: run.messages.map((message) => message.id === assistantId
          ? applyStreamFrameToMessage(message, { sequence: frame.sequence, turn_id: frame.turn_id || turnId, kind: "request_budget" })
          : message),
      }));
      return;
    }
    if (sourceKind === "user_inject") {
      updateRun(sessionId, (run) => acknowledgeSteering(run, String(frame.payload?.message || ""), String(frame.payload?.message_id || "")));
    }
    const eventText = `${kind} ${frame.label || ""} ${frame.payload?.tool_name || ""} ${frame.payload?.name || ""}`.toLowerCase();
    if (sourceKind === "task_status" && sessionId === get().sessionId) {
      get().setTaskStatus(taskStatusFromPayload(frame.payload || {}));
    }
    if (sourceKind.startsWith("peer_")) {
      const capabilityId = String(frame.payload?.capability_id || "").toLowerCase();
      const surface = capabilityId.includes("browser")
        ? "browser"
        : capabilityId.includes("desktop") || capabilityId.includes("computer")
          ? "computer"
          : "worker";
      const rawStatus = String(frame.payload?.state || (sourceKind === "peer_task_submitted" ? "requested" : "running"));
      const status = ["requested", "running", "completed", "failed", "canceled"].includes(rawStatus)
        ? rawStatus as "requested" | "running" | "completed" | "failed" | "canceled"
        : sourceKind === "peer_task_completed" ? "completed" : sourceKind === "peer_task_failed" ? "failed" : "running";
      if (sessionId === get().sessionId && (!opts.backgroundPeerMonitor ||
        (get().runs[sessionId]?.transport?.turnId === turnId && get().runs[sessionId]?.clientRequestId === requestId))) {
        get().setRemoteExecution({
          peerId: String(frame.payload?.peer_id || ""),
          peerName: frame.payload?.peer_name ? String(frame.payload.peer_name) : undefined,
          taskId: String(frame.payload?.task_id || ""),
          capabilityId,
          surface,
          status,
        });
        if (surface === "browser" || surface === "computer") {
          revealLiveSurface(get, set, sessionId, surface);
        }
      }
    }
    const frameApp = String(
      (frame.computer_frame && typeof frame.computer_frame === "object"
        ? (frame.computer_frame as { app?: string }).app
        : "") || "",
    );
    if (kind === "machine_ready" || kind === "machine_starting") {
      attachMachineLivePanel(get, set, updateRun, sessionId, frame.payload || {});
    } else if (sessionId === get().sessionId && (frameApp === "Kageha Browser" || isBrowserToolName(eventText) || activityTarget(eventText) === "browser")) {
      revealLiveSurface(get, set, sessionId, "browser");
    } else if (sessionId === get().sessionId && isMachineDesktopApp(frameApp)) {
      attachMachineLivePanel(get, set, updateRun, sessionId);
    } else if (sessionId === get().sessionId && get().machineActive && (isMachineShellToolName(eventText) || activityTarget(eventText) === "machine")) {
      revealLiveSurface(get, set, sessionId, "machine");
    } else if (sessionId === get().sessionId && (isComputerToolName(eventText) || activityTarget(eventText) === "computer" || (frame.computer_frame && frameApp !== "Kageha Browser"))) {
      if (get().machineActive) {
        revealLiveSurface(get, set, sessionId, "machine");
      } else if (!isComputerToolName(eventText) || frame.computer_frame) {
        revealLiveSurface(get, set, sessionId, "computer");
      }
    }
    if (sourceKind === "peer_task_completed" || sourceKind === "peer_task_failed") peerFinished = true;
    const isInternalProtocolKind = ["stream_item", "stream_patch", "delta", "model", "keepalive"].includes(kind);
    const label = String(frame.label || (isInternalProtocolKind ? "" : kind) || "");
    const detail = Array.isArray(frame.detail) && frame.detail.length
      ? frame.detail
      : loopEventDetail(kind, frame.payload);
    const interesting = frame.interesting !== false;
    if (kind === "approval_resolved") {
      suppressResolvedApproval(sessionId, String(frame.payload?.approval_id || ""));
    }
    const frameApproval = pendingApprovalFromFrame(frame, sessionId);

    if (frameApproval && !isApprovalSuppressed(sessionId, frameApproval)) {
      const waitLabel = approvalStatusLabel(frameApproval);
      updateRun(sessionId, (run) => ({
        ...run,
        waitingApproval: true,
        pendingApproval: frameApproval,
        status: "waiting_approval",
        statusLabel: waitLabel,
      }));
      if (sessionId === get().sessionId) {
        set({
          pendingApproval: frameApproval,
          runStatus: "waiting_approval",
          statusLabel: waitLabel,
        });
      }
    } else if (
      kind === "approval_resolved" ||
      kind === "blocked" ||
      kind === "cancelled" ||
      kind === "turn_completed"
    ) {
      updateRun(sessionId, (run) => ({ ...run, waitingApproval: false, pendingApproval: null }));
      if (sessionId === get().sessionId) set({ pendingApproval: null });
    }

    if (kind === "todo_board" && frame.payload) {
      const board = parseTodoBoardPayload(frame.payload);
      if (board) updateRun(sessionId, (run) => ({ ...run, todoBoard: mergeTodoBoard(run.todoBoard, board, String(frame.turn_id || run.transport?.turnId || "")) }));
    }

    set((s) => {
      const run = s.runs[sessionId];
      if (!run) return s;
      const messages = run.messages.map((m) => {
        if (m.id !== assistantId) return m;
        let next = m;
        if (label) {
          next = {
            ...next,
            statusLabel: label,
            statusDetail: detail[0] || next.statusDetail,
            steps: interesting
              ? appendActivityStep(next.steps || [], {
                  label,
                  detail,
                  kind,
                  interesting,
                })
              : next.steps || [],
          };
        }
        let card: ReturnType<typeof normalizeToolCard> = null;
        if (frame.tool_card) {
          card = normalizeToolCard(frame.tool_card, kind || "tool_card");
          if (card) {
            const jobIds = card.subagentGraph?.flatMap(node => node.job_id ? [node.job_id] : []) || [];
            if (jobIds.length) refreshWorkers(sessionId, jobIds);
            const resolved = card;
            const cards = [...(next.toolCards || [])];
            const idx = cards.findIndex((c) => c.id === resolved.id);
            if (idx >= 0) cards[idx] = { ...cards[idx], ...resolved };
            else cards.push(resolved);
            next = { ...next, toolCards: cards.slice(-24) };
            if (resolved.artifactRefs?.length && sessionId === get().sessionId) {
              get().upsertCanvasPaths(resolved.artifactRefs);
            }
          }
        }
        if (frame.computer_frame) {
          const cf = normalizeComputerFrame(frame.computer_frame, sessionId);
          if (cf) {
            next = {
              ...next,
              computerFrames: [...(next.computerFrames || []), cf].slice(-12),
            };
          }
        }
        return applyStreamFrameToMessage(next, {
          sequence: frame.sequence,
          turn_id: frame.turn_id || turnId,
          kind,
          label,
          detail,
          interesting,
          payload: frame.payload,
          card,
        });
      });
      const nextRun = { ...run, messages, statusLabel: opts.backgroundPeerMonitor ? run.statusLabel : label || run.statusLabel };
      const sync = s.sessionId === sessionId ? syncFromRun(nextRun) : {};
      return { ...sync, runs: { ...s.runs, [sessionId]: nextRun } };
    });
  };

  const finalize = async (confirmed?: SessionSnapshot) => {
    if (stopped || obsolete() || finalizing) return;
    finalizing = true;
    let autoTitle: string | null = null;
    let rawStatus = "";
    let pendingApproval: PendingApproval | null = null;
    try {
      const observedAt = Date.now();
      const data = confirmed || await api<SessionSnapshot & { status?: string }>(
        `/api/sessions/${encodeURIComponent(sessionId)}`,
        { signal: pollController.signal },
      );
      if (stopped || obsolete()) return;
      if (!confirmed) deps.onServerTurn?.(sessionId, serverTurnFromSession(data, observedAt));
      if (data.active_turn?.turn_id === turnId) {
        // EOF only ends the transport. The runtime is authoritative: catch up and reconnect.
        finalizing = false;
        void recover();
        return;
      }
      autoTitle = data.title ? String(data.title) : null;
      const requested = data.requested_turn?.turn_id === turnId ? data.requested_turn : data.turns?.find(t => t.id === turnId);
      const sameTurn = !data.active_turn || data.active_turn.turn_id === turnId;
      rawStatus = String(requested?.status || (sameTurn ? data.status : "") || "").toLowerCase();
      const needsInput = !requested || ["ask_user", "awaiting_clarify", "waiting_input", "waiting_approval", "awaiting_plan_approval"].includes(rawStatus);
      pendingApproval = sameTurn && needsInput && data.pending_approval
        ? { ...data.pending_approval, sessionId }
        : null;
      if (isApprovalSuppressed(sessionId, pendingApproval)) pendingApproval = null;
    } catch {
      finalizing = false;
      if (!stopped && !obsolete()) {
        transportMode = "polling";
        scheduleRecover();
      }
      return;
    }
    if (stopped || obsolete()) return;
    stopped = true;
    window.clearTimeout(pollTimer);
    window.clearTimeout(reconnectTimer);
    pollController.abort();
    streamController.abort();
    const terminal = terminalRunState(rawStatus, { cancelled });
    if (obsolete()) return;
    const status = pendingApproval ? "waiting_approval" : terminal.status;
    const label = pendingApproval
      ? approvalStatusLabel(pendingApproval)
      : terminal.label;
    updateRun(sessionId, (r) => {
      const messages = r.messages.map((m) => {
        if (m.id !== assistantId) return m;
        if (!m.streaming) return m;
        return {
          ...applyTranscriptToMessage(m, {
            type: "finish",
            assembled: turnScopedFinalText(m.text, []),
          }),
          streaming: false,
          resultStatus: rawStatus,
          statusLabel: label,
          failureCause: m.failureCause || (["invalid_output", "unverified"].includes(rawStatus)
            ? "The response was delivered, but task completion could not be verified."
            : terminal.status === "interrupted" ? terminal.label : undefined),
        };
      });
      return {
        ...r,
        messages,
        sending: false,
        abort: null,
        waitingApproval: Boolean(pendingApproval),
        pendingApproval,
        status,
        statusLabel: label,
      };
    });
    if (sessionId === get().sessionId) {
      set({
        ...(autoTitle ? { sessionTitle: autoTitle } : {}),
        runStatus: status,
        statusLabel: label,
        sending: false,
        pendingApproval,
      });
    }
    void get().refreshArtifacts();
    deps.onSettled?.(sessionId, status);
  };

  type LivePacket = { events?: EventFrame[]; pending_approval?: PendingApproval | null; live?: SessionSnapshot };
  let liveText = "";
  const applyPacket = (events: LivePacket, session: SessionSnapshot) => {
    if (stopped || obsolete()) return false;
    for (const frame of events.events || []) applyFrame(frame);
    if (opts.backgroundPeerMonitor) return session.active_turn?.turn_id === turnId;
    const ownsSnapshot = !session.active_turn || session.active_turn.turn_id === turnId;
    const snapshotPending = !ownsSnapshot ? null :
      session.pending_approval?.approval_id
        ? { ...session.pending_approval, sessionId }
        : events.pending_approval?.approval_id
          ? { ...events.pending_approval, sessionId }
          : null;
    // A suppressed credential prompt must not end polling: returning here
    // used to kill the poller silently, freezing the thread mid-turn.
    if (snapshotPending && !isApprovalSuppressed(sessionId, snapshotPending)) {
      updateRun(sessionId, (r) => {
        if (shouldIgnoreStaleQuestionApproval(r, snapshotPending)) return r;
        return {
          ...r,
          waitingApproval: true,
          pendingApproval: snapshotPending,
          status: "waiting_approval",
          statusLabel: approvalStatusLabel(snapshotPending),
        };
      });
    }
    const stillActive =
      session.active_turn &&
      String(session.active_turn.turn_id || "") === turnId;
    if (stillActive && !opts.backgroundPeerMonitor) {
      updateRun(sessionId, (run) => {
        const connected = touchTransport(run, { mode: transportMode, turnId });
        if (run.waitingApproval || run.statusLabel !== "Reconnecting…") return connected;
        const phase = String(session.active_turn?.phase || "");
        const label = phase === "planning" ? "Planning…"
          : session.active_turn?.status === "waiting_for_unlock" ? terminalRunState("waiting_for_unlock").label
          : "Working…";
        return {
          ...connected,
          statusLabel: label,
          messages: connected.messages.map((m) =>
            m.id === assistantId && m.statusLabel === "Reconnecting…"
              ? { ...m, statusLabel: label }
              : m,
          ),
        };
      });
    }
    if (stillActive && session.active_turn?.speaker?.name) {
      updateRun(sessionId, run => ({ ...run, messages: run.messages.map(message =>
        message.id === assistantId ? { ...message, speaker: session.active_turn!.speaker } : message) }));
    }
    liveText = session.live_message !== undefined
      ? String(session.live_message)
      : liveText + String(session.live_message_delta || "");
    const live = liveText;
    if (stillActive && live) {
      set((s) => {
        const run = s.runs[sessionId];
        if (!run) return s;
        const messages = run.messages.map((m) =>
          m.id === assistantId ? applyLiveMessage(m, live) : m,
        );
        const nextRun = { ...run, messages };
        const sync = s.sessionId === sessionId ? syncFromRun(nextRun) : {};
        return { ...sync, runs: { ...s.runs, [sessionId]: nextRun } };
      });
    }
    return Boolean(stillActive);
  };
  const tick = async () => {
    if (stopped) return;
    try {
      const events = await api<{
        events?: EventFrame[];
        pending_approval?: PendingApproval | null;
        live?: SessionSnapshot;
      }>(
        `/api/sessions/${encodeURIComponent(sessionId)}/events` +
          `?thread_id=${encodeURIComponent(threadId)}` +
          `&turn_id=${encodeURIComponent(turnId)}` +
          `&after_sequence=${afterSeq}&include_state=1`,
        { signal: pollController.signal },
      );
      const session = events.live ?? await api<SessionSnapshot>(
        `/api/sessions/${encodeURIComponent(sessionId)}`,
        { signal: pollController.signal },
      );
      if (stopped || obsolete()) return;
      const stillActive = applyPacket(events, session);
      if (stillActive || (opts.backgroundPeerMonitor && !peerFinished)) {
        pollTimer = window.setTimeout(() => void tick(), POLL_MS);
      } else if (!opts.backgroundPeerMonitor) {
        await finalize(session);
      }
    } catch {
      if (stopped || obsolete()) return;
      if (!opts.backgroundPeerMonitor) updateRun(sessionId, run => ({ ...run, statusLabel: "Connection lost · checking task status" }));
      pollTimer = window.setTimeout(() => void tick(), POLL_MS_RETRY);
    }
  };

  /** Pages through the events missed since `afterSeq`, in order and once each. */
  const replay = async (): Promise<{ turn?: ReplayTurn | null; live?: SessionSnapshot; running: boolean | null } | null> => {
    let turn: ReplayTurn | null | undefined;
    let live: SessionSnapshot | undefined;
    for (let page = 0; page < REPLAY_MAX_PAGES; page++) {
      const from = afterSeq;
      const observedAt = Date.now();
      const packet = await api<LivePacket & { next_after?: number; has_more?: boolean; turn?: ReplayTurn | null }>(
        `/api/sessions/${encodeURIComponent(sessionId)}/events` +
          `?thread_id=${encodeURIComponent(threadId)}` +
          `&turn_id=${encodeURIComponent(turnId)}` +
          `&after=${from}&limit=${REPLAY_PAGE}&include_state=1`,
        { signal: pollController.signal },
      );
      if (stopped || obsolete()) return null;
      // applyFrame skips sequences at or below the cursor, so overlap is applied once.
      if (packet.live) applyPacket(packet, packet.live);
      else for (const frame of packet.events || []) applyFrame(frame);
      if (stopped || obsolete()) return null;
      const next = Number(packet.next_after);
      if (Number.isSafeInteger(next) && next > afterSeq) afterSeq = next;
      if (packet.turn !== undefined) turn = packet.turn;
      if (packet.live) live = packet.live;
      deps.onServerTurn?.(sessionId, serverTurnFromReplay(packet.turn, observedAt));
      if (packet.has_more !== true || afterSeq <= from) break;
    }
    const running = turn && typeof turn.running === "boolean"
      ? turn.running
      : live
        ? String(live.active_turn?.turn_id || "") === turnId
        : null;
    return { turn, live, running };
  };

  const scheduleRecover = () => {
    window.clearTimeout(reconnectTimer);
    reconnectTimer = window.setTimeout(() => void recover(), reconnectDelay(attempt++));
  };

  /**
   * After a stream error or a close before `done`: replay what was missed,
   * finalize if the server says the turn ended, else resume streaming with backoff.
   */
  const recover = async () => {
    if (stopped || obsolete()) return;
    window.clearTimeout(reconnectTimer);
    transportMode = "polling";
    updateRun(sessionId, (run) => touchTransport(run, { mode: transportMode, lastSequence: afterSeq, turnId }));
    let result: Awaited<ReturnType<typeof replay>>;
    try {
      result = await replay();
    } catch {
      if (stopped || obsolete()) return;
      updateRun(sessionId, run => ({ ...run, statusLabel: "Connection lost · checking task status" }));
      scheduleRecover();
      return;
    }
    if (!result || stopped || obsolete()) return;
    if (result.running === false) {
      const live = result.live;
      const finalText = live && live.active_turn?.turn_id !== turnId ? String(live.live_message || "") : "";
      if (finalText) {
        updateRun(sessionId, (run) => ({ ...run, messages: run.messages.map((m) =>
          m.id === assistantId && m.streaming ? applyLiveMessage(m, finalText) : m) }));
      }
      const otherActive = live?.active_turn && String(live.active_turn.turn_id || "") !== turnId ? live.active_turn : null;
      const requested = live?.requested_turn?.turn_id === turnId ? live.requested_turn : null;
      await finalize({
        ...(live || {}),
        active_turn: otherActive,
        requested_turn: {
          turn_id: turnId,
          status: String(result.turn?.status || requested?.status || ""),
          phase: String(requested?.phase || result.turn?.native_status || ""),
        },
      });
      return;
    }
    window.clearTimeout(reconnectTimer);
    reconnectTimer = window.setTimeout(openStream, reconnectDelay(attempt++));
  };

  const openStream = () => {
    if (stopped || obsolete()) return;
    const controller = new AbortController();
    streamController = controller;
    transportMode = "sse";
    const url = `/api/sessions/${encodeURIComponent(sessionId)}/events/stream`
      + `?thread_id=${encodeURIComponent(threadId)}&turn_id=${encodeURIComponent(turnId)}&after=${afterSeq}`;
    let stream: Promise<void>;
    try {
      stream = Promise.resolve(streamSessionEvents(url, (raw) => {
        if (stopped || controller.signal.aborted) return;
        attempt = 0;
        const packet = raw as LivePacket;
        if (packet.live) {
          const active = applyPacket(packet, packet.live);
          if (!active && packet.live.requested_turn) void finalize(packet.live);
        }
      }, controller.signal));
    } catch (error) {
      stream = Promise.reject(error);
    }
    stream.then(async () => {
      if (!stopped && !controller.signal.aborted) await finalize();
    }).catch(() => {
      if (stopped || obsolete() || controller.signal.aborted) return;
      void recover();
    });
  };

  if (opts.initialSnapshot) applyPacket({ events: [] }, opts.initialSnapshot);

  const replayFirst = opts.replayFirst
    ?? Boolean(reuseMessage && !opts.initialSnapshot && afterSeq > 0);
  if (opts.backgroundPeerMonitor) {
    void tick();
  } else if (replayFirst) {
    void recover();
  } else {
    openStream();
  }

  // Caller can stop the poll loop early (e.g. on unmount / session switch /
  // starting a fresh reattach for this session). Only mark Cancelled when
  // Stop was pressed — a handoff to a new poller must not finalize as cancelled.
  return (opts?: { cancelled?: boolean }) => {
    stopped = true;
    window.clearTimeout(pollTimer);
    window.clearTimeout(reconnectTimer);
    pollController.abort();
    streamController.abort();
    if (opts?.cancelled) cancelled = true;
  };
}
