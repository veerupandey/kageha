import { readKnowledgeSelection } from "../lib/knowledgeSelection";
import { terminalRunState } from "../lib/runStatus";
import { createClientId } from "../lib/clientId";
import { refreshWorkers } from "../lib/workerConnection";
import { mergeTerminalNarrative } from "../lib/narrativeText";
import { streamChat } from "../api/stream";
import { StreamDroppedError } from "../api/stream";
import { loopEventDetail } from "../lib/loopEvents";
import { ApiError, api } from "../api/client";
import type {
  AgentMode,
  ChatMessage,
  PendingApproval,
  RunStatus,
  SessionRun,
} from "../api/types";
import { applyStreamFrameToMessage, applyTranscriptToMessage, attachAskHumanPending, replayStreamFrames, toolChipSummary } from "../lib/transcript";
import { pendingApprovalFieldsFromPayload } from "../lib/askHumanApproval";
import { pinnedComposerModel } from "../lib/modelCatalog";
import { activityTarget, isBrowserToolName, isComputerToolName, isMachineShellToolName, wantsLiveBrowser, wantsLiveMachine } from "../lib/activityUi";
import { isMachineDesktopApp, machineLiveStateFromPayload, resolveLiveSurface } from "../lib/liveSurface";
import { taskStatusFromPayload } from "../lib/taskStatus";
import { shouldAgentFocus } from "../lib/agentFocus";
import { speakText } from "../lib/voiceClient";
import { appendActivityStep, asDetailLines } from "./activity";
import {
  approvalStatusLabel,
  acknowledgeSteering,
  isApprovalSuppressed,
  suppressResolvedApproval,
  isMachineAdminSlash,
  isQuestionApproval,
  normalizeComputerFrame,
  normalizeToolCard,
  emptyTransport,
  parseTodoBoardPayload,
  mergeTodoBoard,
  rememberSession,
  settleTodoBoardOnTurnEnd,
  syncFromRun,
  touchTransport,
  uid,
  earlyStopLabel,
} from "./helpers";
import type { AppState } from "./types";
import { provisionalSessionTitle } from "./sessions";
import { recoverReceipt } from "../api/receipts";
import { rememberDelivery, acknowledgeDelivery, markDelivery } from "./delivery";

type SetState = (
  partial:
    | Partial<AppState>
    | ((state: AppState) => Partial<AppState> | AppState),
) => void;
type GetState = () => AppState;

const MODE_MAX_STEPS = { normal: 128, plan: 256, goal: 512, multitask: 192 } as const;

export function revealLiveSurface(
  get: GetState,
  set: SetState,
  sessionId: string,
  incoming: "computer" | "browser" | "machine",
) {
  if (sessionId !== get().sessionId) return;
  const firstFrameOfTurn = !get().liveRevealedThisTurn;
  const allow = shouldAgentFocus({
    pinnedAt: get().surfacePinnedAt,
    controlling: get().surfaceControlling,
    now: Date.now(),
    firstFrameOfTurn,
  });
  set({ liveRevealedThisTurn: true });
  if (!allow) return;
  const next = resolveLiveSurface(get().liveSurface, incoming, get().machineActive);
  if (next !== get().liveSurface || get().workspaceSurface !== "live") get().setLiveSurface(next);
}

/** Isolated desktop owns the right Live panel (ComputerStage + Take control). */
export function attachMachineLivePanel(
  get: GetState,
  set: SetState,
  updateRun: (
    sessionId: string,
    updater: (run: SessionRun) => SessionRun,
  ) => void,
  sessionId: string,
  payload: Record<string, unknown> = {},
) {
  const next = machineLiveStateFromPayload(payload);
  updateRun(sessionId, (run) => ({ ...run, machineActive: next.machineActive }));
  if (sessionId !== get().sessionId) return;
  set({
    ...next,
    liveRevealedThisTurn: true,
  });
  if (!next.machineActive) return;
  revealLiveSurface(get, set, sessionId, "machine");
}

export type RunTurnDeps = {
  set: SetState;
  get: GetState;
  updateRun: (
    sessionId: string,
    updater: (run: SessionRun) => SessionRun,
  ) => void;
  patchAssistant: (
    sessionId: string,
    assistantId: string,
    patch: Partial<ChatMessage>,
  ) => void;
  patchAssistantNarrative: (
    sessionId: string,
    assistantId: string,
    assembled: string,
  ) => void;
  flushStreamPatches: () => void;
  flushQueue: (sessionId: string) => Promise<void>;
  /** Reattach to a still-running backend turn (reload / mid-stream recovery). */
  reattach: (
    sessionId: string,
    threadId: string,
    turnId: string,
    opts?: {
      assistantId?: string;
      pendingApproval?: PendingApproval | null;
      backgroundPeerMonitor?: boolean;
      afterSequence?: number;
    },
  ) => void;
};

/** Heartbeats must not hide a live tool/status label with "Still working…". */
const TRANSIENT_PULSE = /^(still\s+)?(working|connecting|starting)[.…]*$/i;

function remapIdleWorking(label: string): string {
  const compact = String(label || "").trim();
  if (TRANSIENT_PULSE.test(compact)) return compact;
  return label;
}

function liveStatusLabel(
  current: string | undefined,
  incoming: string,
  heartbeat: boolean,
): string {
  const mapped = remapIdleWorking(incoming);
  if (!heartbeat) return mapped;
  const cur = String(current || "").trim();
  if (cur && !TRANSIENT_PULSE.test(cur)) return cur;
  return mapped;
}

export type TurnOptions = {
  clientRequestId?: string;
  autoBuild?: boolean; agentMode?: AgentMode; resumeTurnId?: string;
  /** "Resume task": continue the latest request from its checkpoint (sends `resume: true`). */
  resume?: boolean;
  recoveryMode?: "recheck" | "continue"; recoveryOutcomeIds?: string[];
};

export async function runTurn(
  deps: RunTurnDeps,
  sessionId: string,
  threadId: string,
  text: string,
  attachments: string[],
  displayText: string,
  opts: TurnOptions = {},
): Promise<void> {
  const { set, get, updateRun, patchAssistant, patchAssistantNarrative, flushStreamPatches, flushQueue, reattach } = deps;
  if (get().sessionId === sessionId) rememberSession(sessionId);
  const assistantId = uid("a");
  const clientRequestId = opts.clientRequestId ?? createClientId();
  const abort = new AbortController();
  const ownsTurn = () => get().runs[sessionId]?.abort === abort && !abort.signal.aborted;
  let peerMonitorStarted = false;
  let lastJournalSequence = 0;
  const userMsg: ChatMessage = {
    id: uid("u"),
    role: "user",
    text: displayText || text || "(attachments)",
    attachments: attachments.length ? attachments.slice() : undefined,
  };
  const mentioned = text.trimStart().match(/^@([A-Za-z0-9][A-Za-z0-9._-]{0,79})(?=\s|$)/)?.[1];
  const mentionedTeam = text.trimStart().match(/^&([A-Za-z0-9][A-Za-z0-9._-]{0,79})(?=\s|$)/)?.[1];
  // File references share the @ syntax. Only known personas select a speaker;
  // the backend resolves unknown mentions against project files and personas.
  const mentionedPersona = mentioned && get().personas?.some(item => item.id === mentioned) ? mentioned : undefined;
  const previousRun = get().runs[sessionId];
  // A selected team starts a new conversation. Existing conversations retain
  // their pinned identity; an explicit agent mention takes precedence.
  const teamId = !mentionedPersona && !opts.resume && !previousRun?.messages.some(message =>
    message.role === "user" || message.role === "assistant")
    ? mentionedTeam || get().activeTeamId || undefined : undefined;
  let coordinator = teamId ? get().teams?.find(team => team.id === teamId)?.agents[0] : undefined;
  let recipientId = mentionedPersona || coordinator || get().activePersonaId || "main";
  const persona = get().personas?.find(item => item.id === recipientId);
  const speaker = { id: recipientId, name: recipientId === "main" ? "Kageha" : persona?.name || recipientId, avatar: persona?.avatar || "" };
  const mode = opts.agentMode || get().agentMode;
  if (coordinator && get().sessionId === sessionId) set({ activePersonaId: coordinator });
  // Credential answers use their dedicated secure endpoint, never this outbox.
  // Omit text for any explicitly secret approval supplied by legacy callers.
  const recoverySaved = rememberDelivery({ id: clientRequestId, sessionId,
    text: get().pendingApproval?.secret ? "" : text, createdAt: Date.now(), state: "pending" });
  if (!recoverySaved) get().showToast("Browser recovery storage is unavailable. Keep this tab open until delivery is confirmed.");

  updateRun(sessionId, (run) => ({
    ...run,
    personaId: get().activePersonaId || run.personaId || "main",
    sending: true,
    clientRequestId,
    abort,
    startedAt: Date.now(),
    status: "running",
    statusLabel: "Sending request…",
    waitingApproval: false,
    needsAttention: false,
    todoBoard: { done: 0, total: 0, items: [], objective: userMsg.text, notice: "Preparing this request…" },
    transport: emptyTransport(),
    messages: [
      ...run.messages,
      userMsg,
      {
        id: assistantId,
        role: "assistant",
        speaker,
        text: "",
        streaming: true,
        statusLabel: "Sending request…",
        steps: [],
        toolCards: [],
        computerFrames: [],
        blocks: [],
      },
    ],
  }));
  set({ error: null, ...(opts.resume || opts.resumeTurnId ? {} : { draft: "" }), canvasTurnPaths: new Set(), liveRevealedThisTurn: false });
  get().setRemoteExecution(null);

  // Explicit computer/browser requests should reveal the matching live
  // surface immediately. For ordinary tasks we wait for an actual tool event
  // so mentioning a browser in a research prompt does not steal the panel.
  const explicitComputer = /(?:^|\s)\/(?:computer|desktop)\b/i.test(text);
  const explicitBrowser = wantsLiveBrowser(text);
  const explicitMachine =
    /(?:^|\s)\/machine\b/i.test(text) && !isMachineAdminSlash(text);
  const isolatedDesktop = explicitMachine || wantsLiveMachine(text);
  // The selected chat's current choice wins over a cached run snapshot.
  // In particular, false must survive a machine -> host switch.
  const selected = get();
  const selectedMachine = selected.sessionId === sessionId
    ? selected.machineActive : previousRun?.machineActive;
  const selectedRemote = selected.sessionId === sessionId
    ? selected.remoteActive : previousRun?.remoteActive;
  const requestedMachine = Boolean(selectedMachine || isolatedDesktop);
  const executionTarget = requestedMachine ? "machine" : selectedRemote ? "remote" : "local";
  const remoteHostId = selected.sessionId === sessionId ? selected.remoteHostId : previousRun?.remoteHostId;
  if (explicitComputer && !isolatedDesktop) get().pinLiveSurface("computer");
  else if (isolatedDesktop) {
    set({ machineActive: true, machineBlocked: false, machineDetail: "" });
    updateRun(sessionId, (run) => ({ ...run, machineActive: true }));
    get().setLiveSplit(true);
    get().pinLiveSurface("computer");
  } else if (get().machineActive) {
    get().setLiveSplit(true);
  } else if (explicitBrowser) {
    get().pinLiveSurface("browser");
  }

  // Set a provisional title from the user's message immediately (don't wait for done event).
  // This prevents showing session IDs like "4a4b00b0" in the header/sidebar while running.
  if (!get().sessionTitle && displayText) {
    const provisional = provisionalSessionTitle(displayText);
    if (provisional.length >= 3) {
      set((s) => ({
        sessionTitle: provisional,
        sessions: s.sessions.map((session) =>
          session.session_id === sessionId
            ? { ...session, title: provisional }
            : session,
        ),
      }));
    }
  }

  const prefs = get().prefs;
  const model =
    pinnedComposerModel(
      get().modelOverride,
      prefs.mainModel,
      get().defaultModel,
    ) || undefined;
  const effortRaw = String(prefs.reasoningEffort || "").trim().toLowerCase();
  const effort =
    effortRaw === "low" || effortRaw === "medium" || effortRaw === "high"
      ? effortRaw
      : undefined;
  const model_roles = {
    planner: prefs.plannerModel.trim(),
    // Composer pick is the driving agent. Settings worker is fallback only.
    executor: (model || prefs.workerModel).trim(),
  };
  const knowledge_bases = readKnowledgeSelection();
  // The Active_Persona is resolved once at session creation (Requirement 2.4)
  // and the backend pins it thereafter regardless of what's sent on
  // follow-up turns (Requirement 2.5) — always sending the current
  // activePersonaId is therefore safe for both the first and later turns.

  // The search/vision selection made in the composer menu and Settings > Search
  // is persisted to the backend search config (searchSetup), NOT to prefs.*
  // (nothing writes prefs.webSearch/prefs.vision). Send the actually-configured
  // backend so the per-turn capability route honors the user's pick (e.g.
  // antigravity) instead of always defaulting to "auto".
  const searchSetup = get().searchSetup;
  const configuredWebSearch =
    searchSetup?.configured || prefs.webSearch || "auto";
  const configuredVision =
    searchSetup?.vision?.configured || prefs.vision || "auto";

  try {
    // Render the local pending message before potentially slow team discovery.
    // Once discovery completes, update its speaker without creating another message.
    if (teamId && !get().teams?.some(team => team.id === teamId)) {
      await get().refreshTeams?.();
      if (!ownsTurn()) return;
      coordinator = get().teams?.find(team => team.id === teamId)?.agents[0];
      recipientId = mentionedPersona || coordinator || get().activePersonaId || "main";
      const resolvedPersona = get().personas?.find(item => item.id === recipientId);
      const resolvedSpeaker = { id: recipientId, name: recipientId === "main" ? "Kageha" : resolvedPersona?.name || recipientId, avatar: resolvedPersona?.avatar || "" };
      if (coordinator && get().sessionId === sessionId) set({ activePersonaId: coordinator });
      updateRun(sessionId, run => ({ ...run, personaId: get().activePersonaId || run.personaId || "main",
        messages: run.messages.map(message => message.id === assistantId ? { ...message, speaker: resolvedSpeaker } : message) }));
    }
    const agent_id = get().activePersonaId || "main";
    // Reveal the machine workspace as soon as the backend confirms activation.
    if (requestedMachine) {
      if (get().desktopLeaseOwner === "user" || get().surfaceControlling) {
        await get().releaseDesktopControl?.();
        if (!ownsTurn()) return;
      }
      await api(`/api/sessions/${encodeURIComponent(sessionId)}/machine`, {
        method: "POST",
        body: JSON.stringify({ active: true }),
        signal: abort.signal,
      });
      if (!ownsTurn()) return;
    }
    const done = await streamChat(
      {
        thread_id: threadId,
        client_request_id: clientRequestId,
        session_id: sessionId,
        message: text,
        ...(opts.resume ? { resume: true } : {}),
        resume_turn_id: opts.resumeTurnId,
        ...(opts.recoveryMode ? { recovery_mode: opts.recoveryMode, recovery_outcome_ids: opts.recoveryOutcomeIds } : {}),
        attachments: attachments.length ? attachments : undefined,
        auto_approve: get().autoApprove,
        auto_build: opts.autoBuild,
        agent_mode: mode,
        machine_active: requestedMachine,
        execution_target: executionTarget,
        remote_host_id: executionTarget === "remote" ? remoteHostId : undefined,
        model,
        effort,
        media_model: prefs.mediaModel || "auto",
        capability_preferences: {
          vision: configuredVision,
          web_search: configuredWebSearch,
          image_generation: prefs.imageGeneration !== "auto" ? prefs.imageGeneration : (prefs.mediaModel || "auto"),
          tts: prefs.tts || "auto",
          tts_voice: prefs.ttsVoice || "",
          stt: prefs.stt || "auto",
        },
        model_roles,
        knowledge_bases: knowledge_bases.length ? knowledge_bases : undefined,
        agent_id,
        team_id: teamId,
        recipient_id: recipientId,
        loop_mode: mode !== "normal" ? "full" : undefined,
        max_steps: MODE_MAX_STEPS[mode],
      },
      {
        onStatus: (label, data) => {
          if (!ownsTurn()) return;
          const expectedTurnId = get().runs[sessionId]?.transport?.turnId;
          if (data.turn_id && expectedTurnId && data.turn_id !== expectedTurnId) return;
          const detail = asDetailLines(data, 2);
          const statusDetail = detail[0] || undefined;
          const phase = String(data.phase || "");
          const seq = Number(data.sequence);
          const frameTurnId = String(data.turn_id || "").trim();
          if (data.request_id === clientRequestId) markDelivery(clientRequestId, "received");
          if (frameTurnId) acknowledgeDelivery(clientRequestId);
          const heartbeat = phase === "heartbeat";
          updateRun(sessionId, (r) =>
            touchTransport(
              {
                ...r,
                messages: frameTurnId && frameTurnId !== r.transport?.turnId ? r.messages.map((m) =>
                  m.id === userMsg.id || m.id === assistantId ? { ...m, turnId: frameTurnId } : m) : r.messages,
                status: r.waitingApproval ? "waiting_approval" : "running",
                statusLabel: liveStatusLabel(r.statusLabel, label, heartbeat),
              },
              {
                mode: "sse",
                lastSequence: Number.isFinite(seq) && seq > 0 ? seq : r.transport?.lastSequence,
                turnId: frameTurnId || r.transport?.turnId,
              },
            ),
          );
          // Status always updates the live pulse. Notable phases also enter
          // the transcript so refresh/reattach keep the same surface.
          set((s) => {
            const run = s.runs[sessionId];
            if (!run) return s;
            const waiting = run.waitingApproval;
            const messages = run.messages.map((m) => {
              if (m.id !== assistantId) return m;
              const nextLabel = liveStatusLabel(m.statusLabel, label, heartbeat);
              const nextMsg = {
                ...m,
                statusLabel: nextLabel,
                statusDetail: heartbeat ? detail[0] || m.statusDetail : statusDetail,
              };
              if (heartbeat) {
                return applyStreamFrameToMessage(nextMsg, {
                  kind: "heartbeat",
                  label: nextLabel,
                  detail,
                });
              }
              return applyStreamFrameToMessage(nextMsg, {
                kind: phase || "status",
                label,
                detail,
                interesting: true,
              });
            });
            const next = {
              ...run,
              messages,
              statusLabel: liveStatusLabel(run.statusLabel, label, heartbeat),
              status: (waiting ? "waiting_approval" : "running") as RunStatus,
            };
            const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
            return {
              ...sync,
              runs: { ...s.runs, [sessionId]: next },
            };
          });
        },
        onDelta: (assembled) => {
          if (!ownsTurn()) return;
          patchAssistantNarrative(sessionId, assistantId, assembled);
        },
        onMessage: (full) => {
          if (!ownsTurn()) return;
          flushStreamPatches();
          updateRun(sessionId, (run) => ({
            ...run,
            messages: run.messages.map((message) =>
              message.id === assistantId
                ? replayStreamFrames(message, [], full)
                : message,
            ),
          }));
        },
        onToolCard: (data) => {
          if (!ownsTurn()) return;
          flushStreamPatches();
          const card = normalizeToolCard(data, "tool_card");
          if (!card) return;
          const jobIds = card.subagentGraph?.flatMap(node => node.job_id ? [node.job_id] : []) || [];
          if (jobIds.length) refreshWorkers(sessionId, jobIds);
          if (sessionId === get().sessionId && isComputerToolName(card.name)) {
            if (get().machineActive) revealLiveSurface(get, set, sessionId, "machine");
          } else if (sessionId === get().sessionId && isBrowserToolName(card.name)) {
            revealLiveSurface(get, set, sessionId, "browser");
          } else if (sessionId === get().sessionId && get().machineActive && isMachineShellToolName(card.name)) {
            revealLiveSurface(get, set, sessionId, "machine");
          }
          if (card.artifactRefs?.length && sessionId === get().sessionId) {
            // Canvas is session-scoped top-level state — only populate it for
            // the active session. Background turns are rehydrated via
            // refreshArtifacts() when the user switches back.
            get().upsertCanvasPaths(card.artifactRefs);
          }
          set((s) => {
            const run = s.runs[sessionId];
            if (!run) return s;
            const messages = run.messages.map((m) => {
              if (m.id !== assistantId) return m;
              const cards = [...(m.toolCards || [])];
              const idx = cards.findIndex((c) => c.id === card.id);
              if (idx >= 0) cards[idx] = { ...cards[idx], ...card };
              else cards.push(card);
              return applyStreamFrameToMessage(
                { ...m, toolCards: cards.slice(-24) },
                { kind: "tool_card", interesting: true, card },
              );
            });
            const next = { ...run, messages };
            const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
            return {
              ...sync,
              runs: { ...s.runs, [sessionId]: next },
            };
          });
        },
        onComputerFrame: (data) => {
          if (!ownsTurn()) return;
          const frame = normalizeComputerFrame(data, sessionId);
          if (!frame) return;
          if (sessionId === get().sessionId && frame.app === "Kageha Browser") {
            revealLiveSurface(get, set, sessionId, "browser");
          } else if (sessionId === get().sessionId && isMachineDesktopApp(frame.app)) {
            attachMachineLivePanel(get, set, updateRun, sessionId);
          } else if (sessionId === get().sessionId && get().machineActive && frame.app) {
            revealLiveSurface(get, set, sessionId, "machine");
          } else if (sessionId === get().sessionId && frame.app) {
            revealLiveSurface(get, set, sessionId, "computer");
          }
          // Computer captures stay in the live frames strip — not Canvas.
          set((s) => {
            const run = s.runs[sessionId];
            if (!run) return s;
            const messages = run.messages.map((m) => {
              if (m.id !== assistantId) return m;
              const frames = [...(m.computerFrames || []), frame].slice(-12);
              return { ...m, computerFrames: frames };
            });
            const next = { ...run, messages };
            const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
            return {
              ...sync,
              runs: { ...s.runs, [sessionId]: next },
            };
          });
        },
        onEvent: (data) => {
          if (!ownsTurn()) return;
          const expectedTurnId = get().runs[sessionId]?.transport?.turnId;
          if (data.turn_id && expectedTurnId && data.turn_id !== expectedTurnId) return;
          const sequence = typeof data.sequence === "number" && Number.isSafeInteger(data.sequence)
            && data.sequence > 0 ? data.sequence : undefined;
          if (sequence !== undefined && sequence <= lastJournalSequence) return;
          if (sequence !== undefined) lastJournalSequence = sequence;
          // Narrative tokens are batched until the next animation frame.
          // Commit them before adding a tool/thought boundary so they stay
          // in order even when several SSE frames arrive in one read.
          flushStreamPatches();
          const kind = String(data.kind || "");
          const payload =
            data.payload && typeof data.payload === "object"
              ? (data.payload as Record<string, unknown>)
              : {};
          const sourceKind = String(payload.source_kind || kind);
          const isBudgetTelemetry = kind === "request_budget" || sourceKind === "request_budget";
          if (isBudgetTelemetry) {
            set((s) => {
              const run = s.runs[sessionId];
              if (!run) return s;
              const messages = run.messages.map((m) => {
                const bound = data.turn_id && data.turn_id !== run.transport?.turnId &&
                  (m.id === userMsg.id || m.id === assistantId)
                  ? { ...m, turnId: String(data.turn_id) } : m;
                return m.id === assistantId ? applyStreamFrameToMessage(bound, {
                  sequence,
                  turn_id: typeof data.turn_id === "string" ? data.turn_id : undefined,
                  kind,
                  payload,
                }) : bound;
              });
              const next = typeof data.sequence === "number" || data.turn_id
                ? touchTransport({ ...run, messages }, {
                    mode: "sse",
                    lastSequence: typeof data.sequence === "number" ? data.sequence : run.transport?.lastSequence,
                    turnId: String(data.turn_id || "").trim() || run.transport?.turnId,
                  })
                : { ...run, messages };
              const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
              return { ...sync, runs: { ...s.runs, [sessionId]: next } };
            });
            return;
          }
          if (sourceKind === "user_inject") {
            updateRun(sessionId, (run) => acknowledgeSteering(run, String(payload.message || ""), String(payload.message_id || "")));
          }
          const eventText = `${kind} ${data.label || ""} ${payload.tool_name || ""} ${payload.name || ""}`.toLowerCase();
          if (sourceKind === "task_status" && sessionId === get().sessionId) {
            get().setTaskStatus(taskStatusFromPayload(payload));
          }
          if (sourceKind !== "skill_autoload") {
            if (sourceKind.startsWith("peer_")) {
              const capabilityId = String(payload.capability_id || "").toLowerCase();
              const surface = capabilityId.includes("browser")
                ? "browser"
                : capabilityId.includes("desktop") || capabilityId.includes("computer")
                  ? "computer"
                  : "worker";
              const rawStatus = String(payload.state || (sourceKind === "peer_task_submitted" ? "requested" : "running"));
              const status = ["requested", "running", "completed", "failed", "canceled"].includes(rawStatus)
                ? rawStatus as "requested" | "running" | "completed" | "failed" | "canceled"
                : sourceKind === "peer_task_completed" ? "completed" : sourceKind === "peer_task_failed" ? "failed" : "running";
              if (sessionId === get().sessionId) {
                get().setRemoteExecution({
                  peerId: String(payload.peer_id || ""),
                  peerName: payload.peer_name ? String(payload.peer_name) : undefined,
                  taskId: String(payload.task_id || ""),
                  capabilityId,
                  surface,
                  status,
                });
                if (surface === "browser" || surface === "computer") revealLiveSurface(get, set, sessionId, surface);
              }
            }
            if (kind === "machine_starting" || kind === "machine_ready" || kind === "computer_frame" || kind === "artifact_thumb") {
              /* Live routing: machine_* attach below; frames go through onComputerFrame. */
            } else if (sessionId === get().sessionId && isComputerToolName(String(payload.tool_name || payload.name || "")) ) {
              if (get().machineActive) revealLiveSurface(get, set, sessionId, "machine");
            } else if (sessionId === get().sessionId && (isBrowserToolName(String(payload.tool_name || payload.name || "")) || activityTarget(eventText) === "browser")) {
              revealLiveSurface(get, set, sessionId, "browser");
            } else if (sessionId === get().sessionId && get().machineActive && (isMachineShellToolName(String(payload.tool_name || payload.name || "")) || activityTarget(eventText) === "machine")) {
              revealLiveSurface(get, set, sessionId, "machine");
            } else if (sessionId === get().sessionId && activityTarget(eventText) === "computer") {
              if (get().machineActive) revealLiveSurface(get, set, sessionId, "machine");
              else revealLiveSurface(get, set, sessionId, "computer");
            }
          }
          if (kind === "capability_preference_updated" && sessionId === get().sessionId && payload.capability === "image_generation") {
            const provider = String(payload.provider || "");
            if (["auto", "codex", "antigravity", "nano-banana", "fal", "cloudflare"].includes(provider)) {
              set({ prefs: { ...get().prefs, imageGeneration: provider as AppState["prefs"]["imageGeneration"], mediaModel: provider as AppState["prefs"]["mediaModel"] } });
            }
          }
          if (kind === "machine_starting" || kind === "machine_ready") {
            const skipped = payload.skipped === true;
            const message = String((payload.blocked === true && payload.error) || payload.message || payload.error || "");
            attachMachineLivePanel(get, set, updateRun, sessionId, payload);
            if (sessionId === get().sessionId) {
              if (kind === "machine_ready" && payload.blocked === true) {
                get().showToast(message || "Task machine blocked");
              } else if (kind === "machine_ready" && !skipped && message) {
                if (String(payload.phase || "") !== "browser") {
                  get().showToast(message);
                }
              }
            }
          }
          if (kind === "remote_ready") {
            // Remote (SSH) session boot: mirror the machine_ready pattern —
            // update the sticky remote health state (per-run + top-level) so the
            // thread-header badge reflects live connectivity, and surface a toast.
            {
              const ok = payload.ok !== false;
              const host = String(payload.host_name || payload.host || "");
              const blocked = !ok;
              const health = blocked ? "blocked" : "connected";
              const detail = blocked
                ? String(payload.error || "unreachable")
                : String(payload.host_name || payload.host || "");
              updateRun(sessionId, (run) => ({
                ...run,
                remoteActive: !blocked ? true : run.remoteActive,
                remoteHostName: host || run.remoteHostName,
              }));
              if (sessionId === get().sessionId) {
                set({
                  remoteHealth: health,
                  remoteDetail: detail,
                  ...(payload.risk_tier
                    ? { remoteRiskTier: String(payload.risk_tier) }
                    : {}),
                  ...(host ? { remoteHostName: host } : {}),
                });
                if (!ok) {
                  get().showToast(`Remote host unavailable: ${String(payload.error || "unreachable")}`);
                } else if (payload.browser_error) {
                  get().showToast(`Remote browser unavailable: ${String(payload.browser_error)}`);
                } else if (payload.desktop_error) {
                  get().showToast(`Remote desktop unavailable: ${String(payload.desktop_error)}`);
                } else if (payload.browser) {
                  get().showToast(`Remote browser ready on ${host}`);
                } else if (payload.desktop) {
                  get().showToast(`Remote desktop ready on ${host} (${String(payload.desktop)})`);
                } else if (host) {
                  get().showToast(`Remote host ready: ${host}`);
                }
              }
            }
          }
          if (sourceKind === "peer_task_submitted" && !peerMonitorStarted) {
            peerMonitorStarted = true;
            const peerTurnId = String(data.turn_id || "");
            if (peerTurnId) {
              reattach(sessionId, threadId, peerTurnId, {
                assistantId,
                backgroundPeerMonitor: true,
                afterSequence: typeof data.sequence === "number" ? data.sequence : undefined,
              });
            }
          }
          const isInternalProtocolKind = ["stream_item", "stream_patch", "delta", "model", "keepalive"].includes(kind);
          const activityCard = kind === "tool_card" ? normalizeToolCard(data, kind) : undefined;
          const label = String(data.label || (kind === "tool_card"
            ? (activityCard?.status === "running" ? toolChipSummary(activityCard) : "Working…")
            : isInternalProtocolKind ? "" : kind) || "");
          const listed = asDetailLines(data);
          const detail = listed.length ? listed : loopEventDetail(kind, payload);
          const interesting =
            data.interesting === undefined ? !isInternalProtocolKind : Boolean(data.interesting);
          const eventCard = label && !isInternalProtocolKind
            ? normalizeToolCard({ ...payload, ...data }, kind || "tool_card")
            : null;
          set((s) => {
            const run = s.runs[sessionId];
            if (!run) return s;
            const messages = run.messages.map((m) => {
              if (m.id !== assistantId) return m;
              const steps = label && interesting
                ? appendActivityStep(m.steps || [], {
                    label,
                    detail,
                    kind,
                    interesting,
                  })
                : m.steps || [];
              return applyStreamFrameToMessage(
                {
                  ...m,
                  steps,
                  ...(label ? { statusLabel: label, statusDetail: detail[0] || m.statusDetail } : {}),
                },
                {
                  sequence,
                  turn_id: typeof data.turn_id === "string" ? data.turn_id : undefined,
                  kind,
                  label,
                  detail,
                  interesting,
                  payload,
                  card: eventCard,
                },
              );
            });
            const next = {
              ...run,
              messages,
              ...(label ? { statusLabel: label } : {}),
              status: "running" as RunStatus,
            };
            const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
            return {
              ...sync,
              runs: { ...s.runs, [sessionId]: next },
            };
          });

          if (["run_start", "execution_profile", "response_budget", "skill_autoload", "capability_surface", "mcp_skipped", "mcp_connected", "codex_runtime"].includes(sourceKind)) {
            updateRun(sessionId, (run) => {
              const current = run.operatingState;
              const skills = Array.isArray(payload.skills)
                ? payload.skills.map(String)
                : current.activeSkills;
              const packs = Array.isArray(payload.native_packs)
                ? payload.native_packs.map(String)
                : current.nativePacks;
              const serverCount = Number(payload.servers || 0);
              const mcpStatus = sourceKind === "codex_runtime"
                ? `${String(payload.bridge || "native")} · ${Number(payload.bridge_tools || 0)} tools · ${Number(payload.apps || 0)} apps`
                : sourceKind === "mcp_skipped"
                ? `skipped · ${String(payload.reason || "not needed")}`
                : sourceKind === "mcp_connected"
                  ? `${serverCount} server${serverCount === 1 ? "" : "s"} · ${String(payload.tools || 0)} tools`
                  : current.mcpStatus;
              return {
                ...run,
                operatingState: {
                  ...current,
                  mode: (payload.agent_mode || current.mode) as AgentMode,
                  loopMode: String(payload.loop_mode || current.loopMode || ""),
                  effort: String(payload.effort || current.effort || ""),
                  maxTokens: typeof payload.max_tokens === "number" ? payload.max_tokens : current.maxTokens,
                  activeSkills: skills,
                  nativePacks: packs,
                  mcpStatus,
                },
              };
            });
          }

          if (["tool_card", "tool_started", "tool_result"].includes(sourceKind)) {
            const toolName = String(payload.tool || payload.tool_name || payload.name || "");
            if (toolName) {
              const route = toolName.startsWith("mcp_") || toolName.startsWith("mcp.")
                ? "mcp"
                : toolName.startsWith("skill_")
                  ? "skill"
                  : "native";
              updateRun(sessionId, (run) => ({
                ...run,
                operatingState: { ...run.operatingState, route, currentTool: toolName },
              }));
            }
          }

          // Mode is session-scoped UI state — never let a backgrounded
          // turn clobber the active session's mode. Todo boards belong on
          // the owning run even while the tab is parked.
          if (sessionId === get().sessionId && kind === "goal_qa_misfit") {
            const msg = String(
              payload.message || data.label || "This looks like Normal",
            );
            get().showToast(`${msg} — answering without Goal theater`);
            get().setAgentMode("normal");
          }
          if (sessionId === get().sessionId && kind === "step_watchdog") {
            get().showToast("Model call took too long — retrying automatically");
          }
          if (sessionId === get().sessionId && kind === "reasoning_loop") {
            get().showToast("Model repeated its reasoning — stopped this turn. Try another model.");
          }
          if (sessionId === get().sessionId && kind === "tool_guardrail") {
            const code = String(payload.code || "");
            if (code === "browser_hard_restart") {
              const failed = payload.restart_failed === true;
              get().showToast(
                failed
                  ? "Browser looked stuck — the automatic restart failed"
                  : "Browser looked stuck — restarted it automatically",
              );
            } else if (code === "browser_recovery_exhausted") {
              get().showToast(
                "Browser recovery gave up for this turn — it may need your help",
              );
            }
            // A guardrail halt that already halted in a PRIOR turn this
            // session (loop/task_state.py chronic_tool_halts, surfaced as
            // chronic_halt_count on the event) means the agent is repeating
            // the same class of mistake across turns, not stumbling once.
            // That is worth a toast the first time it's chronic (>=2) —
            // a plain per-turn halt already gets a TaskStatusChip update via
            // the next task_status event and does not need its own toast.
            const chronicCount = Number(payload.chronic_halt_count || 0);
            const toolName = String(payload.tool_name || "");
            if (chronicCount >= 2 && toolName) {
              get().setTaskChronicHalt(toolName, chronicCount);
              get().showToast(
                `${toolName} keeps failing the same way — ${chronicCount} turns in a row`,
              );
            } else if (chronicCount > 0 && toolName) {
              get().setTaskChronicHalt(toolName, chronicCount);
            }
          }
          if (kind === "todo_board") {
            const board = parseTodoBoardPayload(payload);
            if (board) updateRun(sessionId, (run) => ({ ...run, todoBoard: mergeTodoBoard(run.todoBoard, board, String(data.turn_id || run.transport?.turnId || "")) }));
          }
          if (typeof data.sequence === "number" || data.turn_id) {
            updateRun(sessionId, (run) =>
              touchTransport({ ...run, messages: data.turn_id && data.turn_id !== run.transport?.turnId ? run.messages.map((m) =>
                m.id === userMsg.id || m.id === assistantId ? { ...m, turnId: String(data.turn_id) } : m) : run.messages }, {
                mode: "sse",
                lastSequence:
                  typeof data.sequence === "number" ? (data.sequence as number) : run.transport?.lastSequence,
                turnId: String(data.turn_id || "").trim() || run.transport?.turnId,
              }),
            );
          }
          if (kind === "approval_required") {
            const approvalId = String(
              payload.approval_id || data.approval_id || "",
            );
            if (approvalId) {
              const pending: PendingApproval = {
                approval_id: approvalId,
                sessionId,
                action: String(payload.action || data.label || ""),
                ...(payload.profile_scoped === true ? {profile_scoped:true} : {}),
                risk_class: payload.risk_class
                  ? String(payload.risk_class)
                  : undefined,
                detail: payload.detail as string | string[] | undefined,
                label: payload.label ? String(payload.label) : undefined,
                secret: payload.secret === true,
                ...pendingApprovalFieldsFromPayload(payload),
              };
              if (!isApprovalSuppressed(sessionId, pending)) {
                const isPlanBuild =
                  pending.risk_class === "plan" ||
                  pending.action === "approve_plan";
                const waitLabel = approvalStatusLabel(pending);
                // Mode is session-scoped — never let a backgrounded turn flip
                // the active session's mode to plan.
                if (
                  isPlanBuild &&
                  sessionId === get().sessionId &&
                  get().agentMode === "normal"
                ) {
                  get().setAgentMode("plan");
                }
                updateRun(sessionId, (r) => ({
                  ...r,
                  waitingApproval: true,
                  status: "waiting_approval",
                  statusLabel: waitLabel,
                  pendingApproval: pending,
                  messages: r.messages.map((m) => {
                    if (m.id !== assistantId) return m;
                    const next = { ...m, approval: pending };
                    return isQuestionApproval(pending)
                      ? attachAskHumanPending(next, pending)
                      : next;
                  }),
                }));
                if (sessionId === get().sessionId) {
                  set({
                    pendingApproval: pending,
                    runStatus: "waiting_approval",
                    statusLabel: waitLabel,
                  });
                }
              }
            }
          } else if (kind === "approval_resolved") {
            suppressResolvedApproval(sessionId, String(payload.approval_id || ""));
            updateRun(sessionId, (r) => ({
              ...r,
              waitingApproval: false,
              pendingApproval: null,
            }));
            if (
              get().pendingApproval?.sessionId === sessionId ||
              get().pendingApproval?.session_id === sessionId
            ) {
              set({ pendingApproval: null });
            }
          }

          if (kind === "tool_card" || data.tool_card) {
            const card = normalizeToolCard(
              { ...payload, ...data },
              kind || "tool_card",
            );
            if (card) {
              set((s) => {
                const run = s.runs[sessionId];
                if (!run) return s;
                const messages = run.messages.map((m) => {
                  if (m.id !== assistantId) return m;
                  const cards = [...(m.toolCards || [])];
                  const idx = cards.findIndex((c) => c.id === card.id);
                  if (idx >= 0) cards[idx] = { ...cards[idx], ...card };
                  else cards.push(card);
                  return applyStreamFrameToMessage(
                    { ...m, toolCards: cards.slice(-24) },
                    {
                      kind,
                      label: String(data.label || m.statusLabel || ""),
                      interesting: true,
                      payload,
                      card,
                    },
                  );
                });
                const next = { ...run, messages };
                const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
                return {
                  ...sync,
                  runs: { ...s.runs, [sessionId]: next },
                };
              });
            }
          }
          if (
            kind === "computer_frame" ||
            kind === "artifact_thumb" ||
            data.computer_frame
          ) {
            const frame = normalizeComputerFrame(
              { ...payload, ...data },
              sessionId,
            );
            if (frame) {
              set((s) => {
                const run = s.runs[sessionId];
                if (!run) return s;
                const messages = run.messages.map((m) => {
                  if (m.id !== assistantId) return m;
                  const frames = [...(m.computerFrames || []), frame].slice(
                    -12,
                  );
                  return { ...m, computerFrames: frames };
                });
                const next = { ...run, messages };
                const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
                return {
                  ...sync,
                  runs: { ...s.runs, [sessionId]: next },
                };
              });
            }
          }
        },
      },
      { signal: abort.signal },
    );

    if (!ownsTurn()) return;
    acknowledgeDelivery(clientRequestId);
    flushStreamPatches();

    const proposal = done.skill_proposal && typeof done.skill_proposal === "object"
      ? done.skill_proposal as Record<string, unknown>
      : null;
    if (proposal?.approval_id && sessionId === get().sessionId) {
      set({
        pendingApproval: {
          approval_id: String(proposal.approval_id),
          sessionId,
          action: "save_skill",
          risk_class: "skill",
          label: `Save reusable skill: ${String(proposal.name || "learned workflow")}`,
          detail: `${String(proposal.reason || "This workflow may be reusable")}\n\n${String(proposal.content || "")}`,
          proposal_name: String(proposal.name || "learned-workflow"),
          proposal_content: String(proposal.content || ""),
        },
        runStatus: "waiting_approval",
        statusLabel: "Skill suggestion",
      });
    }

    const runAfter = get().runs[sessionId];
    const status = String(done.status || "unknown");
    if (done.speaker && typeof done.speaker === "object") patchAssistant(sessionId, assistantId, { speaker: done.speaker as ChatMessage["speaker"] });
    const isFailure = ["error", "invalid_output", "unverified", "failed", "unknown"].includes(status);
    const streamedText = runAfter?.messages.find((m) => m.id === assistantId)?.text || "";
    const finalText = isFailure
      ? mergeTerminalNarrative(streamedText, String(done.message || ""))
      : String(done.message || streamedText);
    const awaitingBuild = status === "awaiting_plan_approval";
    const awaitingClarify =
      status === "awaiting_clarify" || status === "ask_user";
    // Mode is session-scoped — don't let a just-finished background turn
    // flip the active session's mode to plan.
    if (
      awaitingBuild &&
      sessionId === get().sessionId &&
      get().agentMode === "normal"
    ) {
      get().setAgentMode("plan");
    }
    // no_progress / max_steps / budget / hitl: the agent stopped early.
    // Say so plainly instead of a misleading "Done".
    const stoppedLabel = earlyStopLabel(status);
    const terminal = terminalRunState(status);
    const doneLabel = terminal.label;
    const doneRunStatus = terminal.status;
    set((s) => {
      const run = s.runs[sessionId];
      if (!run) return s;
      const messages = run.messages.map((m) => {
        if (m.id !== assistantId) return m;
        return {
          ...replayStreamFrames(m, [], finalText),
          streaming: false,
          statusLabel: doneLabel,
          resultStatus: status,
          failureCause: isFailure ? String(done.failure_cause || m.failureCause || (["invalid_output", "unverified"].includes(status)
            ? "The response was delivered, but task completion could not be verified."
            : status === "unknown" ? "Completion could not be confirmed from the saved run status."
            : done.message || "The run failed.")) : undefined,
          failureDetail: isFailure && typeof done.failure_detail === "string" && done.failure_detail.trim()
            ? done.failure_detail : undefined,
          turnSteps: typeof done.steps === "number" ? done.steps : undefined,
          turnCostUsd: typeof done.spent_usd === "number" ? done.spent_usd : undefined,
          attachments: Array.isArray(done.turn_artifacts)
            ? done.turn_artifacts
                .map((path) => String(path || "").trim())
                .filter(Boolean)
            : m.attachments,
        };
      });
      const next = { ...run, messages };
      const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
      return { ...sync, runs: { ...s.runs, [sessionId]: next } };
    });
    updateRun(sessionId, (r) => ({
      ...r,
      sending: false,
      abort: null,
      waitingApproval: awaitingBuild,
      todoBoard: settleTodoBoardOnTurnEnd(r.todoBoard, {
        status,
        mode,
        awaiting: awaitingBuild || awaitingClarify,
      }),
      status: doneRunStatus,
      statusLabel: doneLabel,
      needsAttention: sessionId !== get().sessionId,
    }));
    // Ensure chrome status clears off Working… after success.
    if (sessionId === get().sessionId) {
      const autoTitle =
        typeof done.title === "string" ? done.title.trim() : "";
      const settledBoard = settleTodoBoardOnTurnEnd(
        get().runs[sessionId]?.todoBoard,
        {
          status,
          mode,
          awaiting: awaitingBuild || awaitingClarify,
        },
      );
      set({
        ...(autoTitle ? { sessionTitle: autoTitle } : {}),
        runStatus: doneRunStatus,
        statusLabel: doneLabel,
        sending: false,
        todoBoard: settledBoard,
      });
    }
    if (sessionId !== get().sessionId) {
      get().showToast(
        stoppedLabel
          ? `Task stopped early · ${sessionId.slice(0, 8)}`
          : `Task finished · ${sessionId.slice(0, 8)}`,
      );
    }
    if (
      (get().prefs.voiceReply || get().prefs.voiceConversation) &&
      sessionId === get().sessionId &&
      status !== "cancelled" &&
      status !== "error" &&
      !awaitingBuild &&
      !awaitingClarify &&
      finalText.trim()
    ) {
      const conversation = get().prefs.voiceConversation;
      void speakText(sessionId, finalText)
        .then(() => {
          // Hands-free loop: after the spoken reply finishes, re-open the mic
          // for the next turn (Codex/Perplexity-style continuous conversation).
          if (conversation && get().prefs.voiceConversation && sessionId === get().sessionId) {
            const relisten = get().voiceRelisten;
            if (typeof relisten === "function") relisten();
          }
        })
        .catch((err) => {
          if (err instanceof DOMException && err.name === "AbortError") return;
          get().showToast(
            `Speak: ${err instanceof Error ? err.message : String(err)}`,
          );
        });
    }
    await get().refreshSessions();
    if (sessionId === get().sessionId) {
      await get().refreshArtifacts();
      const arts = Array.isArray(done.artifacts) ? done.artifacts : [];
      const preview = arts.some((raw) => {
        const path = String(raw || "").replace(/\\/g, "/");
        return /^(artifacts|outputs|diagrams)\//i.test(path)
          && /\.(html?|md|pdf|png|jpe?g|webp|svg)$/i.test(path);
      });
      if (get().machineActive && preview) {
        get().revealWorkspaceSurface("artifacts");
      }
      if (doneRunStatus === "success" && get().runs[sessionId]?.clientRequestId === clientRequestId) {
        await flushQueue(sessionId);
      }
    }
  } catch (err) {
    if (get().runs[sessionId]?.abort !== abort) return;
    const rejection = err instanceof ApiError
      ? err.data as { admission?: string; request_id?: string } | undefined
      : undefined;
    if (rejection?.admission === "rejected" && rejection.request_id === clientRequestId) {
      acknowledgeDelivery(clientRequestId);
      if (get().sessionId === sessionId && !get().draft && !opts.resume && !opts.resumeTurnId) set({ draft: text });
    }
    if ((opts.resume || opts.resumeTurnId) && err instanceof ApiError && [400, 409].includes(err.status)
        && !get().runs[sessionId]?.transport?.turnId) {
      // Rejected before acceptance: this is not an agent run or a chat reply.
      updateRun(sessionId, (run) => ({ ...run,
        messages: run.messages.filter((m) => m.id !== userMsg.id && m.id !== assistantId),
        sending: false, abort: null,
        status: previousRun?.status || "interrupted",
        statusLabel: previousRun?.statusLabel || "Task paused",
      }));
      if (get().sessionId === sessionId) {
        set({ error: null });
        get().showToast(err.message);
        await get().openSession(sessionId);
      }
      return;
    }
    const aborted =
      abort.signal.aborted ||
      (err instanceof Error && err.name === "AbortError");
    if (aborted) {
      set((s) => {
        const run = s.runs[sessionId];
        if (!run) return s;
        const messages = run.messages.map((m) =>
          m.id === assistantId
            ? {
                ...applyTranscriptToMessage(m, { type: "finish", assembled: m.text }),
                streaming: false,
                statusLabel: "Cancelled",
              }
            : m,
        );
        const next = { ...run, messages };
        const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
        return { ...sync, runs: { ...s.runs, [sessionId]: next } };
      });
      updateRun(sessionId, (r) => ({
        ...r,
        sending: false,
        abort: null,
        status: "cancelled",
        statusLabel: "Cancelled",
      }));
      return;
    }
    // Mid-stream network drop — the backend turn is still running. Hand off
    // to the reattach poller, reusing the existing assistant bubble so the
    // user sees "Reconnecting…" then live progress, not a dead Error.
    if (err instanceof StreamDroppedError) {
      updateRun(sessionId, (r) =>
        touchTransport(
          { ...r, abort: null, sending: true, status: "running", statusLabel: "Reconnecting…" },
          {
            mode: "polling",
            lastSequence: err.lastSequence || r.transport?.lastSequence || 0,
            turnId: err.turnId !== "pending" ? err.turnId : r.transport?.turnId,
            reconnectCount: (r.transport?.reconnectCount || 0) + 1,
          },
        ),
      );
      patchAssistant(sessionId, assistantId, {
        streaming: true,
        statusLabel: "Reconnecting…",
        turnId: err.turnId !== "pending" ? err.turnId : undefined,
      });
      if (sessionId === get().sessionId) {
        set({ error: null, runStatus: "running", statusLabel: "Reconnecting…", abort: null });
      }
      // Missing stream headers do not prove acceptance. Resolve read-only;
      // never replace the optimistic reply with old history or replay its POST.
      let resolvedTurnId = err.turnId;
      let receivedWithoutTurn = false;
      let finishedWithoutTurn = false;
      let rejectedWithoutTurn: string | undefined;
      if (err.requestId && resolvedTurnId === "pending") {
        const receipt = await recoverReceipt(sessionId, err.requestId, {
          stopped: () => get().runs[sessionId]?.status === "cancelled"
            || get().runs[sessionId]?.clientRequestId !== clientRequestId,
          onReceived: () => {
            if (get().runs[sessionId]?.status === "cancelled" || get().runs[sessionId]?.clientRequestId !== clientRequestId) return;
            markDelivery(clientRequestId, "received");
            patchAssistant(sessionId, assistantId, { statusLabel: "Request received · preparing…" });
            updateRun(sessionId, r => ({ ...r, statusLabel: "Request received · preparing…" }));
          },
        });
        resolvedTurnId = receipt?.receipt.turn_id || "";
        receivedWithoutTurn = receipt?.receipt.state === "received";
        if (receipt?.receipt.state === "rejected") {
          rejectedWithoutTurn = receipt.receipt.message || "The server rejected this request before execution.";
        }
        if (resolvedTurnId) acknowledgeDelivery(clientRequestId);
        else if (receipt?.receipt.state === "finished") {
          // Ending admission is not proof that history accepted the reply.
          finishedWithoutTurn = true;
        }
      } else if (resolvedTurnId === "pending") {
        try {
          const sessionData = await api<{ active_turn?: { turn_id?: string } }>(
            `/api/sessions/${encodeURIComponent(err.sessionId || sessionId)}`,
            { signal: AbortSignal.timeout(5000) },
          );
          resolvedTurnId = sessionData.active_turn?.turn_id || "";
          // An old turn is not an acknowledgement of this new message.
          const previousTurnId = previousRun?.transport?.turnId
            || [...(previousRun?.messages || [])].reverse().find(m => m.turnId)?.turnId;
          if (resolvedTurnId === previousTurnId) resolvedTurnId = "";
        } catch {
          resolvedTurnId = "";
        }
      }
      // Stop/new turn may have superseded this request while the check awaited.
      if (!get().runs[sessionId]?.messages.some(m => m.id === assistantId && m.streaming)
        || get().runs[sessionId]?.status === "cancelled"
        || get().runs[sessionId]?.clientRequestId !== clientRequestId) return;
      if (rejectedWithoutTurn) {
        acknowledgeDelivery(clientRequestId);
        patchAssistant(sessionId, assistantId, {
          streaming: false, resultStatus: "error", statusLabel: "Request rejected", failureCause: rejectedWithoutTurn,
        });
        updateRun(sessionId, r => ({
          ...r, sending: false, abort: null, status: "error", statusLabel: "Request rejected",
        }));
        if (sessionId === get().sessionId) set({ error: rejectedWithoutTurn, runStatus: "error", sending: false, statusLabel: "Request rejected" });
      } else if (resolvedTurnId && resolvedTurnId !== "pending") {
        reattach(err.sessionId || sessionId, err.threadId || threadId, resolvedTurnId, {
          assistantId,
          afterSequence: err.lastSequence,
        });
      } else {
        const label = finishedWithoutTurn ? "Request ended · reopen chat for result" : receivedWithoutTurn ? "Request received · startup delayed" : "Delivery unconfirmed · check before retrying";
        const detail = finishedWithoutTurn
          ? "The server finished handling this request. Reopen this chat to load its saved result; do not resend it automatically."
          : receivedWithoutTurn
          ? "The server received this request but has not confirmed a turn yet. Your reply is preserved. Check its status or Stop before sending another request."
          : "The connection ended before a new run was confirmed. Your reply is preserved. Check this chat before sending it again; the request may have arrived late.";
        patchAssistant(sessionId, assistantId, {
          streaming: false, resultStatus: "delivery_unconfirmed", statusLabel: label, failureCause: detail,
        });
        updateRun(sessionId, r => ({
          ...r, sending: false, abort: null, status: "interrupted", statusLabel: label,
        }));
        if (sessionId === get().sessionId) set({ error: detail, runStatus: "interrupted", sending: false, statusLabel: label });
      }
      return;
    }
    // A proxy/HTML error still means the turn is running. Reattach the same
    // bubble instead of painting Error and later dumping every chip twice.
    const existingTurn = String(get().runs[sessionId]?.transport?.turnId || "").trim();
    if (existingTurn) {
      reattach(sessionId, threadId, existingTurn, {
        assistantId,
        afterSequence: get().runs[sessionId]?.transport?.lastSequence || 0,
      });
      return;
    }
    const message = err instanceof Error ? err.message : String(err);
    set((s) => {
      const run = s.runs[sessionId];
      if (!run) return s;
      const messages = run.messages.map((m) =>
        m.id === assistantId
          ? {
              ...applyTranscriptToMessage(m, { type: "finish", assembled: m.text || message }),
              streaming: false,
              statusLabel: "Error",
              failureCause: message || "The run failed.",
            }
          : m,
      );
      const next = { ...run, messages };
      const sync = s.sessionId === sessionId ? syncFromRun(next) : {};
      return { ...sync, runs: { ...s.runs, [sessionId]: next } };
    });
    updateRun(sessionId, (r) => ({
      ...r,
      sending: false,
      abort: null,
      status: "error",
      statusLabel: "Error",
      needsAttention: sessionId !== get().sessionId,
    }));
    if (sessionId === get().sessionId) {
      set({ error: message });
    }
  }
}
