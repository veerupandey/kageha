import type {
  AgentMode,
  ChatMessage,
  ComputerFrame,
  PendingApproval,
  RunStatus,
  SessionRun,
  TodoBoard,
  SubagentGraphNode,
  SessionSummary,
  ToolCard,
  TransportState,
} from "../api/types";
import { applyLiveNarrativeSnapshot, applyTranscriptToMessage, attachAskHumanPending, resolveAskHumanBlock } from "../lib/transcript";
import { captureSteeringBoundary } from "../lib/steering";
import type { AppState } from "./types";

export const LAST_SESSION_KEY = "kageha.lastSessionId";
export const MODE_SLASH_RE = /^\/(plan|goal|normal)\b/i;
export const AGENT_MODES: AgentMode[] = ["normal", "plan", "goal"];

export function uid(prefix = "m"): string {
  return `${prefix}_${Math.random().toString(36).slice(2, 10)}`;
}

/** Acknowledgement from the controller when it consumes a user correction. */
export function acknowledgeSteering(run: SessionRun, message: string, receiptId?: string): SessionRun {
  const index = run.messages.findIndex((m) => {
    if (m.role !== "user" || !m.steering) return false;
    if (receiptId && m.steeringId) {
      // A refresh can restore the receipt before live replay reaches its position.
      return receiptId === m.steeringId && (m.steering === "pending" || !m.steeringBoundary);
    }
    return m.steering === "pending" && (m.steeringText || m.text) === message;
  });
  if (index < 0) return run;
  const assistant = run.messages.findLast((m) => m.role === "assistant" && m.streaming);
  return { ...run, messages: run.messages.map((m, i) => i === index ? { ...m, steering: "applied",
    turnId: m.turnId || assistant?.turnId || run.transport?.turnId,
    steeringBoundary: m.steeringBoundary || captureSteeringBoundary(assistant),
  } : m) };
}

export function sessionFileUrl(sessionId: string, relPath: string): string {
  const encoded = String(relPath || "")
    .replace(/\\/g, "/")
    .split("/")
    .filter(Boolean)
    .map(encodeURIComponent)
    .join("/");
  return `/api/sessions/${encodeURIComponent(sessionId)}/files/${encoded}`;
}

export function rememberSession(id: string | null) {
  try {
    if (id) localStorage.setItem(LAST_SESSION_KEY, id);
    else localStorage.removeItem(LAST_SESSION_KEY);
  } catch {
    /* ignore */
  }
}

export function loadLastSession(): string | null {
  try {
    return localStorage.getItem(LAST_SESSION_KEY);
  } catch {
    return null;
  }
}

export const SESSIONS_CACHE_KEY = "kageha.sessions.cache";

export function saveCachedSessions(sessions: SessionSummary[]): void {
  try {
    localStorage.setItem(SESSIONS_CACHE_KEY, JSON.stringify(sessions.slice(0, 40)));
  } catch {
    /* ignore */
  }
}

export function loadCachedSessions(): SessionSummary[] {
  try {
    const raw = localStorage.getItem(SESSIONS_CACHE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as SessionSummary[]) : [];
  } catch {
    return [];
  }
}

export async function apiSoft(
  path: string,
  options: RequestInit = {},
): Promise<{ ok: boolean; status: number; data: Record<string, unknown> }> {
  try {
    const headers = new Headers(options.headers || {});
    const isForm =
      typeof FormData !== "undefined" && options.body instanceof FormData;
    if (!isForm && options.body && !headers.has("Content-Type")) {
      headers.set("Content-Type", "application/json");
    }
    const res = await fetch(path, { ...options, headers });
    const data = (await res.json().catch(() => ({}))) as Record<
      string,
      unknown
    >;
    return { ok: res.ok, status: res.status, data };
  } catch {
    return { ok: false, status: 0, data: {} };
  }
}

export function parseTodoBoardPayload(
  payload: Record<string, unknown> | TodoBoard | null | undefined,
): TodoBoard | null {
  if (!payload || typeof payload !== "object") return null;
  const rawItems = Array.isArray(payload.items) ? payload.items : [];
  const items = rawItems.map((it, i) => {
    const row = it && typeof it === "object" ? (it as Record<string, unknown>) : {};
    const evidence = typeof row.evidence === "string" ? row.evidence.trim() : "";
    return {
      id: String(row.id || `t${i}`),
      text: String(row.text || ""),
      done: row.done === true,
      ...(typeof row.status === "string" ? { status: row.status } : {}),
      ...(Array.isArray(row.outcome_ids) ? { outcome_ids: row.outcome_ids.filter((id): id is string => typeof id === "string") } : {}),
      ...(Array.isArray(row.depends_on) ? { depends_on: row.depends_on.filter((id): id is string => typeof id === "string") } : {}),
      ...(evidence ? { evidence } : {}),
    };
  });
  const total = typeof payload.total === "number" ? payload.total : items.length;
  if (total <= 0 && items.length === 0) return null;
  const notice = typeof payload.notice === "string" ? payload.notice.trim() : "";
  const version =
    typeof payload.version === "number"
      ? payload.version
      : typeof payload.version === "string" && payload.version.trim()
        ? Number(payload.version)
        : undefined;
  return {
    done:
      typeof payload.done === "number"
        ? payload.done
        : items.filter((item) => item.done).length,
    total,
    items,
    ...(typeof payload.objective === "string" ? { objective: payload.objective } : {}),
    ...(notice ? { notice } : {}),
    ...(version && Number.isFinite(version) ? { version } : {}),
    ...(typeof payload.revision === "number" ? { revision: payload.revision } : {}),
    ...(typeof payload.turn_id === "string" ? { turn_id: payload.turn_id } : {}),
    ...(typeof payload.current_stage_id === "string" ? { current_stage_id: payload.current_stage_id } : {}),
    ...(typeof payload.complete === "boolean" ? { complete: payload.complete } : {}),
    ...(typeof payload.verified === "number" ? { verified: payload.verified } : {}),
    ...(typeof payload.outcome_total === "number" ? { outcome_total: payload.outcome_total } : {}),
    ...(Array.isArray(payload.outcomes) ? { outcomes: payload.outcomes.filter((it) => it !== null && typeof it === "object").map((it) => {
      const row = it as Record<string, unknown>;
      return { id: String(row.id || ""), text: String(row.text || ""), done: row.done === true, evidence: String(row.evidence || ""),
        ...(typeof row.status === "string" ? { status: row.status } : {}) };
    }) } : {}),
  };
}

/** Older frames and snapshots cannot overwrite a newer task revision. */
export function mergeTodoBoard(current: TodoBoard | null, incoming: TodoBoard | null, turnId?: string): TodoBoard | null {
  if (!incoming) return current;
  if (turnId && incoming.turn_id !== turnId) return current;
  if (current?.turn_id && incoming.turn_id && current.turn_id !== incoming.turn_id) return incoming;
  if (current?.revision != null) {
    if (current.turn_id === incoming.turn_id && incoming.revision != null && incoming.revision < current.revision) return current;
    if (incoming.revision == null && (incoming.version ?? 0) <= (current.version ?? 0)) return current;
  }
  return incoming;
}

/** User-requested Stop must not be revived by visibility/online/tab recovery. */
export function shouldSkipLiveRecovery(run: SessionRun | undefined | null): boolean {
  if (!run) return false;
  if (run.status === "cancelled") return true;
  return Boolean(run.abort && !run.abort.signal.aborted && run.sending);
}

export function isPlanApproval(pending: PendingApproval | null | undefined): boolean {
  if (!pending) return false;
  return pending.risk_class === "plan" || pending.action === "approve_plan";
}

export function isContinueApproval(pending: PendingApproval | null | undefined): boolean {
  if (!pending) return false;
  return String(pending.action || "").startsWith("continue_after_");
}

export function approvalStatusLabel(pending: PendingApproval | null | undefined): string {
  if (isPlanApproval(pending)) return "Awaiting Build";
  if (isContinueApproval(pending)) return "Continue?";
  return "Approval needed";
}

export function isQuestionApproval(pending: PendingApproval | null | undefined): boolean {
  if (!pending) return false;
  if (isCredentialApproval(pending)) return false;
  return pending.risk_class === "hitl" || pending.action === "ask_human";
}

export function isCredentialApproval(pending: PendingApproval | null | undefined): boolean {
  if (!pending) return false;
  return pending.action === "request_credential" || pending.secret === true;
}

const resolvingApprovals = new Set<string>();
const resolvedApprovals = new Set<string>();
function decisionKey(sessionId: string | null | undefined, approvalId: string) {
  return `kageha:resolved-approval:${JSON.stringify([sessionId || "", approvalId])}`;
}
export function suppressResolvedApproval(sessionId: string | null | undefined, approvalId: string) {
  if (!approvalId) return;
  const key = decisionKey(sessionId, approvalId);
  resolvedApprovals.add(key);
  try { sessionStorage.setItem(key, "1"); } catch { /* Memory fallback. */ }
}
export function beginApprovalResolution(sessionId: string | null | undefined, approvalId: string) {
  const pending = { approval_id: approvalId };
  if (isApprovalSuppressed(sessionId, pending)) return false;
  resolvingApprovals.add(decisionKey(sessionId, approvalId));
  return true;
}
export function finishApprovalResolution(sessionId: string | null | undefined, approvalId: string, success: boolean) {
  resolvingApprovals.delete(decisionKey(sessionId, approvalId));
  if (success) suppressResolvedApproval(sessionId, approvalId);
}
export function isApprovalSuppressed(sessionId: string | null | undefined, pending: PendingApproval | null | undefined): boolean {
  if (!pending?.approval_id) return false;
  const key = decisionKey(sessionId || pending.sessionId || pending.session_id, pending.approval_id);
  if (resolvingApprovals.has(key) || resolvedApprovals.has(key)) return true;
  try { if (sessionStorage.getItem(key) === "1") return true; } catch { /* Memory fallback. */ }
  return isCredentialApprovalSuppressed(sessionId, pending);
}

const submittedCredentialApprovals = new Set<string>();

function approvalSuppressionKey(
  sessionId: string | null | undefined,
  approvalId: string | null | undefined,
): string {
  return `kageha:submitted-credential:${JSON.stringify([sessionId || "", approvalId || ""])}`;
}

export function suppressCredentialApproval(
  sessionId: string | null | undefined,
  approvalId: string | null | undefined,
): void {
  if (!approvalId) return;
  const key = approvalSuppressionKey(sessionId, approvalId);
  submittedCredentialApprovals.add(key);
  try {
    // Only an approval receipt is persisted, never a credential or vault handle.
    sessionStorage.setItem(key, "1");
  } catch { /* Storage may be unavailable; keep in-memory replay protection. */ }
}

export function isCredentialApprovalSuppressed(
  sessionId: string | null | undefined,
  pending: PendingApproval | null | undefined,
): boolean {
  if (!pending?.approval_id) return false;
  const key = approvalSuppressionKey(sessionId || pending.sessionId || pending.session_id, pending.approval_id);
  if (submittedCredentialApprovals.has(key)) return true;
  try {
    return sessionStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

/** Manus running ≠ waiting: ignore a stale ask after the user already replied. */
export function shouldIgnoreStaleQuestionApproval(
  run: SessionRun,
  pending: PendingApproval | null | undefined,
): boolean {
  return Boolean(
    pending &&
      isQuestionApproval(pending) &&
      (run.sending || run.status === "running") &&
      !run.waitingApproval,
  );
}

function todoBoardsEqual(
  left: TodoBoard | null | undefined,
  right: TodoBoard | null | undefined,
): boolean {
  if (left === right) return true;
  if (!left || !right) return false;
  return (
    left.revision === right.revision &&
    left.turn_id === right.turn_id &&
    left.complete === right.complete &&
    JSON.stringify(left.outcomes) === JSON.stringify(right.outcomes) &&
    left.done === right.done &&
    left.total === right.total &&
    (left.notice || "") === (right.notice || "") &&
    (left.version ?? 0) === (right.version ?? 0) &&
    left.items.length === right.items.length &&
    left.items.every(
      (item, index) =>
        item.id === right.items[index]?.id &&
        item.text === right.items[index]?.text &&
        item.done === right.items[index]?.done &&
        item.status === right.items[index]?.status &&
        (item.evidence || "") === (right.items[index]?.evidence || ""),
    )
  );
}

export interface LiveSessionSnapshot {
  messages?: Array<{ role?: string; text?: string; turn_id?: string; history_index?: number; job_reply_id?: string; message_id?: string; attachments?: string[] }>;
  pending_approval?: PendingApproval | null;
  todo_board?: Record<string, unknown> | TodoBoard | null;
  live_message?: string;
}

/** Reconcile saved rows without discarding hydrated activity or local sends. */
export function reconcileHistoryMessages(run: SessionRun, rows: Parameters<typeof mapHistoryMessages>[0]): SessionRun {
  if (!rows) return run;
  const used = new Set<string>();
  const key = (m: ChatMessage) => m.steeringId ? `steer:${m.steeringId}`
    : m.historyIndex !== undefined ? `legacy:${m.historyIndex}`
    : m.turnId ? `turn:${m.turnId}:${m.role}:${m.steering || ""}` : `id:${m.id}`;
  const known = new Map(run.messages.map(m => [key(m), m]));
  const messages = mapHistoryMessages(rows).map(saved => {
    const prior = known.get(key(saved)) || run.messages.find(m => !used.has(m.id) && (
      m.id === saved.id || (!m.turnId && !m.streaming && m.role === saved.role && m.text === saved.text && !m.steering)));
    if (!prior) return saved;
    used.add(prior.id);
    // The live consumer owns in-flight narrative. Settlement applies the final text.
    if (prior.streaming) return prior;
    if (prior.text === saved.text && prior.turnId === saved.turnId &&
        prior.resultStatus === saved.resultStatus && prior.steering === saved.steering &&
        JSON.stringify(prior.attachments) === JSON.stringify(saved.attachments) &&
        JSON.stringify(prior.resume) === JSON.stringify(saved.resume) &&
        JSON.stringify(prior.speaker) === JSON.stringify(saved.speaker)) return prior;
    return { ...(prior.text !== saved.text && prior.role === "assistant"
      ? applyTranscriptToMessage(prior, { type: "finish", assembled: saved.text }) : prior), ...saved, id: prior.id,
      // A missing field in an older snapshot must not erase journal metadata.
      speaker: saved.speaker || prior.speaker,
      steeringBoundary: prior.steeringBoundary || saved.steeringBoundary };
  });
  // Retain pending sends and older rows outside the server's bounded history.
  const unmatched = run.messages.filter(m => !used.has(m.id));
  const pending = unmatched.filter(m => m.streaming || (!m.turnId && m.role === "user"));
  const older = unmatched.filter(m => !pending.includes(m));
  const next = [...older, ...messages, ...pending];
  return next.length === run.messages.length && next.every((m, i) => m === run.messages[i])
    ? run : { ...run, messages: next };
}

/**
 * Merge the durable session snapshot into a live run. Refresh uses this same
 * payload; SSE heartbeats can keep a stream "alive" without ever delivering
 * the plan/approval frames, so the live UI must read it too.
 */
export function applyLiveSessionSnapshot(
  run: SessionRun,
  snapshot: LiveSessionSnapshot,
  sessionId: string,
): SessionRun {
  const pendingRaw = snapshot.pending_approval;
  let pending =
    pendingRaw && String(pendingRaw.approval_id || "").trim()
      ? { ...pendingRaw, sessionId }
      : null;
  const incomingBoard = parseTodoBoardPayload(snapshot.todo_board);
  const todoBoard = run.sending && !run.transport?.turnId && !isPlanApproval(pending) ? run.todoBoard
    : mergeTodoBoard(run.todoBoard, incomingBoard, run.sending ? run.transport?.turnId : undefined);
  const live = String(snapshot.live_message || "");

  let messages = run.messages;
  let changed = false;
  const history = snapshot.messages || [];
  const matchesHistoryRow = (message: ChatMessage, row: NonNullable<LiveSessionSnapshot["messages"]>[number]) =>
    message.id === row.job_reply_id ||
    (row.history_index !== undefined && message.historyIndex === row.history_index) ||
    Boolean(row.turn_id && message.turnId === row.turn_id && message.role === row.role);
  for (let index = 0; index < history.length; index++) {
    const row = history[index];
    if (!row.job_reply_id || messages.some((message) => message.id === row.job_reply_id)) continue;
    const foreground = row.job_reply_id.startsWith("foreground:");
    // Foreground replies already have a streamed row with a different local ID.
    if (foreground && messages.some((message) => matchesHistoryRow(message, row))) continue;
    const reply = mapHistoryMessages([row])[0];
    if (!reply) continue;
    let insertAt = messages.length;
    if (foreground) {
      // Restore older replies before their answers, not below the active turn.
      for (const later of history.slice(index + 1)) {
        const anchor = messages.findIndex((message) => matchesHistoryRow(message, later));
        if (anchor >= 0) { insertAt = anchor; break; }
      }
    }
    messages = [...messages.slice(0, insertAt), reply, ...messages.slice(insertAt)];
    changed = true;
  }

  if (live) {
    let lastIdx = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === "assistant" && messages[i].streaming) {
        lastIdx = i;
        break;
      }
    }
    if (lastIdx >= 0 && messages[lastIdx].text !== live) {
      messages = messages.map((message, index) =>
        index === lastIdx
          ? applyLiveNarrativeSnapshot(message, live)
          : message,
      );
      changed = true;
    }
  }

  if (shouldIgnoreStaleQuestionApproval(run, pending)) {
    pending = null;
  }
  if (isApprovalSuppressed(sessionId, pending)) {
    pending = null;
  }

  if (pending) {
    const label = approvalStatusLabel(pending);
    const already =
      !changed &&
      run.waitingApproval &&
      run.status === "waiting_approval" &&
      run.statusLabel === label &&
      run.pendingApproval?.approval_id === pending.approval_id &&
      (!todoBoard || todoBoardsEqual(run.todoBoard, todoBoard));
    if (already) {
      const streaming = run.messages.find(
        (message) => message.role === "assistant" && message.streaming,
      );
      if (
        streaming?.statusLabel === label &&
        streaming.approval?.approval_id === pending.approval_id
      ) {
        return run;
      }
    }
    messages = messages.map((message, index) => {
      if (message.role !== "assistant") return message;
      const isStreamingTarget = Boolean(message.streaming);
      const isLastAssistant =
        index ===
        messages.reduce((last, item, idx) => (item.role === "assistant" ? idx : last), -1);
      if (!isStreamingTarget && !isLastAssistant) return message;
      if (
        message.statusLabel === label &&
        message.approval?.approval_id === pending.approval_id
      ) {
        return message;
      }
      return isQuestionApproval(pending)
        ? attachAskHumanPending({ ...message, statusLabel: label, approval: pending }, pending)
        : { ...message, statusLabel: label, approval: pending };
    });
    return {
      ...run,
      messages,
      todoBoard: todoBoard ?? run.todoBoard,
      waitingApproval: true,
      pendingApproval: pending,
      status: "waiting_approval",
      statusLabel: label,
    };
  }

  if (snapshot.pending_approval === null && run.waitingApproval) {
    const approvalId = run.pendingApproval?.approval_id;
    const status = run.sending ? "running" : run.status === "waiting_approval" ? "idle" : run.status;
    const label = run.sending ? "Working…" : "Ready";
    messages = messages.map(message => message.approval?.approval_id === approvalId && approvalId
      ? { ...resolveAskHumanBlock(message, approvalId), approval: undefined, statusLabel: label } : message);
    return { ...run, messages, todoBoard: todoBoard ?? run.todoBoard, waitingApproval: false,
      pendingApproval: null, status, statusLabel: label };
  }

  if (todoBoard && !todoBoardsEqual(run.todoBoard, todoBoard)) {
    return { ...run, messages, todoBoard };
  }

  if (!changed) return run;
  return { ...run, messages };
}

/** Preserve backend step evidence. A successful reply cannot finish a step. */
export function settleTodoBoardOnTurnEnd(
  board: TodoBoard | null | undefined,
  opts: { status: string; mode: AgentMode; awaiting?: boolean },
): TodoBoard | null {
  void opts;
  return board ?? null;
}

/** Freeze the last streaming assistant and mark the run cancelled (Stop). */
export function cancelInFlightRun(run: SessionRun): SessionRun {
  let lastIdx = -1;
  for (let i = run.messages.length - 1; i >= 0; i--) {
    const message = run.messages[i];
    if (message.role === "assistant" && message.streaming) {
      lastIdx = i;
      break;
    }
  }
  const messages =
    lastIdx < 0
      ? run.messages
      : run.messages.map((message, i) =>
          i === lastIdx
            ? {
                ...applyTranscriptToMessage(message, {
                  type: "finish",
                  assembled: message.text,
                }),
                streaming: false,
                statusLabel: "Cancelled",
              }
            : message,
        );
  return {
    ...run,
    messages,
    sending: false,
    abort: null,
    waitingApproval: false,
    status: "cancelled",
    statusLabel: "Cancelled",
  };
}

export function emptyRun(
  sessionId: string,
  threadId: string | null = null,
  personaId = "main",
  modelOverride = "",
): SessionRun {
  return {
    sessionId,
    threadId,
    machineActive: false,
    remoteActive: false,
    remoteHostId: "",
    remoteHostName: "",
    messages: [],
    sending: false,
    status: "idle",
    statusLabel: "Ready",
    queue: [],
    abort: null,
    waitingApproval: false,
    needsAttention: false,
    pendingFiles: [],
    todoBoard: null,
    pendingApproval: null,
    operatingState: {
      activeSkills: [],
      nativePacks: [],
    },
    personaId,
    modelOverride,
    transport: emptyTransport(),
  };
}

/** Keep the home-screen Machine chip when the first send mints a session. */
export function stampPendingMachine(
  run: SessionRun,
  machineActive: boolean,
): SessionRun {
  if (!machineActive) return run;
  if (run.machineActive) return run;
  return { ...run, machineActive: true };
}

/** Keep a home-screen Remote selection when the first send mints a session. */
export function stampPendingRemote(
  run: SessionRun,
  remoteActive: boolean,
  remoteHostId: string,
  remoteHostName: string,
): SessionRun {
  if (!remoteActive) return run;
  if (run.remoteActive) return run;
  return { ...run, remoteActive: true, remoteHostId, remoteHostName };
}

export function emptyTransport(): TransportState {
  return {
    mode: "sse",
    lastSequence: 0,
    lastFrameAt: 0,
    reconnectCount: 0,
    turnId: "",
  };
}

export function touchTransport(
  run: SessionRun,
  patch: Partial<TransportState>,
  now = Date.now(),
): SessionRun {
  const prev = run.transport || emptyTransport();
  const nextTurnId =
    patch.turnId !== undefined ? String(patch.turnId || "").trim() : String(prev.turnId || "").trim();
  const switched = Boolean(prev.turnId && nextTurnId && prev.turnId !== nextTurnId);
  return {
    ...run,
    transport: {
      ...prev,
      ...patch,
      turnId: nextTurnId,
      lastSequence:
        patch.lastSequence !== undefined
          ? patch.lastSequence
          : switched
            ? 0
            : prev.lastSequence,
      lastFrameAt: patch.lastFrameAt ?? now,
    },
  };
}

/** Reuse a journal cursor only when it belongs to the still-active turn. */
export function recoveryAfterSequence(
  transport: TransportState | undefined | null,
  activeTurnId: string,
): number | undefined {
  const turnId = String(activeTurnId || "").trim();
  const owned = String(transport?.turnId || "").trim();
  if (!turnId || !owned || owned !== turnId) return undefined;
  const seq = Number(transport?.lastSequence || 0);
  return seq > 0 ? seq : undefined;
}

function pickFirst(...vals: unknown[]): unknown {
  for (const v of vals) {
    if (v == null) continue;
    if (typeof v === "string" && !v.trim()) continue;
    return v;
  }
  return undefined;
}

export function normalizeToolCard(
  raw: Record<string, unknown>,
  kind?: string,
): ToolCard | null {
  const nested =
    raw.tool_card && typeof raw.tool_card === "object"
      ? (raw.tool_card as Record<string, unknown>)
      : raw;
  const name = pickFirst(
    nested.name,
    nested.tool,
    nested.tool_name,
    nested.toolName,
    raw.name,
    raw.tool,
    raw.tool_name,
  );
  if (!name) return null;
  const durationMs = Number(
    pickFirst(
      nested.duration_ms,
      nested.durationMs,
      nested.elapsed_ms,
      nested.ms,
      raw.duration_ms,
    ),
  );
  const artifactRefs = pickFirst(
    nested.artifact_refs,
    nested.artifactRefs,
    nested.artifacts,
    nested.paths,
    raw.artifact_refs,
  );
  const refs = Array.isArray(artifactRefs)
    ? artifactRefs
        .map((a) =>
          typeof a === "string"
            ? a
            : String(
                (a as { path?: string; url?: string })?.path ||
                  (a as { url?: string })?.url ||
                  "",
              ),
        )
        .filter(Boolean)
    : [];
  const status = String(
    pickFirst(
      nested.status,
      nested.state,
      raw.status,
      kind === "tool_completed"
        ? "ok"
        : kind === "tool_started"
          ? "running"
          : "",
    ) || "running",
  ).toLowerCase();
  const resultPreview = pickFirst(
    nested.result_preview,
    nested.resultPreview,
    nested.result,
    nested.output_preview,
    raw.result_preview,
  );
  const argsPreview = pickFirst(
    nested.args_preview,
    nested.argsPreview,
    nested.args,
    nested.arguments_preview,
    nested.preview,
    raw.args_preview,
  );
  const errorPreview = pickFirst(
    nested.error_preview,
    nested.errorPreview,
    nested.error,
    raw.error_preview,
  );
  const subagentGraph = normalizeSubagentGraph(
    pickFirst(nested.subagent_graph, nested.subagentGraph, raw.subagent_graph),
  );
  const target = nested.browserTarget as Partial<NonNullable<ToolCard["browserTarget"]>> | undefined;
  const source = nested.observedSource as Partial<NonNullable<ToolCard["observedSource"]>> | undefined;
  const research = nested.researchRecovery as Partial<NonNullable<ToolCard["researchRecovery"]>> | undefined;
  let connector = nested.connectorOutcome as ToolCard["connectorOutcome"];
  if (!connector && name === "integration_read" && typeof resultPreview === "string") {
    try {
      const result = JSON.parse(resultPreview);
      if (["agent_not_assigned", "needs_verification", "disconnected"].includes(result.status))
        connector = { connector: "gmail", state: result.status, operation: "status", recovery: "manage_access",
          turnId: String(nested.turn_id || raw.turn_id || "") || undefined };
    } catch { /* Historical previews can be truncated. */ }
  }
  return {
    id: String(
        pickFirst(nested.id, nested.card_id, nested.attempt_id, raw.id, raw.attempt_id, name) ||
        name,
    ),
    name: String(name),
    argsPreview: argsPreview != null ? String(argsPreview).slice(0, 400) : "",
    status,
    durationMs: Number.isFinite(durationMs) ? durationMs : null,
    artifactRefs: refs.slice(0, 8),
    resultPreview:
      resultPreview != null ? String(resultPreview).slice(0, 800) : "",
    outputPreview: resultPreview != null ? String(resultPreview).slice(0, 800) : "",
    errorPreview: errorPreview != null ? String(errorPreview).slice(0, 400) : "",
    attemptId: String(pickFirst(nested.attempt_id, nested.attemptId, raw.attempt_id) || "") || undefined,
    sideEffect: String(pickFirst(nested.side_effect, nested.sideEffect, raw.side_effect) || "") || undefined,
    policyGrant: String(pickFirst(nested.policy_grant, nested.policyGrant, raw.policy_grant) || "") || undefined,
    ...(typeof target?.browserId === "string" && target.browserId && typeof target?.tabId === "string" && target.tabId
      ? { browserTarget: { browserId: target.browserId, tabId: target.tabId } } : {}),
    ...(typeof source?.url === "string" && ["done", "ok", "success", "completed"].includes(status)
      ? { observedSource: { url: source.url, title: typeof source.title === "string" ? source.title : undefined } } : {}),
    ...(name === "web_fetch" && research?.recovered === true && ["done", "ok", "success", "completed"].includes(status)
      ? { researchRecovery: { recovered: true as const, reason: typeof research.reason === "string" ? research.reason.slice(0, 100) : undefined,
          attemptedBackends: Array.isArray(research.attemptedBackends) ? research.attemptedBackends.filter((name): name is string => typeof name === "string" && ["http", "chromium"].includes(name)) : [] } } : {}),
    ...(subagentGraph.length ? { subagentGraph } : {}),
    ...(connector?.connector === "gmail" ? { connectorOutcome: connector } : {}),
  };
}

function normalizeSubagentGraph(value: unknown): SubagentGraphNode[] {
  if (!Array.isArray(value)) return [];
  const nodes: SubagentGraphNode[] = [];
  for (const item of value.slice(0, 24)) {
    if (!item || typeof item !== "object") continue;
    const row = item as Record<string, unknown>;
    const id = String(row.id || "").trim();
    if (!id) continue;
    nodes.push({
      id,
      task: String(row.task || ""),
      ...(row.name ? { name: String(row.name) } : {}),
      ...(typeof row.job_id === "string" ? { job_id: row.job_id } : {}),
      ...(Array.isArray(row.depends_on) ? { depends_on: row.depends_on.filter((d): d is string => typeof d === "string") } : {}),
      ...(typeof row.status === "string" ? { status: row.status } : {}),
      ...(row.result ? { result: String(row.result) } : {}),
      ...(row.error ? { error: String(row.error) } : {}),
    });
  }
  return nodes;
}

export function normalizeComputerFrame(
  raw: Record<string, unknown>,
  sessionId: string | null,
): ComputerFrame | null {
  const nested =
    raw.computer_frame && typeof raw.computer_frame === "object"
      ? (raw.computer_frame as Record<string, unknown>)
      : raw.frame && typeof raw.frame === "object"
        ? (raw.frame as Record<string, unknown>)
        : raw.thumb && typeof raw.thumb === "object"
          ? (raw.thumb as Record<string, unknown>)
          : raw;
  const path = pickFirst(
    nested.thumb_path,
    nested.thumbPath,
    nested.thumb,
    nested.path,
    nested.artifact,
    nested.artifact_path,
    nested.artifactPath,
    nested.rel_path,
    nested.file,
    raw.thumb_path,
    raw.path,
  );
  const url = pickFirst(
    nested.thumb_url,
    nested.thumbUrl,
    nested.url,
    nested.src,
    nested.thumbnail_url,
    nested.image_url,
    raw.thumb_url,
    raw.url,
  );
  const b64 = pickFirst(
    nested.thumb_b64,
    nested.b64,
    nested.base64,
    nested.thumbnail_b64,
    raw.thumb_b64,
  );
  const sid =
    sessionId ||
    String(
      pickFirst(
        nested.session_id,
        nested.sessionId,
        raw.session_id,
        raw.sessionId,
      ) || "",
    ) ||
    null;
  let resolvedUrl = url ? String(url) : "";
  if (!resolvedUrl && path) {
    const p = String(path);
    if (/^https?:\/\//i.test(p) || p.startsWith("/api/")) resolvedUrl = p;
    else if (sid) {
      resolvedUrl = sessionFileUrl(sid, p);
    } else {
      resolvedUrl = p;
    }
  }
  if (!resolvedUrl && b64) {
    const mime = String(
      pickFirst(nested.mime, nested.content_type, "image/png"),
    );
    resolvedUrl = `data:${mime};base64,${String(b64).replace(/^data:[^;]+;base64,/, "")}`;
  }
  if (!resolvedUrl) return null;
  const action = pickFirst(
    nested.action,
    nested.label,
    nested.tool,
    raw.action,
    raw.label,
  );
  const app = pickFirst(nested.app, nested.application, nested.window, raw.app);
  return {
    url: resolvedUrl,
    path: path ? String(path) : "",
    action: action ? String(action) : "",
    app: app ? String(app) : "",
    caption: [app, action].filter(Boolean).join(" · ") || undefined,
  };
}

export function parseModeSlash(text: string): AgentMode | null {
  const m = MODE_SLASH_RE.exec(String(text || "").trim());
  if (!m) return null;
  const mode = m[1].toLowerCase() as AgentMode;
  return AGENT_MODES.includes(mode) ? mode : null;
}

export function isModeOnlyComposerText(text: string): boolean {
  const t = String(text || "").trim();
  return Boolean(
    parseModeSlash(t) && t.replace(MODE_SLASH_RE, "").trim() === "",
  );
}

const COMPUTER_ADMIN_ACTIONS = new Set([
  "status",
  "doctor",
  "pack",
  "allowlist",
  "apps",
  "list",
  "allow",
  "deny",
  "clear",
  "help",
  "-h",
  "--help",
  "?",
]);

/** Bare `/computer` or pack admin verbs — not a computer_use skill task. */
export function isComputerAdminSlash(text: string): boolean {
  const t = String(text || "").trim();
  const low = t.toLowerCase();
  if (low !== "/computer" && !low.startsWith("/computer ")) return false;
  const parts = t.split(/\s+/).filter(Boolean);
  if (parts.length <= 1) return true;
  return COMPUTER_ADMIN_ACTIONS.has(parts[1].toLowerCase());
}

const MACHINE_ADMIN_ACTIONS = new Set([
  "status",
  "doctor",
  "pack",
  "stop",
  "help",
  "-h",
  "--help",
  "?",
]);

/** Bare `/machine` or pack admin verbs — not a task-machine turn. */
export function isMachineAdminSlash(text: string): boolean {
  const t = String(text || "").trim();
  const low = t.toLowerCase();
  if (low !== "/machine" && !low.startsWith("/machine ")) return false;
  const parts = t.split(/\s+/).filter(Boolean);
  if (parts.length <= 1) return true;
  return MACHINE_ADMIN_ACTIONS.has(parts[1].toLowerCase());
}

/**
 * Slash that should never mint a chat session / agent turn by itself:
 * mode-only, bare skill prefixes, computer/browser/machine admin.
 */
export function isMetaOnlySlash(text: string): boolean {
  const t = String(text || "").trim();
  if (!t.startsWith("/")) return false;
  if (isModeOnlyComposerText(t)) return true;
  if (isComputerAdminSlash(t)) return true;
  if (isMachineAdminSlash(t)) return true;
  const low = t.toLowerCase();
  if (
    low === "/browser" ||
    low.startsWith("/browser ") ||
    low === "/comet" ||
    low.startsWith("/comet ")
  ) {
    return true;
  }
  // Bare skill invoke with no task body: `/computer_use` or `/web_research`
  if (/^\/[a-z][a-z0-9_-]*$/i.test(t) && !isModeOnlyComposerText(t)) {
    const id = t.slice(1).toLowerCase();
    if (
      id === "new" ||
      id === "task" ||
      id === "ask" ||
      id === "auto" ||
      id === "model" ||
      id === "permissions" ||
      id === "artifacts"
    ) {
      return false;
    }
    return true;
  }
  return false;
}

/** Recovery instructions are agent-only context, never part of the chat transcript. */
export function isInternalRecoveryMessage(text: string | undefined): boolean {
  const message = String(text || "").trim();
  return (
    message.startsWith("Continue the interrupted request:") &&
    message.includes("\n\nClarification question:") &&
    message.includes("User's answer:")
  );
}

export function mapHistoryMessages(
  rows: Array<{ speaker?: ChatMessage["speaker"]; role?: string; text?: string; turn_id?: string; result_status?: string; resume?: ChatMessage["resume"] | null; history_index?: number; job_reply_id?: string; message_id?: string; attachments?: string[]; steering?: "pending" | "applied"; steering_boundary?: ChatMessage["steeringBoundary"] }> | undefined,
): ChatMessage[] {
  return (rows || []).filter((m) => !isInternalRecoveryMessage(m.text)).map((m) => ({
    id: m.job_reply_id || uid(),
    speaker: m.speaker,
    role: m.role === "assistant" || m.role === "system" ? m.role : "user",
    text: m.text || "",
    turnId: String(m.turn_id || "").trim() || undefined,
    ...(m.result_status ? { resultStatus: m.result_status } : {}),
    ...(m.resume ? { resume: m.resume } : {}),
    historyIndex: m.history_index,
    steering: m.steering,
    steeringId: m.message_id,
    steeringBoundary: m.steering_boundary,
    steeringText: m.steering ? m.text : undefined,
    attachments: Array.isArray(m.attachments)
      ? m.attachments.map((path) => String(path || "").trim()).filter(Boolean)
      : undefined,
  }));
}

export function syncFromRun(
  run: SessionRun | undefined,
  extras: Partial<AppState> = {},
): Partial<AppState> {
  if (!run) {
    return {
      messages: [],
      sending: false,
      runStatus: "idle",
      statusLabel: "Ready",
      abort: null,
      pendingFiles: [],
      threadId: null,
      pendingApproval: null,
      modelOverride: "",
      ...extras,
    };
  }
  return {
    messages: run.messages,
    sending: run.sending,
    runStatus: run.waitingApproval ? "waiting_approval" : run.status,
    statusLabel: run.statusLabel,
    abort: run.abort,
    pendingFiles: run.pendingFiles,
    machineActive: run.machineActive,
    remoteActive: run.remoteActive,
    remoteHostId: run.remoteHostId,
    remoteHostName: run.remoteHostName,
    // Model pin is session-scoped so a pick made in one tab never bleeds
    // into another tab's next turn on switch (falls back to "" = Auto).
    modelOverride: run.modelOverride || "",
    liveSplit: Boolean(run.machineActive),
    // Task status is turn-scoped, not persisted on SessionRun — always reset
    // on session switch so a stale "blocked" chip never survives a swap.
    taskControl: "",
    taskControlReason: "",
    taskFailureKind: "",
    taskFailureChange: "",
    taskFailureCount: 0,
    taskStageId: "",
    taskStageStatus: "",
    taskChronicTool: "",
    taskChronicCount: 0,
    ...(run.machineActive
      ? {}
      : {
          machineBlocked: false,
          machineDetail: "",
          machinePaused: false,
          machineShellRunning: false,
          machineShellLine: "",
          machineNovncUrl: "",
          machineAdopted: false,
          machineBootPhase: "",
          machineBrowserReady: false,
          desktopLeaseOwner: "",
        }),
    ...(run.remoteActive
      ? {}
      : {
          remoteHealth: "",
          remoteDetail: "",
        }),
    todoBoard: run.todoBoard,
    threadId: run.threadId,
    pendingApproval: run.pendingApproval,
    ...extras,
  };
}

export type { RunStatus };

export { earlyStopLabel, isEarlyStopLabel } from "../lib/runStatus";
