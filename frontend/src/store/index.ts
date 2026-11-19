import { terminalRunState } from "../lib/runStatus";
import { captureSteeringBoundary } from "../lib/steering";
import { create } from "zustand";
import { credentialIntent, detailText } from "../lib/askHumanApproval";
import { loadComposerDraft, saveComposerDraft } from "./composerDraft";
import { api, ApiError } from "../api/client";
import { readSession, type SessionDetail } from "../api/sessionReads";
import { loadSessionPages } from "../api/sessionPages";
import { downloadSession, uploadSession } from "../api/sessionTransfer";
import {
  filterSlashByCapabilities,
  mergeServerCatalog,
  SLASH_COMMANDS,
} from "../api/slashCatalog";
import type {
  AgentMode,
  AgentPersona,
  AgentPersonaHistoryEntry,
  EffectiveAgentPersona,
  ArtifactEntry,
  ChatMessage,
  MetaPayload,
  PendingApproval,
  Project,
  QueuedMessage,
  SessionRun,
  SessionSummary,
  ToastMessage,
} from "../api/types";
import {
  isShowcaseArtifact,
  showcaseSortKey,
  toCanvasItem,
} from "../lib/artifactMedia";
import type { CanvasItem } from "../lib/artifactMedia";
import { deliveredPaths } from "../lib/deliverables";
import { shouldAgentFocus } from "../lib/agentFocus";
import { refreshSessionTruth } from "../lib/sessionTruth";
import { isNarrowViewport } from "../lib/viewport";
import { attachAskHumanPending, applyTranscriptToMessage, resolveAskHumanBlock } from "../lib/transcript";
import {
  apiSoft,
  applyLiveSessionSnapshot,
  reconcileHistoryMessages,
  approvalStatusLabel,
  cancelInFlightRun,
  emptyRun,
  stampPendingMachine,
  stampPendingRemote,
  isPlanApproval,
  isQuestionApproval,
  isCredentialApproval,
  isContinueApproval,
  recoveryAfterSequence,
  shouldSkipLiveRecovery,
  suppressCredentialApproval,
  beginApprovalResolution,
  finishApprovalResolution,
  isComputerAdminSlash,
  isMachineAdminSlash,
  isMetaOnlySlash,
  isModeOnlyComposerText,
  loadCachedSessions,
  loadLastSession,
  saveCachedSessions,
  isInternalRecoveryMessage,
  mapHistoryMessages,
  parseModeSlash,
  parseTodoBoardPayload,
  rememberSession,
  syncFromRun,
  uid,
} from "./helpers";
import type { ReattachStop } from "./reattach";
import {
  applyPrefsToDocument,
  loadPrefs,
  mergePrefs,
  savePrefs,
} from "./prefs";
import { validateAttachments } from "../lib/attachments";
import { uploadSessionFiles } from "../lib/uploadAttachments";
import { parseModelCatalogPayload } from "../lib/modelCatalog";
import { parseSearchFromProviderSetup } from "../lib/searchSetup";
import { isBrowserToolName } from "../lib/activityUi";
import { reattachToActiveTurn } from "./reattach";
import { serverTurnFromSession, staleLocalRun, type ServerTurn } from "../lib/turnTruth";
import { hasLiveRequestStream, localTurn, settleRunFromServer } from "./serverTruth";
import { hydrateMessageTranscripts, restoreMissingAssistantRows } from "./replayTranscript";
import { runTurn as runTurnStream, type TurnOptions } from "./runTurn";
import {
  applySessionFlagsLocally,
  deleteSessionApi,
  patchSession,
  sessionIdToRestoreOnBoot,
  sortSessionsPinnedFirst,
} from "./sessions";
import type { AppState, AskHumanUndoState } from "./types";
import { rememberQueue, restoredQueue, pendingDeliveries, acknowledgeDelivery, markDelivery, clearDeliverySession } from "./delivery";
import type { RequestReceipt } from "../api/receipts";
import { parseUrlState, updateUrl } from "../lib/urlSync";

/** Drop stale GET /api/provider-setup responses when loads overlap. */
let searchLoadSeq = 0;
let sessionsRefreshController: AbortController | null = null;

function patchAskHumanResolution(
  state: AppState,
  sessionId: string,
  approvalId: string,
  answer: string | undefined,
  deferred: boolean,
): Partial<AppState> {
  if (!sessionId) return {};
  const run = state.runs[sessionId];
  if (!run) return {};
  const messages = run.messages.map((message) =>
    message.blocks?.some(
      (block) => block.type === "ask_human" && block.approvalId === approvalId,
    )
      ? resolveAskHumanBlock(message, approvalId, answer, deferred)
      : message,
  );
  const nextRun: SessionRun = {
    ...run,
    messages,
    waitingApproval: false,
    pendingApproval: null,
  };
  const runs = { ...state.runs, [sessionId]: nextRun };
  if (sessionId !== state.sessionId) {
    return { runs };
  }
  return {
    runs,
    ...syncFromRun(nextRun),
    pendingApproval: null,
  };
}

export const useAppStore = create<AppState>((set, get) => {
  const initialPrefs = loadPrefs();
  // Refreshing a phone should return to chat, regardless of the last opened pane.
  if (isNarrowViewport()) initialPrefs.rightPanelCollapsed = true;
  applyPrefsToDocument(initialPrefs);
  const remoteActivations = new Map<string, Promise<void>>();
  const machineActivations = new Map<string, Promise<unknown>>();
  let openSessionSeq = 0;

  const activateRemote = async (sessionId: string, hostId: string): Promise<void> => {
    await api("/api/remote/activate", {
      method: "POST",
      body: JSON.stringify({ session_id: sessionId, active: true, host_id: hostId || undefined }),
    });
  };

  const probeRemoteHealth = async (sessionId: string, hostId: string, signal?: AbortSignal): Promise<void> => {
    let health = "blocked";
    let detail = "Remote host is unreachable";
    try {
      const result = await api<{ ok?: boolean; report?: { error?: string } }>("/api/remote/test", {
        method: "POST",
        body: JSON.stringify({ host_id: hostId || undefined }),
        signal,
      });
      health = result.ok ? "connected" : "blocked";
      detail = result.ok ? "Remote host reachable" : result.report?.error || detail;
    } catch (err) {
      detail = err instanceof Error ? err.message : String(err);
    }
    if (!signal?.aborted && get().sessionId === sessionId && get().remoteActive && get().remoteHostId === hostId) {
      set({ remoteHealth: health, remoteDetail: detail });
    }
  };

  const ensureRun = (sessionId: string): SessionRun => {
    const existing = get().runs[sessionId];
    if (existing) return existing;
    const run = emptyRun(sessionId);
    set((s) => ({ runs: { ...s.runs, [sessionId]: run } }));
    return run;
  };

  const updateRun = (
    sessionId: string,
    updater: (run: SessionRun) => SessionRun,
  ) => {
    set((s) => {
      const prev = s.runs[sessionId] || emptyRun(sessionId, s.threadId);
      const next = updater(prev);
      if (next === prev) return s;
      if (next.queue !== prev.queue) rememberQueue(sessionId, next.queue);
      const runs =
        s.runs[sessionId] === next
          ? s.runs
          : { ...s.runs, [sessionId]: next };
      if (sessionId === s.sessionId) {
        return {
          runs,
          ...syncFromRun(next),
        };
      }
      return { runs };
    });
  };

  /** Coalesce stream text patches to one Zustand update per animation frame. */
  const pendingTextPatches = new Map<
    string,
    { sessionId: string; assistantId: string; text: string }
  >();
  const pendingNarrativePatches = new Map<
    string,
    { sessionId: string; assistantId: string; assembled: string }
  >();
  let textPatchRaf = 0;
  let narrativePatchRaf = 0;

  /** Active reattach poll cancellers, keyed by sessionId. */
  const reattachCancellers = new Map<string, ReattachStop>();
  /** Peer-monitor pollers must not cancel (or be cancelled by) the main turn. */
  const peerMonitorCancellers = new Map<string, ReattachStop>();
  const reattachTurns = new Map<string, string>();
  let sessionLoadController: AbortController | null = null;
  let historyReplayController: AbortController | null = null;
  let prefetchController: AbortController | null = null;
  let prefetchId = "";
  const sessionDetailCache = new Map<string, { data: Record<string, unknown>; timestamp: number }>();
  const MAX_SESSION_CACHE = 25;
  const cacheSessionDetail = (sessionId: string, data: Record<string, unknown>) => {
    if (sessionDetailCache.size >= MAX_SESSION_CACHE) {
      const oldest = sessionDetailCache.keys().next().value;
      if (oldest) sessionDetailCache.delete(oldest);
    }
    sessionDetailCache.set(sessionId, { data, timestamp: Date.now() });
  };

  const flushTextPatches = () => {
    textPatchRaf = 0;
    if (!pendingTextPatches.size) return;
    const batch = [...pendingTextPatches.values()];
    pendingTextPatches.clear();
    set((s) => {
      let runs = s.runs;
      let syncRun: SessionRun | undefined;
      let any = false;
      const bySession = new Map<string, Map<string, string>>();
      for (const p of batch) {
        let m = bySession.get(p.sessionId);
        if (!m) {
          m = new Map();
          bySession.set(p.sessionId, m);
        }
        m.set(p.assistantId, p.text);
      }
      for (const [sessionId, idToText] of bySession) {
        const prev = runs[sessionId];
        if (!prev) continue;
        let changed = false;
        const messages = prev.messages.map((m) => {
          const text = idToText.get(m.id);
          if (text == null || m.text === text) return m;
          changed = true;
          return { ...m, text };
        });
        if (!changed) continue;
        any = true;
        const next = { ...prev, messages };
        runs = { ...runs, [sessionId]: next };
        if (sessionId === s.sessionId) syncRun = next;
      }
      if (!any) return s;
      if (syncRun) return { runs, ...syncFromRun(syncRun) };
      return { runs };
    });
  };

  const flushNarrativePatches = () => {
    narrativePatchRaf = 0;
    if (!pendingNarrativePatches.size) return;
    const batch = [...pendingNarrativePatches.values()];
    pendingNarrativePatches.clear();
    set((s) => {
      let runs = s.runs;
      let syncRun: SessionRun | undefined;
      let any = false;
      const bySession = new Map<string, Map<string, string>>();
      for (const p of batch) {
        let m = bySession.get(p.sessionId);
        if (!m) {
          m = new Map();
          bySession.set(p.sessionId, m);
        }
        m.set(p.assistantId, p.assembled);
      }
      for (const [sessionId, idToText] of bySession) {
        const prev = runs[sessionId];
        if (!prev) continue;
        let changed = false;
        const messages = prev.messages.map((m) => {
          const assembled = idToText.get(m.id);
          if (assembled == null || m.text === assembled) return m;
          changed = true;
          return applyTranscriptToMessage(m, { type: "narrative", assembled });
        });
        if (!changed) continue;
        any = true;
        const next = { ...prev, messages };
        runs = { ...runs, [sessionId]: next };
        if (sessionId === s.sessionId) syncRun = next;
      }
      if (!any) return s;
      if (syncRun) return { runs, ...syncFromRun(syncRun) };
      return { runs };
    });
  };

  const flushStreamPatches = () => {
    if (textPatchRaf) {
      cancelAnimationFrame(textPatchRaf);
      flushTextPatches();
    }
    if (narrativePatchRaf) {
      cancelAnimationFrame(narrativePatchRaf);
      flushNarrativePatches();
    }
  };

  const patchAssistantText = (
    sessionId: string,
    assistantId: string,
    text: string,
  ) => {
    pendingTextPatches.set(`${sessionId}:${assistantId}`, {
      sessionId,
      assistantId,
      text,
    });
    if (!textPatchRaf) {
      textPatchRaf = requestAnimationFrame(flushTextPatches);
    }
  };

  const patchAssistantNarrative = (
    sessionId: string,
    assistantId: string,
    assembled: string,
  ) => {
    pendingNarrativePatches.set(`${sessionId}:${assistantId}`, {
      sessionId,
      assistantId,
      assembled,
    });
    if (!narrativePatchRaf) {
      narrativePatchRaf = requestAnimationFrame(flushNarrativePatches);
    }
  };

  const patchAssistant = (
    sessionId: string,
    assistantId: string,
    patch: Partial<ChatMessage>,
  ) => {
    const keys = Object.keys(patch);
    if (keys.length === 1 && keys[0] === "text" && typeof patch.text === "string") {
      patchAssistantText(sessionId, assistantId, patch.text);
      return;
    }
    if (textPatchRaf || narrativePatchRaf) {
      flushStreamPatches();
    }
    updateRun(sessionId, (run) => {
      let changed = false;
      const messages = run.messages.map((m) => {
        if (m.id !== assistantId) return m;
        changed = true;
        return { ...m, ...patch };
      });
      if (!changed) return run;
      return { ...run, messages };
    });
  };

  const touchTab = (sessionId: string) => {
    if (!sessionId) return;
    set((s) =>
      s.tabs.includes(sessionId) ? s : { tabs: [...s.tabs, sessionId] },
    );
  };

  const parkActive = () => {
    const { sessionId, pendingFiles, threadId, pendingApproval, machineActive, modelOverride } =
      get();
    if (!sessionId) return;
    updateRun(sessionId, (run) => ({
      ...run,
      pendingFiles: pendingFiles.slice(),
      threadId: threadId || run.threadId,
      machineActive,
      // Persist the composer's model pin with the tab so switching away and
      // back (or another tab changing its own pin) never bleeds across runs.
      modelOverride,
      pendingApproval:
        pendingApproval?.sessionId === sessionId ||
        pendingApproval?.session_id === sessionId
          ? pendingApproval
          : run.pendingApproval,
    }));
    // Clear global pending — restored when switching back.
    set({ pendingFiles: [] });
  };

  const uploadControllers = new Map<string, AbortController>();
  const steeringRequests = new Set<string>();
  const resumeRequests = new Set<string>();
  const immediateRequests = new Set<string>();

  const flushQueue = async (sessionId: string) => {
    const run = get().runs[sessionId];
    if (!run || run.sending || immediateRequests.has(sessionId) || sessionId !== get().sessionId) return;
    const next = run.queue[0];
    if (!next) return;
    if (next.needsFiles) {
      get().showToast("This restored message needs its files reattached before sending.");
      return;
    }
    if (next.needsReview) {
      get().showToast("Recovered queued message is ready for review. Choose Send now when ready.");
      return;
    }
    // Reuse the queue-send path: it preserves the current composer draft and
    // files, and removes this item only after uploads succeed.
    await get().sendNowMessage(next.text || "", next);
  };

  const runTurn = async (
    sessionId: string,
    threadId: string,
    text: string,
    attachments: string[],
    displayText: string,
    opts: TurnOptions = {},
  ) =>
    runTurnStream(
      {
        set,
        get,
        updateRun,
        patchAssistant,
        patchAssistantNarrative,
        flushStreamPatches,
        flushQueue,
        reattach,
      },
      sessionId,
      threadId,
      text,
      attachments,
      displayText,
      opts,
    );

  const reattach = (
    sessionId: string,
    threadId: string,
    turnId: string,
    opts: {
      pendingApproval?: PendingApproval | null;
      assistantId?: string;
      backgroundPeerMonitor?: boolean;
      afterSequence?: number;
      initialSnapshot?: NonNullable<Parameters<typeof reattachToActiveTurn>[4]>["initialSnapshot"];
    } = {},
  ) => {
    const bucket = opts.backgroundPeerMonitor
      ? peerMonitorCancellers
      : reattachCancellers;
    // Cancel any prior poll loop for this consumer only — a peer monitor
    // must not tear down the main-turn poller, and vice versa.
    bucket.get(sessionId)?.();
    const cancel = reattachToActiveTurn(
      { set, get, updateRun, onSettled: (id, status) => {
        if (status === "success") void flushQueue(id);
      }, onServerTurn: recordServerTurn },
      sessionId,
      threadId,
      turnId,
      opts,
    );
    bucket.set(sessionId, cancel);
    if (!opts.backgroundPeerMonitor) reattachTurns.set(sessionId, turnId);
  };

  /** Keeps the newest server report of a chat's turn status. */
  const recordServerTurn = (sessionId: string, turn: ServerTurn | null) => {
    if (!sessionId || !turn) return;
    const previous = get().serverTurns?.[sessionId];
    if (previous && previous.observedAt > turn.observedAt) return;
    if (previous && previous.observedAt === turn.observedAt && previous.running === turn.running
      && previous.status === turn.status && previous.turnId === turn.turnId) return;
    set((s) => ({ serverTurns: { ...(s.serverTurns || {}), [sessionId]: turn } }));
  };
  /** Ends a local run the server reports has no running turn (Requirement 18.4). */
  const settleStaleRun = (sessionId: string, report: ServerTurn, snapshot: Parameters<typeof settleRunFromServer>[2]) => {
    const run = get().runs[sessionId];
    if (!run || !staleLocalRun(report, localTurn(run)) || hasLiveRequestStream(run)) return false;
    // A stream consumer of the ended turn is obsolete; replace it with the report.
    reattachCancellers.get(sessionId)?.();
    reattachCancellers.delete(sessionId);
    reattachTurns.delete(sessionId);
    const settled = settleRunFromServer(run, report, snapshot);
    updateRun(sessionId, () => settled);
    if (settled.status === "success") void flushQueue(sessionId);
    return true;
  };
  /** Records reports in one update; a report that a local run's turn ended settles it. */
  const noteServerTurns = (reports: Array<[string, ServerTurn | null]>) => {
    const current = get().serverTurns || {};
    const fresh = reports.filter((entry): entry is [string, ServerTurn] =>
      Boolean(entry[0] && entry[1] && !(current[entry[0]]?.observedAt > entry[1].observedAt)));
    if (!fresh.length) return;
    set((s) => ({ serverTurns: { ...(s.serverTurns || {}), ...Object.fromEntries(fresh) } }));
    for (const [sessionId, turn] of fresh) {
      const run = get().runs[sessionId];
      // The report has no transcript; the session detail settles the run with its answer.
      if (staleLocalRun(turn, localTurn(run)) && !hasLiveRequestStream(run)) void recoverLiveProgress(sessionId);
    }
  };
  const noteServerTurn = (sessionId: string, turn: ServerTurn | null) => noteServerTurns([[sessionId, turn]]);

  const liveRecoveryInFlight = new Set<string>();
  const recoverLiveProgress = async (targetSessionId?: string, opts: { lightweight?: boolean; snapshot?: SessionDetail; signal?: AbortSignal; approvalAtStart?: string } = {}) => {
    const sessionId = targetSessionId || get().sessionId;
    if (!sessionId) return;
    const run = get().runs[sessionId];
    const threadId =
      run?.threadId ||
      (sessionId === get().sessionId ? get().threadId : null);
    if (!threadId || (run?.status === "cancelled" && !opts.snapshot)) return;
    // The two-second UI timer must not stack full-session reads behind a
    // slow response and consume every browser HTTP connection.
    if (liveRecoveryInFlight.has(sessionId) && !opts.snapshot) return;
    if (!opts.snapshot) liveRecoveryInFlight.add(sessionId);
    try {
      let expectedTurn = run?.transport?.turnId || "";
      let settledReceipt = false;
      if (run?.clientRequestId && !expectedTurn) {
        // Never attach a delayed admission to the previous/latest turn.
        if (run.abort && !run.abort.signal.aborted) return;
        const receipt = await api<RequestReceipt>(
          `/api/sessions/${encodeURIComponent(sessionId)}/requests/${encodeURIComponent(run.clientRequestId)}`,
        );
        if (get().runs[sessionId]?.clientRequestId !== run.clientRequestId || get().runs[sessionId]?.status === "cancelled") return;
        expectedTurn = receipt?.receipt?.turn_id || "";
        if (!expectedTurn) return;
        settledReceipt = receipt.receipt.state === "finished" || receipt.receipt.state === "interrupted";
      }
      const observedAt = Date.now();
      let data = opts.snapshot ?? await readSession<SessionDetail>(sessionId, opts.signal, opts.lightweight);
      if (opts.signal?.aborted) return;
      if (opts.lightweight && !data.messages?.length) {
        const last = data.turns?.at(-1);
        const lastId = last?.turn_id || last?.id;
        const saved = run?.messages.findLast(m => m.role === "assistant" && m.turnId === lastId);
        const knownTurn = run?.messages.some(m => m.turnId === lastId);
        const changed = lastId && (!knownTurn || (!data.active_turn && (!saved || saved.resultStatus !== last?.status)));
        if (changed || (run?.sending && !data.active_turn)) {
          data = await readSession<SessionDetail>(sessionId, opts.signal);
          if (opts.signal?.aborted) return;
        } else {
          // An empty lightweight projection carries no transcript update.
          data = { ...data, messages: undefined };
        }
      }
      const report = serverTurnFromSession(data, observedAt);
      recordServerTurn(sessionId, report);
      const currentRun = get().runs[sessionId];
      if (currentRun?.clientRequestId !== run?.clientRequestId
        || (currentRun?.status === "cancelled" && !opts.snapshot)
        || currentRun?.transport?.turnId !== run?.transport?.turnId) return;
      const approvalAtStart = opts.snapshot ? opts.approvalAtStart : run?.pendingApproval?.approval_id;
      if (data.pending_approval === null && currentRun?.pendingApproval?.approval_id &&
          currentRun.pendingApproval.approval_id !== approvalAtStart) {
        // A new live approval arrived after this read began; an older null
        // snapshot cannot dismiss it.
        data = { ...data, pending_approval: undefined };
      }
      updateRun(sessionId, current => reconcileHistoryMessages(current, data.messages));
      if (sessionId === get().sessionId) set({
        ...(data.title !== undefined ? { sessionTitle: data.title } : {}),
        ...(data.project !== undefined ? { sessionProject: data.project } : {}),
      });
      // No running turn on the server: a local flag left over from a dropped
      // stream must not keep the chat running (Requirement 18.4).
      if (report && !report.running && settleStaleRun(sessionId, report, data)) return;
      if (run?.clientRequestId && (run.sending || run.status === "interrupted") && expectedTurn && data.active_turn?.turn_id !== expectedTurn) {
        if (!settledReceipt || run.status !== "interrupted" || shouldSkipLiveRecovery(currentRun)) return;
        const assistantId = run.messages.findLast(m => m.resultStatus === "delivery_unconfirmed")?.id;
        if (!assistantId || reattachTurns.get(sessionId) === expectedTurn) return;
        acknowledgeDelivery(run.clientRequestId);
        reattach(sessionId, threadId, expectedTurn, { assistantId, afterSequence: 0 });
        return;
      }
      // Refresh reads this snapshot. A buffered SSE stream can sit on
      // "Working…" heartbeats without ever applying plan/approval frames,
      // so merge the snapshot even while the live consumer is still attached.
      updateRun(sessionId, (current) =>
        applyLiveSessionSnapshot(current, data, sessionId),
      );
      const applied = get().runs[sessionId];
      if (
        sessionId === get().sessionId &&
        applied?.pendingApproval?.approval_id &&
        isPlanApproval(applied.pendingApproval) &&
        get().agentMode === "normal"
      ) {
        get().setAgentMode("plan");
      }
      if (shouldSkipLiveRecovery(get().runs[sessionId])) return;
      const turnId = String(data.active_turn?.turn_id || "").trim();
      if (!turnId) {
        if (!hasLiveRequestStream(get().runs[sessionId]) && !get().runs[sessionId]?.sending) {
          const terminal = terminalRunState(data.status || data.turns?.at(-1)?.status || "idle");
          updateRun(sessionId, current => ({ ...current, status: terminal.status, statusLabel: terminal.label }));
        }
        return;
      }
      if (
        reattachTurns.get(sessionId) === turnId &&
        reattachCancellers.has(sessionId)
      ) {
        return;
      }
      if (run && !run.sending && run.transport?.turnId !== turnId) {
        updateRun(sessionId, current => ({ ...current, clientRequestId: undefined, transport: undefined, startedAt: undefined }));
      }
      let assistantId = [...(get().runs[sessionId]?.messages || [])]
        .reverse()
        .find((m) => m.role === "assistant" && (m.streaming || m.resultStatus === "delivery_unconfirmed"))?.id;
      if (!assistantId && run) {
        assistantId = uid("a");
        updateRun(sessionId, (current) => ({
          ...current,
          sending: true,
          status: "running",
          statusLabel: "Reconnecting…",
          messages: [
            ...current.messages,
            {
              id: assistantId!,
              role: "assistant",
              text: String(data.live_message || ""),
              streaming: true,
              speaker: data.active_turn?.speaker,
              statusLabel: "Reconnecting…",
              steps: [],
              toolCards: [],
              computerFrames: [],
              blocks: [],
              turnId,
            },
          ],
        }));
      }
      reattach(sessionId, threadId, turnId, {
        assistantId,
        pendingApproval: data.pending_approval,
        afterSequence: recoveryAfterSequence(run?.transport, turnId),
      });
    } catch {
      /* best-effort — next visibility/online tick retries */
    } finally {
      if (!opts.snapshot) liveRecoveryInFlight.delete(sessionId);
    }
  };

  const revalidateSelectedSession = async (sessionId: string, seq: number) => {
    const controller = new AbortController();
    sessionLoadController = controller;
    set({ sessionRefreshing: sessionId, sessionRefreshError: null });
    const approvalAtStart = get().runs[sessionId]?.pendingApproval?.approval_id;
    try {
      const data = await readSession<SessionDetail>(sessionId, controller.signal);
      if (seq !== openSessionSeq || get().sessionId !== sessionId || controller.signal.aborted) return;
      cacheSessionDetail(sessionId, data as unknown as Record<string, unknown>);
      await recoverLiveProgress(sessionId, { snapshot: data, signal: controller.signal, approvalAtStart });
    } catch (error) {
      if (seq !== openSessionSeq || controller.signal.aborted) return;
      set({ sessionRefreshError: { sessionId, message: error instanceof Error ? error.message : String(error) } });
    } finally {
      if (seq === openSessionSeq) set({ sessionRefreshing: null });
      if (sessionLoadController === controller) sessionLoadController = null;
    }
  };

  return {
    sessions: loadCachedSessions(),
    projects: [],
    selectedProjectId: null,
    sessionId: null,
    threadId: null,
    sessionTitle: null,
    sessionProject: null,
    messages: [],
    runStatus: "idle",
    statusLabel: "Ready",
    agentMode: initialPrefs.defaultAgentMode,
    machineActive: false,
    machineBlocked: false,
    machineDetail: "",
    machinePaused: false,
    machineShellRunning: false,
    machineShellLine: "",
    machineNovncUrl: "",
    machineScreenConnected: false,
    machineAdopted: false,
    machineBootPhase: "",
    machineBrowserReady: false,
    remoteActive: false,
    remoteHostId: "",
    remoteHostName: "",
    remoteHealth: "",
    remoteDetail: "",
    remoteRiskTier: "",
    taskControl: "",
    taskControlReason: "",
    taskFailureKind: "",
    taskFailureChange: "",
    taskFailureCount: 0,
    taskStageId: "",
    taskStageStatus: "",
    taskChronicTool: "",
    taskChronicCount: 0,
    taskStatusPanelOpen: false,
    desktopLeaseOwner: "",
    autoApprove: !initialPrefs.defaultAskMode,
    permissionScope: initialPrefs.defaultAskMode ? "ask" : "session",
    sending: false,
    draft: loadComposerDraft(),
    error: null,
    abort: null,
    modelOverride: "",
    activePersonaId: "main",
    personas: [],
    personasError: null,
    personaEditor: null,
    teams: [],
    teamsError: null,
    activeTeamId: null,
    settingsTab: null,
    searchKeyFocus: null,
    pendingFiles: [],
    preparingSends: [],
    attachmentUpload: null,
    pendingApproval: null,
    askHumanUndo: null,
    composerChip: { kind: null, value: null },
    tabs: [],
    runs: {},
    serverTurns: {},
    prefs: initialPrefs,
    canvasOpen: false,
    canvasPreviewRequest: null,
    canvasExpanded: false,
    canvasItems: [],
    canvasSelectedPath: null,
    canvasTurnPaths: new Set(),
    todoBoard: null,
    computerPanelMode: "default",
    browserPanelMode: "default",
    workspaceSurface: "monitor",
    liveSurface: "computer",
    surfacePinnedAt: null,
    surfaceControlling: false,
    liveRevealedThisTurn: false,
    liveSplit: false,
    remoteExecution: null,
    slashCatalog: SLASH_COMMANDS.slice(),
    capabilities: {
      projectFiles: null,
      cometApi: null,
      browserApi: null,
      computerApi: null,
      machineApi: null,
      modelApi: null,
      slashCatalogApi: false,
    },
    meta: null,
    toasts: [],
    peerAlertNavRequestId: 0,
    peerPendingCount: 0,
    models: [],
    modelCredentialsLocked: false,
    modelCatalog: [],
    defaultModel: "",
    searchSetup: null,
    bootError: null,
    connectionOnline:
      typeof navigator === "undefined" ? true : navigator.onLine !== false,
    sessionLoading: false,
    sessionOpenFailure: null,
    sessionRefreshing: null,
    sessionRefreshError: null,
    sessionsError: null,
    resumingSessionId: null,

    setDraft: (value) => set({ draft: value }),

    setConnectionOnline: (online) => {
      set({ connectionOnline: online });
      if (online) void recoverLiveProgress();
    },

    clearError: () => set({ error: null }),

    retryBoot: async () => {
      await get().boot();
    },

    resumeLastTask: async (recovery) => {
      const current = get();
      if (current.sending || !current.sessionId) return;
      const sessionId = current.sessionId;
      if (resumeRequests.has(sessionId)) return;
      resumeRequests.add(sessionId);
      set({ resumingSessionId: sessionId });
      try {
        // The newest durable turn may have no assistant reply (e.g. a restart).
        // Fetch it before adding optimistic messages or starting another turn.
        const saved = await api<{ turns?: Array<{ id?: string; turn_id?: string }>; active_turn?: { turn_id?: string } | null }>(
          `/api/sessions/${encodeURIComponent(sessionId)}`);
        if (get().sessionId !== sessionId || get().sending) return;
        if (saved.active_turn?.turn_id) {
          await get().openSession(sessionId);
          if (get().sessionId === sessionId) set({ draft: current.draft, pendingFiles: current.pendingFiles });
          return;
        }
        const latest = saved.turns?.at(-1);
        const target = latest?.id || latest?.turn_id;
        if (!target) throw new Error("This task has no saved continuation reference. Retry the original request.");
        if (recovery?.turnId && recovery.turnId !== target) throw new Error("This chat has a newer task. Refresh before recovering this result.");
        const message = recovery?.mode === "recheck" ? "Check this result again" : recovery ? "Continue unfinished work" : "Resume task";
        await runTurn(sessionId, current.threadId || `web-${sessionId}`,
          message, [], message, { agentMode: current.agentMode, resume: true, resumeTurnId: target,
            recoveryMode: recovery?.mode, recoveryOutcomeIds: recovery?.outcomeIds });
      } finally {
        resumeRequests.delete(sessionId);
        if (get().resumingSessionId === sessionId) set({ resumingSessionId: null });
      }
    },

    retryLastTurn: async () => {
      const messages = get().messages;
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "user" && messages[i].text?.trim()) {
          await get().sendMessage(messages[i].text);
          return;
        }
      }
    },

    retryLastTurnWithGuidance: async (guidance) => {
      const messages = get().messages;
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "user" && messages[i].text?.trim()) {
          const instruction = guidance.trim();
          await get().sendMessage(
            instruction
              ? `${messages[i].text}\n\n[Repair instruction]\n${instruction}`
              : messages[i].text,
          );
          return;
        }
      }
    },

    setAgentMode: (mode) => {
      set(() => {
        const next: Partial<AppState> = { agentMode: mode };
        if (mode === "normal") {
          next.composerChip = { kind: null, value: null };
        } else {
          next.composerChip = { kind: "mode", value: mode };
        }
        return next;
      });
    },

    setMachineActive: async (active, options) => {
      const before = get();
      if (active !== before.machineActive && (before.sending || before.runs[before.sessionId || ""]?.status === "running")) {
        before.showToast("Stop the current run before changing where it runs.");
        return;
      }
      if (active && before.remoteActive) {
        await get().setRemoteActive(false);
        if (get().sessionId !== before.sessionId || get().remoteActive) return;
      }
      const previous = {
        active: before.machineActive,
        blocked: before.machineBlocked,
        detail: before.machineDetail,
        paused: before.machinePaused,
        shellRunning: before.machineShellRunning,
        shellLine: before.machineShellLine,
        liveSplit: before.liveSplit,
        liveSurface: before.liveSurface,
        workspaceSurface: before.workspaceSurface,
      };
      set({
        machineActive: active,
        machineBlocked: active ? get().machineBlocked : false,
        machineDetail: active
          ? get().machineDetail
          : "",
        machinePaused: active ? get().machinePaused : false,
        machineShellRunning: active ? get().machineShellRunning : false,
        machineShellLine: active ? get().machineShellLine : "",
        machineNovncUrl: active ? get().machineNovncUrl : "",
        machineScreenConnected: active ? get().machineScreenConnected : false,
        machineAdopted: active ? get().machineAdopted : false,
        machineBootPhase: active ? get().machineBootPhase : "",
        machineBrowserReady: active ? get().machineBrowserReady : false,
        desktopLeaseOwner: active ? get().desktopLeaseOwner : "",
        liveSplit: active,
        liveSurface: active ? "computer" : get().liveSurface,
        workspaceSurface: active ? "live" : get().workspaceSurface,
        // Phone/tablet: keep chat as the primary surface; Computer stays a tap away.
        prefs: active && !isNarrowViewport()
          ? { ...get().prefs, rightPanelCollapsed: false }
          : get().prefs,
      });
      const sessionId = get().sessionId;
      if (!sessionId) return;
      updateRun(sessionId, (run) => ({ ...run, machineActive: active }));
      if (options?.synced) {
        refreshSessionTruth(sessionId);
        return;
      }
      set({ executionTargetPending: true });
      const activation = api(`/api/sessions/${encodeURIComponent(sessionId)}/machine`, {
        method: "POST", body: JSON.stringify(options?.upgrade ? { upgrade: true } : { active }),
      });
      machineActivations.set(sessionId, activation);
      try {
        await activation;
      } catch (err) {
        if (get().sessionId === sessionId && get().machineActive === active) {
          set({
            machineActive: previous.active,
            machineBlocked: previous.blocked,
            machineDetail: previous.detail,
            machinePaused: previous.paused,
            machineShellRunning: previous.shellRunning,
            machineShellLine: previous.shellLine,
            liveSplit: previous.liveSplit,
            liveSurface: previous.liveSurface,
            workspaceSurface: previous.workspaceSurface,
          });
          updateRun(sessionId, (run) => ({
            ...run,
            machineActive: previous.active,
          }));
        }
        get().showToast(err instanceof Error ? err.message : String(err));
      } finally {
        if (machineActivations.get(sessionId) === activation) machineActivations.delete(sessionId);
        if (get().sessionId === sessionId) set({ executionTargetPending: machineActivations.has(sessionId) || remoteActivations.has(sessionId) });
        refreshSessionTruth(sessionId);
      }
    },

    setMachinePaused: (paused) => set({ machinePaused: paused }),
    setMachineShellRunning: (running) => set({ machineShellRunning: running }),
    setMachineShellLine: (line) => set({ machineShellLine: line }),
    setMachineNovncUrl: (url) => set({ machineNovncUrl: url }),
    setRemoteActive: async (active, hostId = "", hostName = "") => {
      const before = get();
      if ((active !== before.remoteActive || (active && hostId && hostId !== before.remoteHostId)) &&
          (before.sending || before.runs[before.sessionId || ""]?.status === "running")) {
        before.showToast("Stop the current run before changing where it runs.");
        return;
      }
      const previous = {
        active: before.remoteActive,
        hostId: before.remoteHostId,
        hostName: before.remoteHostName,
        health: before.remoteHealth,
        detail: before.remoteDetail,
        riskTier: before.remoteRiskTier,
      };
      // Mutually exclusive with the isolated machine surface.
      if (active && before.machineActive) {
        await get().setMachineActive(false);
        if (get().sessionId !== before.sessionId || get().machineActive) return;
      }
      set({
        remoteActive: active,
        remoteHostId: active ? hostId || before.remoteHostId : "",
        remoteHostName: active ? hostName || before.remoteHostName : "",
        remoteHealth: active ? "connecting" : "",
        remoteDetail: active ? before.remoteDetail : "",
        remoteRiskTier: active ? before.remoteRiskTier : "",
      });
      const sessionId = get().sessionId;
      if (!sessionId) return; // home screen: pending; stamped when a session mints
      updateRun(sessionId, (run) => ({
        ...run,
        remoteActive: active,
        remoteHostId: active ? hostId || run.remoteHostId : "",
        remoteHostName: active ? hostName || run.remoteHostName : "",
      }));
      const activation = api(`/api/remote/activate`, {
          method: "POST",
          body: JSON.stringify({
            session_id: sessionId,
            active,
            host_id: active ? hostId || undefined : undefined,
          }),
        }).then(() => undefined);
      remoteActivations.set(sessionId, activation);
      set({ executionTargetPending: true });
      try {
        await activation;
        if (active) await probeRemoteHealth(sessionId, hostId || before.remoteHostId);
      } catch (err) {
        if (get().sessionId === sessionId && get().remoteActive === active) {
          set({
            remoteActive: previous.active,
            remoteHostId: previous.hostId,
            remoteHostName: previous.hostName,
            remoteHealth: previous.health,
            remoteDetail: previous.detail,
            remoteRiskTier: previous.riskTier,
          });
          updateRun(sessionId, (run) => ({
            ...run,
            remoteActive: previous.active,
            remoteHostId: previous.hostId,
            remoteHostName: previous.hostName,
          }));
        }
        get().showToast(err instanceof Error ? err.message : String(err));
      } finally {
        if (remoteActivations.get(sessionId) === activation) remoteActivations.delete(sessionId);
        if (get().sessionId === sessionId) set({ executionTargetPending: machineActivations.has(sessionId) || remoteActivations.has(sessionId) });
        refreshSessionTruth(sessionId);
      }
    },
    setTaskStatus: (next) => set(next),
    setTaskChronicHalt: (tool, count) =>
      set({ taskChronicTool: tool, taskChronicCount: count }),
    setTaskStatusPanelOpen: (open) => set({ taskStatusPanelOpen: open }),
    takeDesktopControl: async (opts) => {
      const sessionId = get().sessionId;
      const heartbeat = Boolean(opts?.heartbeat);
      if (!heartbeat) {
        set({ desktopLeaseOwner: "user", surfaceControlling: true });
      }
      if (!sessionId) return;
      try {
        const data = await api<{ owner?: string | null }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/machine/lease`,
          {
            method: "POST",
            body: JSON.stringify(
              heartbeat
                ? { owner: "user", action: "heartbeat" }
                : { owner: "user" },
            ),
          },
        );
        if (heartbeat && data?.owner !== "user") {
          const owner = data?.owner === "agent" ? "agent" : "";
          set({ desktopLeaseOwner: owner, surfaceControlling: false });
        }
      } catch (err) {
        if (!heartbeat) {
          set({ desktopLeaseOwner: "", surfaceControlling: false });
          get().showToast(err instanceof Error ? err.message : String(err));
        }
      }
    },
    releaseDesktopControl: async () => {
      const sessionId = get().sessionId;
      const hadUser = get().desktopLeaseOwner === "user" || (get().machineActive && get().surfaceControlling);
      set({ desktopLeaseOwner: "", surfaceControlling: false });
      if (!sessionId || !hadUser) return;
      try {
        await api(`/api/sessions/${encodeURIComponent(sessionId)}/machine/lease`, {
          method: "POST",
          body: JSON.stringify({ action: "return" }),
        });
      } catch {
        /* overlay already matches agent */
      }
    },

    pauseMachine: async () => {
      const sessionId = get().sessionId;
      if (!sessionId || !get().machineActive) return;
      try {
        const data = await api<{ runtime?: { paused?: boolean; status?: string } }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/machine`,
          { method: "POST", body: JSON.stringify({ paused: true }) },
        );
        set({
          machinePaused: Boolean(
            data.runtime?.paused || data.runtime?.status === "paused",
          ),
        });
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
      }
    },

    resumeMachine: async () => {
      const sessionId = get().sessionId;
      if (!sessionId || !get().machineActive) return;
      try {
        const data = await api<{ runtime?: { paused?: boolean; status?: string } }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/machine`,
          { method: "POST", body: JSON.stringify({ paused: false }) },
        );
        set({
          machinePaused: Boolean(
            data.runtime?.paused || data.runtime?.status === "paused",
          ),
        });
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
      }
    },

    setAskMode: (ask) => {
      const prefs = mergePrefs(get().prefs, { defaultAskMode: ask });
      savePrefs(prefs);
      applyPrefsToDocument(prefs);
      set({
        prefs,
        autoApprove: !ask,
        permissionScope: ask ? "ask" : "session",
      });
    },

    setPermissionsMode: async (mode) => {
      const normalized = mode === "auto" ? "session" : mode;
      try {
        await api("/api/permissions", {
          method: "POST",
          body: JSON.stringify({ mode: normalized === "session" ? "auto" : normalized, session_id:get().sessionId, profile:mode === "auto" ? "auto_review" : mode, confirm_full:mode === "full" }),
        });
      } catch (err) {
        get().showToast(
          `Permissions failed: ${err instanceof Error ? err.message : err}`,
        );
        return;
      }
      const ask = normalized === "ask";
      const prefs = mergePrefs(get().prefs, { defaultAskMode: ask });
      savePrefs(prefs);
      applyPrefsToDocument(prefs);
      set({
        prefs,
        autoApprove: !ask,
        permissionScope: normalized,
      });
      window.dispatchEvent(new CustomEvent("kageha-permissions-changed"));
      const label =
        normalized === "full"
          ? "Full access · this chat"
          : normalized === "session"
            ? "Approve for me · independent review"
            : "Ask · confirm risky tools";
      get().showToast(label);
    },

    setPrefs: (patch) => {
      const prefs = mergePrefs(get().prefs, patch);
      savePrefs(prefs);
      applyPrefsToDocument(prefs);
      const next: Partial<AppState> = { prefs };
      if (typeof patch.defaultAskMode === "boolean") {
        next.autoApprove = !prefs.defaultAskMode;
        next.permissionScope = prefs.defaultAskMode ? "ask" : "session";
      }
      if (patch.defaultAgentMode) {
        next.agentMode = prefs.defaultAgentMode;
      }
      set(next);
    },

    setModelOverride: (model) => {
      set({ modelOverride: model });
      // Persist onto the active run so syncFromRun (fires on submit / session
      // switch) rehydrates the pick instead of resetting it to the fallback.
      const sessionId = get().sessionId;
      if (sessionId && get().runs[sessionId]) {
        updateRun(sessionId, (r) => ({ ...r, modelOverride: model }));
      }
    },

    setActivePersonaId: (id) => set({ activePersonaId: id || "main" }),

    setActiveTeamId: (id) => set({ activeTeamId: id }),

    openPersonaEditor: (spec) => set({ personaEditor: spec }),

    closePersonaEditor: () => set({ personaEditor: null }),

    openSettings: (tab, opts) =>
      set({
        settingsTab: tab,
        searchKeyFocus: opts?.searchKey ?? null,
      }),

    clearSearchKeyFocus: () => set({ searchKeyFocus: null }),

    closeSettings: () => set({ settingsTab: null, searchKeyFocus: null }),

    refreshPersonas: async () => {
      const res = await apiSoft("/api/agents");
      if (!res.ok) {
        const message =
          typeof res.data.error === "string" ? res.data.error : "Couldn't load personas.";
        set({ personasError: message });
        return;
      }
      const list = Array.isArray(res.data.agents)
        ? (res.data.agents as AgentPersona[])
        : [];
      set({ personas: list, personasError: null });
    },

    refreshTeams: async () => {
      const res = await apiSoft("/api/teams");
      if (!res.ok) {
        set({ teamsError: "Couldn't load teams." });
        return;
      }
      const list = Array.isArray(res.data.teams) ? res.data.teams : [];
      set({ teams: list, teamsError: null });
    },

    createPersona: async (payload) => {
      const data = await api<{ agent: AgentPersona }>("/api/agents", {
        method: "POST",
        body: JSON.stringify(payload),
      });
      // Keep the editor open after create: the list must include the new
      // agent before we switch the dialog to edit (upload) mode.
      if (data.agent?.id) {
        set((state) => {
          const exists = state.personas.some((item) => item.id === data.agent.id);
          return {
            personas: exists
              ? state.personas.map((item) => (item.id === data.agent.id ? data.agent : item))
              : [...state.personas, data.agent],
            activePersonaId: data.agent.id,
            personasError: null,
          };
        });
      }
      await get().refreshPersonas();
      if (data.agent?.name) get().showToast(`Created ${data.agent.name}`);
      return data.agent;
    },

    updatePersona: async (id, payload) => {
      const data = await api<{ agent: AgentPersona }>(
        `/api/agents/${encodeURIComponent(id)}`,
        { method: "PUT", body: JSON.stringify(payload) },
      );
      await get().refreshPersonas();
      if (data.agent?.name) get().showToast(`Saved ${data.agent.name}`);
      return data.agent;
    },

    getPersonaEffective: async (id) => {
      const data = await api<EffectiveAgentPersona>(
        `/api/agents/${encodeURIComponent(id)}`,
      );
      return data;
    },

    getPersonaHistory: async (id) => {
      const data = await api<{ history: AgentPersonaHistoryEntry[] }>(
        `/api/agents/${encodeURIComponent(id)}/history`,
      );
      return data.history || [];
    },

    rollbackPersona: async (id, snapshotId) => {
      const data = await api<{ agent: AgentPersona }>(
        `/api/agents/${encodeURIComponent(id)}/rollback`,
        { method: "POST", body: JSON.stringify({ snapshot_id: snapshotId }) },
      );
      await get().refreshPersonas();
      return data.agent;
    },

    deletePersona: async (id) => {
      await api(`/api/agents/${encodeURIComponent(id)}`, { method: "DELETE" });
      await get().refreshPersonas();
      if (get().activePersonaId === id) set({ activePersonaId: "main" });
      get().showToast("Agent deleted");
    },

    setComposerChip: (kind, value) => set({ composerChip: { kind, value } }),

    clearComposerChip: (opts) => {
      set({ composerChip: { kind: null, value: null } });
      if (opts?.resetMode) set({ agentMode: "normal" });
    },

    voiceRelisten: null,

    setVoiceRelisten: (fn) => set({ voiceRelisten: fn }),

    showToast: (text, opts) => {
      const toast: ToastMessage = {
        id: uid("t"),
        text,
        createdAt: Date.now(),
        actionLabel: opts?.actionLabel,
        actionId: opts?.actionId,
      };
      set((s) => ({ toasts: [...s.toasts.slice(-4), toast] }));
      window.setTimeout(() => {
        get().dismissToast(toast.id);
      }, 4200);
    },

    dismissToast: (id) =>
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),

    requestPeerAlertNav: () => set((s) => ({ peerAlertNavRequestId: s.peerAlertNavRequestId + 1 })),
    setPeerPendingCount: (count) => set({ peerPendingCount: count }),

    cancelAskHumanSend: () => {
      const undo = get().askHumanUndo;
      if (!undo) return;
      window.clearTimeout(undo.timerId);
      get().dismissToast(undo.toastId);
      set((state) => {
        const run = state.runs[undo.sessionId];
        if (!run) return { askHumanUndo: null };
        const messages = run.messages.map((message) =>
          message.blocks?.some(
            (block) =>
              block.type === "ask_human" &&
              block.approvalId === undo.pending.approval_id,
          )
            ? attachAskHumanPending({ ...message, approval: undo.pending }, undo.pending)
            : message,
        );
        const nextRun: SessionRun = {
          ...run,
          messages,
          waitingApproval: true,
          status: "waiting_approval",
          statusLabel: approvalStatusLabel(undo.pending),
          pendingApproval: undo.pending,
        };
        const runs = { ...state.runs, [undo.sessionId]: nextRun };
        if (undo.sessionId !== state.sessionId) {
          return { askHumanUndo: null, runs };
        }
        return {
          askHumanUndo: null,
          runs,
          ...syncFromRun(nextRun),
          pendingApproval: undo.pending,
          runStatus: "waiting_approval",
          statusLabel: approvalStatusLabel(undo.pending),
        };
      });
    },

    continueWithGmail: async (sessionId, sourceTurn) => {
      const before = get();
      if (before.sessionId !== sessionId) return;
      const base = "/api/sessions/" + encodeURIComponent(sessionId);
      const access = await api<{ status: string; next_step?: string }>(base + "/gmail-access");
      if (access.status !== "ready") throw new Error(access.next_step || "Gmail access is still needed.");
      const current = get();
      if (current.sessionId !== sessionId) return;
      const instruction = "Gmail access is now available. Continue the original task using the Gmail connector and keep any work already completed.";
      if (current.sending) {
        const pending = current.pendingApproval;
        if (pending && isQuestionApproval(pending) && current.runs[sessionId]?.transport?.turnId === sourceTurn) {
          await current.submitAskHumanAnswer(instruction);
          return;
        }
        throw new Error("Wait for the current turn to finish before continuing.");
      }
      const latest = current.messages.findLast(message => message.role === "assistant");
      if (latest?.turnId !== sourceTurn) throw new Error("This chat has a newer turn.");
      await runTurn(sessionId, current.threadId || `web-${sessionId}`, instruction, [], "Continue with Gmail", {
        agentMode: "normal", clientRequestId: `gmail-continue-${sourceTurn}`,
      });
    },
    submitAskHumanAnswer: (answer) => {
      const pending = get().pendingApproval;
      if (!pending?.approval_id || !isQuestionApproval(pending)) return;
      const text = String(answer || "").trim();
      if (!text) return;
      const sid = pending.sessionId || pending.session_id || get().sessionId;
      if (!sid) return;

      get().cancelAskHumanSend();
      const pendingCopy: PendingApproval = { ...pending, sessionId: sid };
      const approvalId = pending.approval_id;
      if (approvalId.startsWith("question-")) {
        return api("/api/sessions/" + encodeURIComponent(sid) + "/questions/" + encodeURIComponent(approvalId), {
          method: "POST", body: JSON.stringify({ answer: text }),
        }).then(() => undefined);
      }

      set((state) => ({
        ...patchAskHumanResolution(state, sid, approvalId, text, false),
        runStatus: sid === state.sessionId ? "running" : state.runStatus,
        statusLabel:
          sid === state.sessionId ? "Answer sent · continuing…" : state.statusLabel,
      }));

      const timerId = window.setTimeout(() => {
        void (async () => {
          const undo = get().askHumanUndo;
          if (!undo || undo.timerId !== timerId) return;
          set({ askHumanUndo: null });
          get().dismissToast(undo.toastId);
          try {
            if (get().sessionId !== undo.sessionId) {
              await get().openSession(undo.sessionId);
            }
            if (get().sessionId !== undo.sessionId) {
              get().showToast("Could not send answer — switched sessions");
              return;
            }
            await get().sendMessage(text);
          } catch (err) {
            get().showToast(
              err instanceof Error ? err.message : "Failed to send answer",
            );
          }
        })();
      }, 4000);

      const toastId = uid("t");
      const preview =
        text.length > 48 ? `${text.slice(0, 48).trim()}…` : text;
      const undoState: AskHumanUndoState = {
        timerId,
        toastId,
        sessionId: sid,
        pending: pendingCopy,
      };
      set((state) => ({
        askHumanUndo: undoState,
        toasts: [
          ...state.toasts.slice(-4),
          {
            id: toastId,
            text: `Sending “${preview}”`,
            createdAt: Date.now(),
            actionLabel: "Undo",
            actionId: "ask_human_undo",
          },
        ],
      }));
    },

    deferAskHuman: async () => {
      const pending = get().pendingApproval;
      const sid = pending?.sessionId || pending?.session_id || get().sessionId;
      if (sid && pending?.approval_id.startsWith("question-")) {
        await api("/api/sessions/" + encodeURIComponent(sid) + "/questions/" + encodeURIComponent(pending.approval_id), {
          method: "POST", body: JSON.stringify({ skip: true }),
        });
        return;
      }
      await get().resolveApproval(false);
    },

    submitCredential: async (value) => {
      const pending = get().pendingApproval;
      if (!pending?.approval_id) return;
      const secureQuestion = isQuestionApproval(pending) &&
        credentialIntent(`${pending.question || ""} ${detailText(pending.detail)}`) === "credential";
      if (!isCredentialApproval(pending) && !secureQuestion) return;
      const secret = String(value || "");
      if (!secret) return;
      const sid = pending.sessionId || pending.session_id || get().sessionId;
      if (!sid) return;
      // Clear the banner immediately so the secret UI cannot be re-read.
      set({ pendingApproval: null });
      try {
        const result = await api<{ ok: boolean; error?: string }>(`/api/sessions/${encodeURIComponent(sid)}/browser/credential`, {
          method: "POST",
          body: JSON.stringify({ value: secret, label: pending.label || "" }),
        });
        if (!result.ok) throw new Error(result.error || "Could not store credential");
        suppressCredentialApproval(sid, pending.approval_id);
        updateRun(sid, (r) => ({
          ...r,
          waitingApproval: false,
          pendingApproval: null,
        }));
      } catch (err) {
        // Restore the live approval so a failed deposit can be retried.
        updateRun(sid, (r) => ({ ...r, waitingApproval: true, pendingApproval: pending }));
        if (get().sessionId === sid && !get().pendingApproval) set({ pendingApproval: pending });
        get().showToast(
          err instanceof Error ? err.message : "Could not store credential",
        );
        return;
      }
      // Resume the turn with a SAFE, non-secret message. The handle reaches the
      // agent via ctx.meta on the next turn; the credential itself never rides
      // the message stream.
      try {
        if (get().sessionId !== sid) await get().openSession(sid);
        if (get().sessionId !== sid) throw new Error("Credential saved; reopen the original session to continue.");
        await get().sendMessage("Credential provided securely — continue signing in.");
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : "Failed to resume");
      }
    },

    cancelCredential: async () => {
      const pending = get().pendingApproval;
      if (!pending?.approval_id) return;
      const sid = pending.sessionId || pending.session_id || get().sessionId;
      if (!sid) return;
      // Credential requests are synthetic UI gates, not permission approvals.
      // Dismiss without granting anything or sending a secret to the chat.
      suppressCredentialApproval(sid, pending.approval_id);
      updateRun(sid, (r) => ({
        ...r, waitingApproval: false, pendingApproval: null,
        status: r.status === "waiting_approval" ? "idle" : r.status,
        statusLabel: "Secure send cancelled",
      }));
    },

    addPendingFiles: (files) => {
      if (!files) return;
      const { accepted, rejected } = validateAttachments(Array.from(files));
      if (rejected.length) {
        get().showToast(
          `Skipped ${rejected.length} attachment${rejected.length === 1 ? "" : "s"}: ${rejected.join(", ")}`,
        );
      }
      const list = accepted;
      if (!list.length) return;
      set((s) => {
        const keys = new Set(
          s.pendingFiles.map((f) => `${f.name}:${f.size}:${f.lastModified}`),
        );
        const next = [...s.pendingFiles];
        for (const file of list) {
          const key = `${file.name}:${file.size}:${file.lastModified}`;
          if (!keys.has(key)) {
            keys.add(key);
            next.push(file);
          }
        }
        return {
          pendingFiles: next,
          ...(s.sessionId && s.runs[s.sessionId] ? {
            runs: { ...s.runs, [s.sessionId]: { ...s.runs[s.sessionId], pendingFiles: next } },
          } : {}),
        };
      });
    },

    removePendingFile: (index) =>
      set((s) => {
        const pendingFiles = s.pendingFiles.filter((_, i) => i !== index);
        return {
          pendingFiles,
          ...(s.sessionId && s.runs[s.sessionId] ? {
            runs: { ...s.runs, [s.sessionId]: { ...s.runs[s.sessionId], pendingFiles } },
          } : {}),
        };
      }),

    clearPendingFiles: () => set((s) => ({
      pendingFiles: [],
      ...(s.sessionId && s.runs[s.sessionId] ? {
        runs: { ...s.runs, [s.sessionId]: { ...s.runs[s.sessionId], pendingFiles: [] } },
      } : {}),
    })),

    refreshSessions: async (opts) => {
      if (opts?.background && sessionsRefreshController) return;
      sessionsRefreshController?.abort();
      const controller = new AbortController();
      sessionsRefreshController = controller;
      const observedAt = Date.now();
      try {
        await loadSessionPages(opts?.limit ?? 50, (page, complete) => {
          if (controller.signal.aborted) return;
          noteServerTurns(page.map(session => [session.session_id, serverTurnFromSession(session, observedAt)]));
          // Keep already-known older chats while the remaining pages load.
          const collected = new Map(page.map(session => [session.session_id, session]));
          if (!complete) for (const session of get().sessions) {
            if (!collected.has(session.session_id)) collected.set(session.session_id, session);
          }
          const sessions = sortSessionsPinnedFirst([...collected.values()]);
          saveCachedSessions(sessions);
          const sid = get().sessionId;
          const active = sid
            ? sessions.find((s) => s.session_id === sid)
            : undefined;
          set({
            sessions,
            sessionsError: null,
            bootError: null,
            ...(active && active.title != null
              ? { sessionTitle: String(active.title || "") || null }
              : {}),
          });
        }, controller.signal);
      } catch (err) {
        if (controller.signal.aborted) return;
        const message = err instanceof Error ? err.message : String(err);
        set({ sessionsError: message });
      } finally {
        if (sessionsRefreshController === controller) sessionsRefreshController = null;
      }
    },

    loadProjects: async (query: string = "") => {
      try {
        const url = query ? `/api/projects?q=${encodeURIComponent(query)}` : "/api/projects";
        const res = await api<{ projects?: Project[] }>(url);
        const projects = res.projects || [];
        set({ projects });
        return projects;
      } catch (err) {
        console.error("loadProjects failed", err);
        return [];
      }
    },

    createProject: async (name: string, description: string = "", color: string = "blue") => {
      const res = await api<{ project?: Project }>("/api/projects", {
        method: "POST",
        body: JSON.stringify({ name, description, color }),
      });
      const proj = res.project!;
      await get().loadProjects();
      return proj;
    },

    setSelectedProject: (projectId: string | null) => {
      set({ selectedProjectId: projectId });
    },

    setSessionProject: async (sessionId: string, projectId: string | null) => {
      try {
        const res = await api<{ ok: boolean; project_id?: string | null; project?: Project | null }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/project`,
          {
            method: "POST",
            body: JSON.stringify({ project: projectId || "" }),
          }
        );
        const nextProject =
          res.project ||
          (res.project_id
            ? get().projects.find((p) => p.id === res.project_id) || {
                id: res.project_id,
                name: res.project_id,
              }
            : null);
        sessionDetailCache.delete(sessionId);
        set((s) => {
          const exists = s.sessions.some((sess) => sess.session_id === sessionId);
          const sessions = exists
            ? s.sessions.map((sess) =>
                sess.session_id === sessionId
                  ? { ...sess, project_id: res.project_id, project: nextProject }
                  : sess
              )
            : [
                ...s.sessions,
                {
                  session_id: sessionId,
                  id: sessionId,
                  title: s.sessionTitle,
                  project_id: res.project_id,
                  project: nextProject,
                  status: s.runStatus,
                } as SessionSummary,
              ];
          return {
            sessions,
            ...(s.sessionId === sessionId ? { sessionProject: nextProject } : {}),
          };
        });
        await get().loadProjects();
        await get().refreshSessions();
      } catch (err) {
        get().showToast(`Failed to set project: ${err instanceof Error ? err.message : err}`);
      }
    },

    tagTurnProject: async (sessionId: string, turnIndex: number, projectId: string, tag: string = "") => {
      try {
        await api<{ ok: boolean }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/messages/${turnIndex}/tag`,
          {
            method: "POST",
            body: JSON.stringify({ project: projectId, tag }),
          }
        );
        await get().refreshSessions();
        await get().loadProjects();
        get().showToast("Tagged message to project");
      } catch (err) {
        get().showToast(`Failed to tag message: ${err instanceof Error ? err.message : err}`);
      }
    },

    probeCapabilities: async () => {
      const files = await apiSoft("/api/project/files?q=&limit=1");
      const comet = await apiSoft("/api/comet");
      const browser = await apiSoft("/api/browser");
      const computer = await apiSoft("/api/computer");
      const machine = await apiSoft("/api/machine");
      const models = await apiSoft("/api/models");
      const model = models.ok ? models : await apiSoft("/api/model");
      set((s) => ({
        capabilities: {
          ...s.capabilities,
          projectFiles: files.ok === true,
          cometApi: comet.ok === true,
          browserApi: browser.ok === true,
          computerApi: computer.ok === true,
          machineApi: machine.ok === true,
          modelApi: model.ok === true,
        },
      }));
    },

    loadSlashCatalog: async () => {
      const catalog = await apiSoft("/api/slash-catalog");
      if (catalog.ok && Array.isArray(catalog.data.commands)) {
        const caps = (catalog.data.capabilities || {}) as Record<string, unknown>;
        set((s) => ({
          slashCatalog: mergeServerCatalog(
            catalog.data.commands as unknown[],
            SLASH_COMMANDS,
          ),
          capabilities: {
            ...s.capabilities,
            slashCatalogApi: true,
            cometApi:
              typeof caps.comet === "boolean"
                ? caps.comet
                : s.capabilities.cometApi,
            browserApi:
              typeof caps.browser === "boolean"
                ? caps.browser
                : s.capabilities.browserApi,
            computerApi:
              typeof caps.computer === "boolean"
                ? caps.computer
                : s.capabilities.computerApi,
            machineApi:
              typeof caps.machine === "boolean"
                ? caps.machine
                : s.capabilities.machineApi,
            modelApi:
              typeof caps.models === "boolean"
                ? caps.models
                : s.capabilities.modelApi,
          },
        }));
      } else {
        set((s) => ({
          slashCatalog: SLASH_COMMANDS.slice(),
          capabilities: { ...s.capabilities, slashCatalogApi: false },
        }));
      }
    },

    loadMeta: async () => {
      try {
        const meta = await api<MetaPayload>("/api/meta");
        set({ meta });
      } catch {
        /* optional */
      }
    },

    syncPermissions: async () => {
      // The backend's Once/Session/Full grant is process-wide state that
      // outlives a page refresh. Without this, a refreshed tab falls back to
      // the locally persisted pref and can show "ask" while the server is
      // still auto-approving (or vice versa) until the next approval event.
      try {
        const data = await api<{ ok: boolean; scope: "ask" | "session" | "full"; auto_approve: boolean; profile?: "ask" | "auto_review" | "full"; chat_scoped_profiles?:boolean; legacy_routine?:boolean }>(
          `/api/permissions?session_id=${encodeURIComponent(get().sessionId ?? "")}`,
        );
        if (!data?.ok) return;
        set({
          autoApprove: data.chat_scoped_profiles && !data.legacy_routine ? data.profile !== "ask" : data.auto_approve,
          permissionScope: data.chat_scoped_profiles && !data.legacy_routine ? (data.profile === "full" ? "full" : data.profile === "auto_review" ? "session" : "ask") : data.scope,
        });
      } catch {
        /* optional — keep the locally persisted preference on failure */
      }
    },

    loadModels: async () => {
      const res = await apiSoft("/api/models");
      if (!res.ok) return;
      const parsed = parseModelCatalogPayload(res.data);
      // Catalog default is not a user pin.
      set({
        models: parsed.models,
        modelCatalog: parsed.catalog,
        modelCredentialsLocked: res.data.credentials_locked === true,
        defaultModel: parsed.defaultModel,
      });
    },

    loadSearch: async () => {
      const seq = ++searchLoadSeq;
      let res = await apiSoft("/api/provider-setup/search");
      if (!res.ok && (res.status === 404 || res.status === 405)) res = await apiSoft("/api/provider-setup");
      if (seq !== searchLoadSeq) return false;
      if (!res.ok) return false;
      const parsed = parseSearchFromProviderSetup(res.data);
      if (!parsed) return false;
      set({ searchSetup: parsed });
      return true;
    },

    setSearch: async (patch) => {
      const data = await api("/api/provider-setup/search", {
        method: "POST",
        body: JSON.stringify(patch),
      });
      const parsed = parseSearchFromProviderSetup(data);
      if (parsed) set({ searchSetup: parsed });
    },

    boot: async () => {
      try {
        // High priority: refresh top sessions and restore thread immediately
        set({ bootError: null });
        const urlState = parseUrlState();
        const restoreId = sessionIdToRestoreOnBoot(urlState.sessionId || loadLastSession());
        const opening = restoreId ? get().openSession(restoreId) : undefined;
        const sessionsRefresh = get().refreshSessions({ limit: 40 });
        if (restoreId) {
          try {
            await opening;
            updateUrl({ sessionId: restoreId, agentId: urlState.agentId }, { replace: true });
          } catch (err) {
            if (err instanceof ApiError && err.status === 404) {
              rememberSession(null);
              updateUrl({ sessionId: null, agentId: null }, { replace: true });
              set({
                sessionId: null,
                threadId: null,
                sessionLoading: false,
                error: null,
                runStatus: "idle",
                statusLabel: "Ready",
              });
            } else {
              throw err;
            }
          }
        }
        await sessionsRefresh;
        if (get().sessionsError) set({ bootError: get().sessionsError });

        // Defer secondary background catalogs so thread and sidebar own initial network bandwidth
        const deferSecondary = () => {
          void get().probeCapabilities().then(() => get().loadSlashCatalog());
          void get().loadMeta();
          void get().loadModels();
          void get().loadSearch();
          void get().syncPermissions();
          void get().refreshPersonas();
          void get().refreshTeams();
          void get().loadProjects();
        };
        if (typeof window !== "undefined" && "requestIdleCallback" in window) {
          (window as { requestIdleCallback: (cb: () => void, opts?: { timeout: number }) => void }).requestIdleCallback(deferSecondary, { timeout: 1200 });
        } else {
          setTimeout(deferSecondary, 60);
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        set({ bootError: message });
      }
    },

    receiveJobReply: (sessionId, id, text, attachments, speaker) => {
      updateRun(sessionId, (run) => {
        if (run.messages.some(message => message.id === id)) return run;
        return { ...run, messages: [...run.messages, { id, role: "assistant", text, attachments, speaker }] };
      });
    },

    openSession: async (sessionId) => {
      if (!sessionId) return;
      set({ sessionOpenFailure: null, sessionRefreshError: null, sessionRefreshing: null });
      const openSeq = ++openSessionSeq;
      sessionLoadController?.abort();
      sessionLoadController = null;
      const prevId = get().sessionId;
      if (sessionId !== prevId) {
        historyReplayController?.abort();
        historyReplayController = null;
        updateUrl({ sessionId, agentId: null });
      }
      if (sessionId === prevId) {
        set({ sessionLoading: false });
        updateRun(sessionId, (r) => ({ ...r, needsAttention: false }));
        const pending = get().pendingApproval;
        if (pending?.sessionId === sessionId || pending?.session_id === sessionId) {
          /* keep banner */
        } else if (!pending) {
          set({ pendingApproval: null });
        }
        await revalidateSelectedSession(sessionId, openSeq);
        return;
      }

      set({ remoteExecution: null });

      const prevRun = prevId ? get().runs[prevId] : null;
      const stayMultitask =
        get().tabs.length > 1 ||
        get().tabs.includes(sessionId) ||
        Boolean(prevRun?.sending);

      if (stayMultitask) {
        parkActive();
        if (prevId) touchTab(prevId);
        touchTab(sessionId);
      } else if (prevId) {
        set((s) => ({
          tabs: s.tabs.filter((id) => id !== prevId),
        }));
      }

      const parked = get().runs[sessionId];
      if (parked && parked.messages.length) {
        rememberSession(sessionId);
        const run = ensureRun(sessionId);
        const currentSummary = get().sessions.find((s) => s.session_id === sessionId);
        set({
          sessionId,
          sessionTitle: currentSummary?.title ?? null,
          sessionProject: currentSummary?.project || null,
          sessionLoading: false,
          // The artifact drawer is session-scoped top-level state (not stored
          // per-run), so switching to a parked tab would otherwise leave the
          // previous thread's files — including nested artifacts only the new
          // session owns — visible. Clear now and rehydrate from the server.
          canvasItems: [],
          canvasSelectedPath: null,
          canvasTurnPaths: new Set(),
          activePersonaId: run.personaId || get().activePersonaId || "main",
          pendingApproval:
            run.pendingApproval ||
            (get().pendingApproval?.sessionId === sessionId ||
            get().pendingApproval?.session_id === sessionId
              ? get().pendingApproval
              : null),
          ...syncFromRun({
            ...run,
            needsAttention: false,
            pendingFiles: run.pendingFiles,
          }),
        });
        updateRun(sessionId, (r) => ({ ...r, needsAttention: false }));
        void get().refreshArtifacts();
        if (
          run.messages.some(
            (message) =>
              (message.computerFrames || []).some((frame) => frame.app === "Kageha Browser") ||
              (message.toolCards || []).some((card) => isBrowserToolName(card.name)),
          )
        ) {
          get().setLiveSurface("browser");
        }
        await revalidateSelectedSession(sessionId, openSeq);
        return;
      }

      const cached = sessionDetailCache.get(sessionId);
      if (cached) {
        const cachedData = cached.data as Record<string, any>;
        const threadId = cachedData.thread_id || `web-${cachedData.session_id}`;
        const turnId = String(cachedData.active_turn?.turn_id || "").trim();
        const messages = restoreMissingAssistantRows(
          mapHistoryMessages(cachedData.messages), cachedData.turns || [], turnId,
        );
        const hasHistory = (cachedData.messages?.length || 0) > 0 || (cachedData.turns?.length || 0) > 0;
        const personaId = cachedData.persona_id || (hasHistory ? "main" : get().activePersonaId) || "main";
        const sessionModel = String(cachedData.model_override || "").trim();
        const latestTurn = cachedData.turns?.at(-1);
        const restoredStatus = terminalRunState(cachedData.active_turn?.status || cachedData.status || latestTurn?.status || "idle");
        const cachedRun: SessionRun = {
          ...emptyRun(String(cachedData.session_id), threadId, personaId, sessionModel),
          messages,
          todoBoard: parseTodoBoardPayload(cachedData.todo_board),
          status: restoredStatus.status,
          statusLabel: restoredStatus.label,
          pendingFiles: [],
          queue: get().runs[String(cachedData.session_id)]?.queue || restoredQueue(String(cachedData.session_id)),
        };
        const resolvedProject =
          cachedData.project ||
          (cachedData.project_id
            ? get().projects.find((p) => p.id === cachedData.project_id) || {
                id: cachedData.project_id,
                name: cachedData.project_id,
              }
            : null);
        rememberSession(String(cachedData.session_id));
        set((s) => ({
          sessionId: String(cachedData.session_id),
          sessionTitle: cachedData.title ?? null,
          sessionProject: resolvedProject,
          sessionLoading: false,
          activePersonaId: personaId,
          modelOverride: sessionModel,
          runs: { ...s.runs, [String(cachedData.session_id)]: cachedRun },
          ...syncFromRun(cachedRun),
        }));
        await revalidateSelectedSession(sessionId, openSeq);
        return;
      } else {
        set({
          runStatus: "running",
          statusLabel: "Opening…",
          error: null,
          sessionLoading: true,
        });
      }
      const openingAbort = new AbortController();
      sessionLoadController = openingAbort;
      const openingTimeout = setTimeout(() => openingAbort.abort(), 20_000);
      try {
        const openedAt = Date.now();
        if (prefetchId && prefetchId !== sessionId) prefetchController?.abort();
        const data = await readSession<SessionDetail>(sessionId, openingAbort.signal);
        clearTimeout(openingTimeout);
        if (openSeq !== openSessionSeq) return;
        recordServerTurn(data.session_id, serverTurnFromSession(data, openedAt));
        cacheSessionDetail(data.session_id, data as unknown as Record<string, unknown>);
        const threadId = data.thread_id || `web-${data.session_id}`;
        const turnId = String(data.active_turn?.turn_id || "").trim();
        const messages = restoreMissingAssistantRows(
          mapHistoryMessages(data.messages), data.turns || [], turnId,
        );
        let reattachAssistantId = "";
        const restoredMessages = [...messages];
        if (turnId) {
          const liveUser = String(data.live_user_message || "").trim();
          const hasActiveUser = restoredMessages.some((m) => m.role === "user" && !m.steering && m.turnId === turnId);
          if (liveUser && !isInternalRecoveryMessage(liveUser) && !hasActiveUser) {
            restoredMessages.push({ id: uid("u"), role: "user", text: liveUser, turnId });
          }
          const tail = restoredMessages[restoredMessages.length - 1];
          if (tail?.role === "assistant" && tail.turnId === turnId) {
            reattachAssistantId = tail.id;
            restoredMessages[restoredMessages.length - 1] = {
              ...tail,
              streaming: true,
              speaker: data.active_turn?.speaker || tail.speaker,
              statusLabel: "Reconnecting…",
              turnId,
            };
          } else {
            reattachAssistantId = uid("a");
            restoredMessages.push({
              id: reattachAssistantId,
              role: "assistant",
              speaker: data.active_turn?.speaker,
              text: String(data.live_message || ""),
              streaming: true,
              statusLabel: "Reconnecting…",
              steps: [],
              toolCards: [],
              computerFrames: [],
              turnId,
            });
          }
        }
        const hasHistory =
          (data.messages?.length || 0) > 0 || (data.turns?.length || 0) > 0;
        const personaId =
          data.persona_id ||
          (hasHistory ? "main" : get().activePersonaId) ||
          "main";
        const sessionModel = String(data.model_override || "").trim();
        const latestTurn = data.turns?.at(-1);
        const restoredStatus = terminalRunState(data.active_turn?.status || data.status || latestTurn?.status || "idle");
        const run: SessionRun = {
          ...emptyRun(data.session_id, threadId, personaId, sessionModel),
          messages: restoredMessages,
          todoBoard: parseTodoBoardPayload(data.todo_board),
          status: restoredStatus.status,
          statusLabel: restoredStatus.label,
          pendingFiles: [],
          queue: get().runs[data.session_id]?.queue || restoredQueue(data.session_id),
        };
        const pendingDelivery = pendingDeliveries(data.session_id).filter(item => item.state !== "cancelled").at(-1);
        rememberSession(data.session_id);
        const resolvedProject =
          data.project ||
          (data.project_id
            ? get().projects.find((p) => p.id === data.project_id) || {
                id: data.project_id,
                name: data.project_id,
              }
            : null);
        set((s) => ({
          sessionId: data.session_id,
          sessionTitle: data.title ?? null,
          sessionProject: resolvedProject,
          sessionLoading: false,
          activePersonaId: personaId,
          modelOverride: sessionModel,
          machineActive: false,
          machineBlocked: false,
          machineDetail: "",
          machinePaused: false,
          machineShellRunning: false,
          machineShellLine: "",
          machineNovncUrl: "",
          machineScreenConnected: false,
          machineAdopted: false,
          machineBootPhase: "",
          machineBrowserReady: false,
          desktopLeaseOwner: "",
          liveSplit: false,
          canvasItems: [],
          canvasSelectedPath: null,
          canvasTurnPaths: new Set(),
          runs: { ...s.runs, [data.session_id]: run },
          pendingApproval: data.pending_approval
            ? { ...data.pending_approval, sessionId: data.session_id }
            : null,
          ...syncFromRun(run),
        }));
        // Reconnect before optional surface probes or history replay can use
        // the browser's limited HTTP connections. This snapshot is already
        // authoritative about the active turn, even before SSE attaches.
        if (turnId) {
          reattach(data.session_id, threadId, turnId, {
            assistantId: reattachAssistantId,
            pendingApproval: data.pending_approval,
            initialSnapshot: data,
          });
        }
        const viewAbort = new AbortController();
        historyReplayController = viewAbort;
        const isCurrent = () => openSeq === openSessionSeq &&
          get().sessionId === data.session_id && !viewAbort.signal.aborted;
        if (pendingDelivery) {
          // Restore for review, never auto-submit. A receipt, rather than the
          // latest unrelated active turn, decides whether this text arrived.
          const restoreText = pendingDelivery.text && !get().draft;
          if (restoreText) set({ draft: pendingDelivery.text });
          void api<RequestReceipt>(
            `/api/sessions/${encodeURIComponent(data.session_id)}/requests/${encodeURIComponent(pendingDelivery.id)}`,
            { signal: AbortSignal.any([viewAbort.signal, AbortSignal.timeout(5000)]) },
          ).then(receipt => {
            if (!isCurrent() || !receipt?.receipt) return;
            if (receipt.receipt.turn_id) {
              acknowledgeDelivery(pendingDelivery.id);
              if (restoreText && get().draft === pendingDelivery.text) set({ draft: "" });
              if (receipt.turn?.turn_id && receipt.turn.turn_id !== turnId) void recoverLiveProgress(data.session_id);
            } else {
              get().showToast("Recovered your reply for review. It has not been confirmed as a running task; check before resending.");
            }
          }).catch(() => {
            if (isCurrent()) get().showToast("Recovered your reply for review. Delivery is unconfirmed; check before resending.");
          });
        }
        // Messages are useful immediately. Journal replay may involve hundreds
        // of requests on long threads; it must never gate opening the chat.
        const originalRows = new Map(restoredMessages.map((message) => [message.id, message]));
        void hydrateMessageTranscripts(
          data.session_id, threadId, restoredMessages, data.turns || [], turnId, viewAbort.signal, (hydrated) => {
            if (!isCurrent()) return;
            const replayed = new Map(hydrated.map((message) => [message.id, message]));
            set((state) => {
              const current = state.runs[data.session_id];
              if (!current) return state;
              const messages = current.messages.map((message) =>
                !message.streaming && message === originalRows.get(message.id)
                  ? replayed.get(message.id) || message
                  : message,
              );
              return {
                runs: { ...state.runs, [data.session_id]: { ...current, messages } },
                ...(state.sessionId === data.session_id ? { messages } : {}),
              };
            });
        }).catch(() => { /* Keep plain-text history when replay is unavailable. */ });
        if (sessionModel) {
          get().setPrefs({ mainModel: sessionModel });
        }
        void get().refreshArtifacts();
        // Optional machine/SSH status must never hold openSession or live
        // progress hostage. Bound it and cancel it on a view change.
        const surfaceAbort = new AbortController();
        const cancelSurface = () => surfaceAbort.abort();
        viewAbort.signal.addEventListener("abort", cancelSurface, { once: true });
        const surfaceTimeout = setTimeout(cancelSurface, 4_000);
        void (async () => {
          try {
            try {
              const remote = await api<{
                active?: boolean;
                host_id?: string;
                default_host_id?: string;
                hosts?: Array<{ id: string; name: string; host: string; risk_tier: string }>;
              }>(`/api/remote?session_id=${encodeURIComponent(data.session_id)}`, { signal: surfaceAbort.signal });
              if (!isCurrent() || surfaceAbort.signal.aborted) return;
              const active = Boolean(remote.active);
              const hostId = active ? String(remote.host_id || remote.default_host_id || "") : "";
              const host = remote.hosts?.find((item) => item.id === hostId);
              const hostName = active ? String(host?.name || host?.host || "") : "";
              updateRun(data.session_id, (current) => ({
                ...current,
                remoteActive: active,
                remoteHostId: hostId,
                remoteHostName: hostName,
              }));
              if (get().sessionId === data.session_id) {
                set({
                  remoteActive: active,
                  remoteHostId: hostId,
                  remoteHostName: hostName,
                  remoteHealth: active ? "connecting" : "",
                  remoteDetail: "",
                  remoteRiskTier: active ? String(host?.risk_tier || "") : "",
                });
                if (active) await probeRemoteHealth(data.session_id, hostId, surfaceAbort.signal);
              }
            } catch {
              /* Remote status is optional while opening chat history. */
            }
            if (!isCurrent() || surfaceAbort.signal.aborted) return;
            try {
              const machine = await apiSoft(
                `/api/sessions/${encodeURIComponent(data.session_id)}/machine`, { signal: surfaceAbort.signal },
              );
              if (!isCurrent() || surfaceAbort.signal.aborted) return;
              if (machine.ok && typeof machine.data.active === "boolean") {
                updateRun(data.session_id, (current) => ({
                  ...current,
                  machineActive: Boolean(machine.data.active),
                }));
                set({
                  machineActive: machine.data.active,
                  machineBlocked: machine.data.blocked === true,
                  machinePaused: Boolean(
                    (machine.data.runtime as { paused?: boolean; status?: string } | undefined)?.paused
                    || (machine.data.runtime as { status?: string } | undefined)?.status === "paused",
                  ),
                  machineDetail:
                    typeof machine.data.error === "string"
                      ? machine.data.error
                      : "",
                  machineNovncUrl:
                    typeof (machine.data.runtime as { novncUrl?: string } | undefined)?.novncUrl === "string"
                      && String((machine.data.runtime as { novncUrl?: string }).novncUrl)
                      || ((machine.data.runtime as { novncReady?: boolean } | undefined)?.novncReady
                        ? "ready"
                        : ""),
                  machineAdopted: Boolean(
                    (machine.data.runtime as { adopted?: boolean } | undefined)?.adopted,
                  ),
                  machineBrowserReady: Boolean(
                    (machine.data.runtime as { browserReady?: boolean } | undefined)?.browserReady,
                  ),
                  machineBootPhase: machine.data.active
                    ? ((machine.data.runtime as { browserReady?: boolean } | undefined)?.browserReady
                      ? "browser"
                      : "screen")
                    : "",
                });
                if (machine.data.active && shouldAgentFocus({
                  pinnedAt: get().surfacePinnedAt, controlling: get().surfaceControlling,
                  now: Date.now(), firstFrameOfTurn: false,
                })) {
                  get().setLiveSplit(true);
                  get().setLiveSurface("browser");
                }
              }
            } catch {
              /* ignore */
            }
            if (
              shouldAgentFocus({ pinnedAt: get().surfacePinnedAt, controlling: get().surfaceControlling,
                now: Date.now(), firstFrameOfTurn: false }) && restoredMessages.some(
                (message) =>
                  (message.computerFrames || []).some((frame) => frame.app === "Kageha Browser") ||
                  (message.toolCards || []).some((card) => isBrowserToolName(card.name)),
              )
            ) {
              if (get().machineActive) get().setLiveSplit(true);
              else get().setLiveSurface("browser");
            }
          } finally {
            clearTimeout(surfaceTimeout);
            viewAbort.signal.removeEventListener("abort", cancelSurface);
          }
        })();
      } catch (err) {
        if (openSeq !== openSessionSeq) return;
        const message = openingAbort.signal.aborted
          ? "Thread loading timed out. Please try opening it again."
          : err instanceof Error ? err.message : String(err);
        set({
          // A failed read does not change the background task's status.
          runStatus: get().runs[get().sessionId || ""]?.status || "idle",
          statusLabel: "Could not open thread",
          sessionLoading: false,
          sessionOpenFailure: { sessionId, message },
          error: message,
        });
        get().showToast(message);
        throw err;
      } finally {
        clearTimeout(openingTimeout);
        if (sessionLoadController === openingAbort) sessionLoadController = null;
      }
    },

    recoverLiveProgress,
    noteServerTurn,
    refreshSessionStatus: async () => {
      await Promise.all([get().refreshSessions(), recoverLiveProgress()]);
    },
    followRunChange: (sessionId, turnId) => {
      const run = get().runs[sessionId];
      if (!run?.threadId || run.sending || reattachTurns.get(sessionId) === turnId) return;
      updateRun(sessionId, (current) => ({
        ...current,
        clientRequestId: undefined,
        pendingApproval: null,
        todoBoard: null,
      }));
      reattach(sessionId, run.threadId, turnId);
    },

    prefetchSession: (sessionId: string) => {
      const id = String(sessionId || "").trim();
      if (!id || sessionDetailCache.has(id) || get().runs[id]?.messages.length || prefetchId === id) return;
      prefetchController?.abort();
      const controller = new AbortController();
      prefetchController = controller;
      prefetchId = id;
      void readSession<SessionDetail>(id, controller.signal).then(data => {
        if (!controller.signal.aborted && data?.session_id) cacheSessionDetail(data.session_id, data as unknown as Record<string, unknown>);
      }).catch(() => {}).finally(() => {
        if (prefetchController === controller) { prefetchController = null; prefetchId = ""; }
      });
    },

    newChat: async () => {
      set({ sessionOpenFailure: null, sessionRefreshing: null, sessionRefreshError: null });
      ++openSessionSeq;
      sessionLoadController?.abort();
      sessionLoadController = null;
      historyReplayController?.abort();
      historyReplayController = null;
      // Show the CommandCenter hero input — session is created on first send
      const previousId = get().sessionId;
      if (previousId) {
        const prevRun = get().runs[previousId];
        if (prevRun?.waitingApproval || prevRun?.sending) {
          // Keep a Plan waiting for Build, or a still-running turn, alive in
          // the background instead of killing it — starting a new chat
          // should not silently cancel work in flight. Parking + keeping the
          // tab open mirrors openSession's multitask path so the run stays
          // reachable and can be resumed from the sidebar/tab bar.
          parkActive();
          touchTab(previousId);
        }
      }
      rememberSession(null);
      updateUrl({ sessionId: null, agentId: null });
      set({
        sessionId: null,
        sessionLoading: false,
        threadId: null,
        sessionTitle: null,
        draft: "",
        messages: [],
        pendingFiles: [],
        pendingApproval: null,
        sending: false,
        abort: null,
        runStatus: "idle",
        statusLabel: "Ready",
        error: null,
        canvasItems: [],
        canvasSelectedPath: null,
        canvasTurnPaths: new Set(),
        canvasOpen: false,
        canvasPreviewRequest: null,
        todoBoard: null,
        workspaceSurface: "monitor",
        liveSurface: "computer",
        surfacePinnedAt: null,
        surfaceControlling: false,
        liveRevealedThisTurn: false,
        liveSplit: false,
        remoteExecution: null,
        browserPanelMode: "default",
        computerPanelMode: "default",
        // No run backing the hero screen — clear the session-scoped model
        // pin so it can't leak from whatever tab was open a moment ago.
        modelOverride: "",
        machineActive: false,
        machineBlocked: false,
        machineDetail: "",
        machinePaused: false,
        machineShellRunning: false,
        machineShellLine: "",
        machineNovncUrl: "",
        machineScreenConnected: false,
        machineAdopted: false,
        machineBootPhase: "",
        machineBrowserReady: false,
        remoteActive: false,
        remoteHostId: "",
        remoteHostName: "",
        remoteHealth: "",
        remoteDetail: "",
        remoteRiskTier: "",
        desktopLeaseOwner: "",
      });
    },

    newSession: async (opts = {}) => {
      const parallel = opts.parallel === true;
      const keepPending =
        opts.keepPending === true ||
        (parallel && get().pendingFiles.length > 0);
      const previousId = get().sessionId;
      const carryPendingMachine = !previousId && get().machineActive;
      const carryPendingRemote = !previousId && get().remoteActive;
      const carryRemoteHostId = carryPendingRemote ? get().remoteHostId : "";
      const carryRemoteHostName = carryPendingRemote ? get().remoteHostName : "";
      const previousRun = previousId ? get().runs[previousId] : null;
      const keepPreviousTab = parallel || Boolean(previousRun?.sending);
      const carriedPending = keepPending ? get().pendingFiles.slice() : [];

      if (keepPreviousTab) {
        parkActive();
      } else if (previousId) {
        set((s) => {
          const rest = { ...s.runs };
          delete rest[previousId];
          return {
            tabs: s.tabs.filter((id) => id !== previousId),
            runs: rest,
            pendingApproval:
              s.pendingApproval?.sessionId === previousId
                ? null
                : s.pendingApproval,
          };
        });
      }

      const activeProjId = get().selectedProjectId;
      const data = await api<{
        session_id: string;
        thread_id?: string;
        project_id?: string | null;
        project?: Project | null;
      }>(
        "/api/sessions",
        {
          method: "POST",
          body: JSON.stringify({ project: activeProjId || undefined }),
        },
      );
      const nextId = String(data.session_id || "").trim();
      if (!nextId) throw new Error("server returned no session_id");
      if (previousId && nextId === previousId) {
        throw new Error("new session reused the current session id");
      }
      opts.beforeOpen?.();
      const threadId = data.thread_id || `web-${nextId}`;
      const run = stampPendingRemote(
        stampPendingMachine(
          {
            ...emptyRun(nextId, threadId, get().activePersonaId || "main"),
            pendingFiles: carriedPending,
          },
          carryPendingMachine,
        ),
        carryPendingRemote,
        carryRemoteHostId,
        carryRemoteHostName,
      );
      rememberSession(nextId);
      const prefs = get().prefs;
      const projObj =
        data.project ||
        (activeProjId
          ? get().projects.find((p) => p.id === activeProjId) || {
              id: activeProjId,
              name: activeProjId,
            }
          : null);
      const optimisticSession: SessionSummary = {
        session_id: nextId,
        thread_id: threadId,
        title: null,
        active: true,
        updated_at: Date.now() / 1000,
        project_id: activeProjId || null,
        project: projObj,
        persona_id: get().activePersonaId || "main",
      };
      set((s) => ({
        sessionId: nextId,
        sessionTitle: null,
        sessionProject: projObj,
        preparingSends: parallel ? s.preparingSends.map(pending => pending.sessionId === previousId ? { ...pending, sessionId: nextId } : pending) : s.preparingSends,
        attachmentUpload: parallel && s.attachmentUpload?.status === "uploading" && s.attachmentUpload.sessionId === previousId ? { ...s.attachmentUpload, sessionId: nextId } : s.attachmentUpload,
        draft: "",
        error: null,
        pendingApproval: null,
        agentMode: prefs.defaultAgentMode,
        sessions: [optimisticSession, ...s.sessions.filter((item) => item.session_id !== nextId)],
        machineActive: carryPendingMachine,
        machineBlocked: false,
        machineDetail: "",
        machinePaused: false,
        machineShellRunning: false,
        machineShellLine: "",
        machineNovncUrl: "",
        machineScreenConnected: false,
        machineAdopted: false,
        machineBootPhase: "",
        machineBrowserReady: false,
        remoteActive: carryPendingRemote,
        remoteHostId: carryRemoteHostId,
        remoteHostName: carryRemoteHostName,
        remoteHealth: "",
        remoteDetail: "",
        remoteRiskTier: "",
        desktopLeaseOwner: "",
        autoApprove: !prefs.defaultAskMode,
        permissionScope: prefs.defaultAskMode
          ? "ask"
          : get().permissionScope === "full"
            ? "full"
            : "session",
        runs: { ...s.runs, [nextId]: run },
        tabs: parallel || keepPreviousTab ? [...s.tabs.filter((id) => id !== nextId), nextId] : [],
        pendingFiles: carriedPending,
        canvasItems: [],
        canvasSelectedPath: null,
        canvasTurnPaths: new Set(),
        canvasOpen: false,
        canvasPreviewRequest: null,
        todoBoard: null,
        workspaceSurface: "monitor",
        liveSurface: "computer",
        surfacePinnedAt: null,
        surfaceControlling: false,
        liveRevealedThisTurn: false,
        liveSplit: false,
        remoteExecution: null,
        browserPanelMode: "default",
        computerPanelMode: "default",
        ...syncFromRun(run),
      }));
      if (prefs.defaultAgentMode === "normal") {
        get().clearComposerChip({ resetMode: false });
      } else {
        get().setComposerChip("mode", prefs.defaultAgentMode);
      }
      if (carryPendingRemote) await activateRemote(nextId, carryRemoteHostId);
      await get().refreshSessions();
    },

    closeTab: async (sessionId) => {
      if (!sessionId) return;
      set((s) => ({
        tabs: s.tabs.filter((id) => id !== sessionId),
      }));
      if (get().sessionId === sessionId) {
        const next = get().tabs[get().tabs.length - 1];
        if (next) await get().openSession(next);
        else {
          set({
            sessionId: null,
            threadId: null,
            sessionTitle: null,
            sessionProject: null,
            messages: [],
            pendingFiles: [],
            pendingApproval: null,
            sending: false,
            abort: null,
            runStatus: "idle",
            statusLabel: "Ready",
            todoBoard: null,
            modelOverride: "",
          });
        }
      }
      await get().refreshSessions();
    },

    ensureSession: async (beforeOpen, canOpen) => {
      const { sessionId, threadId } = get();
      if (sessionId && threadId) return { sessionId, threadId };
      // Never mint a second session just because threadId was dropped from
      // top-level state — recover from the run or the stable web-{id} binding.
      if (sessionId) {
        const run = get().runs[sessionId];
        const recovered =
          (run?.threadId && String(run.threadId)) ||
          threadId ||
          `web-${sessionId}`;
        if (run && !run.threadId) {
          updateRun(sessionId, (r) => ({ ...r, threadId: recovered }));
        } else {
          set({ threadId: recovered });
        }
        return { sessionId, threadId: recovered };
      }
      const activeProjId = get().selectedProjectId;
      const data = await api<{
        session_id: string;
        thread_id?: string;
        project_id?: string | null;
        project?: Project | null;
      }>(
        "/api/sessions",
        {
          method: "POST",
          body: JSON.stringify({ project: activeProjId || undefined }),
        },
      );
      const nextId = String(data.session_id || "").trim();
      const nextThread = data.thread_id || `web-${nextId}`;
      if (beforeOpen) await beforeOpen(nextId);
      if (canOpen && !canOpen()) throw new DOMException("Request preparation cancelled", "AbortError");
      rememberSession(nextId);
      const pending = get().pendingFiles.slice();
      const run = stampPendingRemote(
        stampPendingMachine(
          {
            ...emptyRun(
              nextId,
              nextThread,
              get().activePersonaId || "main",
              // Carry a pre-session (hero screen) pick so the syncFromRun below
              // doesn't reset modelOverride to "" on first send.
              get().modelOverride || "",
            ),
            pendingFiles: pending,
          },
          get().machineActive,
        ),
        get().remoteActive,
        get().remoteActive ? get().remoteHostId : "",
        get().remoteActive ? get().remoteHostName : "",
      );
      const projObj =
        data.project ||
        (activeProjId
          ? get().projects.find((p) => p.id === activeProjId) || {
              id: activeProjId,
              name: activeProjId,
            }
          : null);
      const optimisticSession: SessionSummary = {
        session_id: nextId,
        thread_id: nextThread,
        title: null,
        active: true,
        updated_at: Date.now() / 1000,
        project_id: activeProjId || null,
        project: projObj,
        persona_id: get().activePersonaId || "main",
      };
      set((s) => ({
        sessionId: nextId,
        sessionTitle: null,
        sessionProject: projObj,
        preparingSends: s.preparingSends.map(pending => pending.sessionId === null ? { ...pending, sessionId: nextId } : pending),
        attachmentUpload: s.attachmentUpload?.status === "uploading" && s.attachmentUpload.sessionId === null ? { ...s.attachmentUpload, sessionId: nextId } : s.attachmentUpload,
        runs: { ...s.runs, [nextId]: run },
        sessions: [optimisticSession, ...s.sessions.filter((item) => item.session_id !== nextId)],
        ...syncFromRun(run),
        // Keep composer attachments across the mint (syncFromRun alone is fine
        // when the run carries them; reaffirm so a race cannot drop them).
        pendingFiles: pending.length ? pending : s.pendingFiles,
      }));
      if (run.remoteActive) await activateRemote(nextId, run.remoteHostId);
      await get().refreshSessions();
      return { sessionId: nextId, threadId: nextThread };
    },

    renameSession: async (title, sessionId = get().sessionId ?? undefined) => {
      if (!sessionId) return;
      const data = await patchSession(sessionId, { title });
      const savedTitle = data.title ?? title;
      sessionDetailCache.delete(sessionId);
      set((s) => ({
        sessions: applySessionFlagsLocally(s.sessions, sessionId, { title: savedTitle }),
        ...(s.sessionId === sessionId ? { sessionTitle: savedTitle } : {}),
      }));
      await get().refreshSessions();
    },

    deleteSession: async (sessionId) => {
      if (!sessionId) return;
      try {
        await deleteSessionApi(sessionId);
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
        throw err;
      }
      clearDeliverySession(sessionId);
      sessionDetailCache.delete(sessionId);
      if (prefetchId === sessionId) prefetchController?.abort();
      set((s) => {
        const sessions = s.sessions.filter((x) => x.session_id !== sessionId);
        const tabs = s.tabs.filter((id) => id !== sessionId);
        const runs = { ...s.runs };
        delete runs[sessionId];
        return {
          sessions,
          tabs,
          runs,
          pendingApproval:
            s.pendingApproval?.sessionId === sessionId ||
            s.pendingApproval?.session_id === sessionId
              ? null
              : s.pendingApproval,
        };
      });
      if (get().sessionId === sessionId) {
        const next = get().tabs[get().tabs.length - 1];
        // The delete has already committed. A failed fallback view cannot undo it.
        if (next) await get().openSession(next).catch(() => {});
        if (get().sessionId === sessionId) {
          await get().newChat();
          set({ sessionProject: null });
        }
      }
      await get().refreshSessions().catch(() => {});
    },

    forkSession: async (sessionId) => {
      try {
        const result = await api<{ session_id: string; unavailable_attachments?: number }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/fork`, { method: "POST" },
        );
        await get().refreshSessions();
        await get().openSession(result.session_id);
        get().showToast(result.unavailable_attachments
          ? `Session forked · ${result.unavailable_attachments} attachments could not be copied`
          : "Session forked");
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
        throw err;
      }
    },

    exportSession: async (sessionId) => {
      try {
        get().showToast("Preparing session archive…");
        const unavailable = await downloadSession(sessionId);
        get().showToast(unavailable
          ? `Session exported · ${unavailable} attachments could not be copied`
          : "Session exported");
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
        throw err;
      }
    },

    importSession: async (file) => {
      try {
        const result = await uploadSession(file);
        await get().refreshSessions();
        await get().openSession(result.session_id);
        get().showToast("Session imported");
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
        throw err;
      }
    },

    archiveSession: async (sessionId, archived) => {
      if (!sessionId) return;
      try {
        const result = await patchSession(sessionId, { archived });
        set((s) => ({
          sessions: s.sessions.map((session) => session.session_id === sessionId ? { ...session, ...result } : session),
        }));
        await get().refreshSessions();
        if (archived && result.lifecycle_status === "closing") {
          get().showToast("Closing session · review any SessionEnd hook approval");
        } else if (result.archived) {
          get().showToast("Archived · find it under Archived threads");
        } else if (!archived) {
          get().showToast("Session unarchived");
        }
        return result;
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
        throw err;
      }
    },

    pinSession: async (sessionId, pinned) => {
      if (!sessionId) return;
      try {
        const result = await patchSession(sessionId, { pinned });
        set((s) => ({
          sessions: sortSessionsPinnedFirst(applySessionFlagsLocally(s.sessions, sessionId, { pinned: result.pinned ?? pinned })),
        }));
        await get().refreshSessions();
      } catch (err) {
        get().showToast(err instanceof Error ? err.message : String(err));
        throw err;
      }
    },

    removeQueuedMessage: (index) => {
      const sid = get().sessionId;
      if (sid) updateRun(sid, (run) => ({ ...run, queue: run.queue.filter((_, i) => i !== index) }));
    },

    sendNowMessage: async (textOverride, queuedMessage) => {
      if (queuedMessage?.needsFiles) { get().showToast("Reattach this restored message's files before sending."); return; }
      const { sessionId, draft, pendingFiles } = get();
      const text = (textOverride ?? draft).trim();
      const files = (queuedMessage?.files || pendingFiles).slice();
      if ((!text && !files.length) || get().attachmentUpload?.status === "uploading") return;
      const run = sessionId ? get().runs[sessionId] : undefined;
      if (!sessionId || !run || (!run.sending && !queuedMessage)) {
        if (!queuedMessage) await get().sendMessage(textOverride);
        return;
      }
      if (immediateRequests.has(sessionId)) return;
      immediateRequests.add(sessionId);
      let startupReleased = false;
      const uploadAbort = new AbortController();
      const stillOwnsRequest = () => get().runs[sessionId]?.clientRequestId === run.clientRequestId;
      const stoppedSinceStart = () => run.status !== "cancelled" && get().runs[sessionId]?.status === "cancelled";
      const threadId = run.threadId || get().threadId || `web-${sessionId}`;
      try {
        await remoteActivations.get(sessionId);
        await machineActivations.get(sessionId);
        if (!stillOwnsRequest() || get().sessionId !== sessionId || stoppedSinceStart()) return;
        // Wait for cancellation on the server before starting a replacement.
        // A failed cancellation leaves the current turn and draft intact.
        if (run.sending) {
          if (run.clientRequestId) markDelivery(run.clientRequestId, "cancelled");
          get().showToast("Stopping the current turn to send your message…");
          const cancelled = await api<{ superseded?: boolean }>("/api/chat/cancel", { method: "POST", signal: AbortSignal.timeout(8000), body: JSON.stringify({
            session_id: sessionId, thread_id: threadId, immediate: true, client_request_id: run.clientRequestId, turn_id: run.transport?.turnId || undefined,
          }) });
          if (cancelled.superseded) throw new Error("This chat moved to a newer turn. Review it before sending now.");
        }
        if (!stillOwnsRequest() || stoppedSinceStart()) return;
        reattachCancellers.get(sessionId)?.({ cancelled: true });
        reattachCancellers.delete(sessionId);
        reattachTurns.delete(sessionId);
        peerMonitorCancellers.get(sessionId)?.();
        peerMonitorCancellers.delete(sessionId);
        get().runs[sessionId]?.abort?.abort();
        updateRun(sessionId, cancelInFlightRun);
        if (get().sessionId !== sessionId) return;
        updateRun(sessionId, r => ({ ...r, sending: true, status: "running", statusLabel: "Sending now…" }));
        if (files.length) uploadControllers.set(sessionId, uploadAbort);
        const attachments = files.length ? await uploadSessionFiles(sessionId, files, undefined, uploadAbort.signal) : [];
        if (uploadAbort.signal.aborted || !stillOwnsRequest()) return;
        if (get().sessionId !== sessionId) {
          updateRun(sessionId, r => ({ ...r, sending: false }));
          return;
        }
        updateRun(sessionId, r => ({ ...r,
          queue: queuedMessage ? r.queue.filter(item => item !== queuedMessage) : r.queue,
          pendingFiles: queuedMessage ? r.pendingFiles : r.pendingFiles.filter(file => !files.includes(file)),
        }));
        const savedDraft = get().draft;
        if (!queuedMessage) set({ pendingFiles: get().pendingFiles.filter(file => !files.includes(file)) });
        // runTurn starts synchronously; release the startup lock so this turn
        // can itself be interrupted and remaining queued messages can drain.
        immediateRequests.delete(sessionId);
        startupReleased = true;
        const slashMode = parseModeSlash(text);
        if (slashMode) get().setAgentMode(slashMode);
        const sending = runTurn(sessionId, threadId, text, attachments, text || "(attachments)", { agentMode: slashMode || get().agentMode });
        if (queuedMessage || savedDraft !== draft) set({ draft: savedDraft });
        await sending;
      } catch (err) {
        if (!stillOwnsRequest()) return;
        if (get().runs[sessionId]?.statusLabel === "Sending now…") {
          updateRun(sessionId, r => ({ ...r, sending: false, status: uploadAbort.signal.aborted ? "cancelled" : "error", statusLabel: uploadAbort.signal.aborted ? "Upload cancelled" : "Send failed" }));
        }
        if (!uploadAbort.signal.aborted && get().sessionId === sessionId) set({ error: err instanceof Error ? err.message : String(err) });
      } finally {
        if (uploadControllers.get(sessionId) === uploadAbort) uploadControllers.delete(sessionId);
        if (uploadAbort.signal.aborted && stillOwnsRequest() && get().runs[sessionId]?.statusLabel === "Sending now…") {
          updateRun(sessionId, r => ({ ...r, sending: false, status: "cancelled", statusLabel: "Upload cancelled" }));
        }
        if (!startupReleased) immediateRequests.delete(sessionId);
      }
    },

    steerMessage: async (textOverride, queuedMessage) => {
      if (queuedMessage?.needsFiles) { get().showToast("Reattach this restored message's files before sending."); return; }
      const { sessionId, draft, pendingFiles } = get();
      const text = (textOverride ?? draft).trim();
      const files = (queuedMessage?.files || pendingFiles).slice();
      if (!text && !files.length) return;
      const run = sessionId ? get().runs[sessionId] : undefined;
      if (!sessionId || !run?.sending) {
        if (queuedMessage) return;
        await get().sendMessage(textOverride);
        return;
      }
      if (steeringRequests.has(sessionId)) return;
      steeringRequests.add(sessionId);
      const turnId = run.transport?.turnId || [...run.messages].reverse().find((m) => m.streaming)?.turnId;
      const messageId = uid("u");
      try {
        const attachments = files.length ? await uploadSessionFiles(sessionId, files) : [];
        const message = attachments.length
          ? [text, "Attached files:\n" + attachments.map((path) => `- \`${path}\``).join("\n")].filter(Boolean).join("\n\n")
          : text;
        flushStreamPatches();
        const activeAssistant = get().runs[sessionId]?.messages.findLast((m) => m.role === "assistant" && m.turnId === turnId);
        const steeringBoundary = { ...captureSteeringBoundary(activeAssistant),
          afterSequence: get().runs[sessionId]?.transport?.lastSequence || 0 };
        // Insert before POST: the controller acknowledgement can arrive over
        // the stream before the HTTP acceptance response reaches the browser.
        updateRun(sessionId, (current) => ({
          ...current,
          messages: [...current.messages, { id: messageId, role: "user", text: text || "(attachments)", attachments, steering: "pending", steeringId: messageId, steeringText: message, steeringBoundary, turnId }],
          pendingFiles: get().sessionId === sessionId ? get().pendingFiles : current.pendingFiles,
        }));
        await api("/api/chat/steer", { method: "POST", body: JSON.stringify({ session_id: sessionId, turn_id: turnId || "", message_id: messageId, message, steering_boundary: steeringBoundary }) });
        updateRun(sessionId, (current) => ({
          ...current,
          queue: queuedMessage ? current.queue.filter((item) => item !== queuedMessage) : current.queue,
          pendingFiles: queuedMessage ? current.pendingFiles : current.pendingFiles.filter((file) => !files.includes(file)),
        }));
        if (get().sessionId === sessionId) {
          set({
            ...(!queuedMessage && get().draft === draft ? { draft: "" } : {}),
            ...(!queuedMessage ? { pendingFiles: get().pendingFiles.filter((file) => !files.includes(file)) } : {}),
            error: null,
          });
          get().showToast("Steering queued · applies after the current tool call");
        }
      } catch (err) {
        updateRun(sessionId, (current) => ({ ...current, messages: current.messages.filter((m) => m.id !== messageId) }));
        if (get().sessionId === sessionId) {
          set({ error: err instanceof Error ? err.message : String(err) });
          get().showToast("Steering was not sent. Your draft is kept; you can Queue it instead.");
        }
      } finally {
        steeringRequests.delete(sessionId);
      }
    },

    sendMessage: async (textOverride) => {
      const text = (textOverride ?? get().draft).trim();
      // Snapshot before any await — ensureSession / newSession can sync an
      // empty run and wipe top-level pendingFiles mid-send.
      const files = get().pendingFiles.slice();
      if (!text && !files.length) return;

      if (isModeOnlyComposerText(text) && !files.length) {
        const mode = text.replace(/^\//, "").trim().toLowerCase() as AgentMode;
        get().setAgentMode(mode);
        set({ draft: "" });
        const label = mode[0].toUpperCase() + mode.slice(1);
        get().showToast(
          mode === "normal"
            ? "Normal mode"
            : `${label} mode · describe the real task, then Send`,
        );
        return;
      }

      // Meta slash (admin / bare skill prefix): never mint a session.
      if (!files.length && isMetaOnlySlash(text)) {
        const low = text.toLowerCase();
        if (
          isComputerAdminSlash(text) &&
          get().capabilities.computerApi !== false
        ) {
          const { dispatchNativeAdminSlash } = await import("../lib/slash");
          set({ draft: "" });
          dispatchNativeAdminSlash(text);
          return;
        }
        if (
          isMachineAdminSlash(text) &&
          get().capabilities.machineApi !== false
        ) {
          const { dispatchNativeAdminSlash } = await import("../lib/slash");
          set({ draft: "" });
          dispatchNativeAdminSlash(text);
          return;
        }
        if (
          (low === "/browser" ||
            low.startsWith("/browser ") ||
            low === "/comet" ||
            low.startsWith("/comet ")) &&
          get().capabilities.browserApi !== false
        ) {
          const { dispatchNativeAdminSlash } = await import("../lib/slash");
          set({ draft: "" });
          dispatchNativeAdminSlash(text);
          return;
        }
        // Bare `/computer` / `/computer_use` / other skill with no task body.
        if (/^\/[a-z][a-z0-9_-]*$/i.test(text)) {
          set({ draft: `${text} ` });
          get().showToast(
            low === "/computer" || low === "/computer_use"
              ? "Computer-use · type a task after /computer, then send"
              : low === "/machine"
                ? "Task machine · type a task after /machine, then send"
                : `Skill · type a task after ${text}, then send`,
          );
          return;
        }
      }

      // This is local preparation, not a server acceptance. Keep it separate
      // from the transcript until runTurn owns the request.
      const startingSession = get().sessionId;
      const queueing = get().composerChip.kind !== "multitask" && Boolean(startingSession && get().runs[startingSession]?.sending);
      if (!queueing && get().preparingSends.some(pending => pending.sessionId === startingSession)) return;
      const preparationId = queueing ? "" : uid("prepare");
      const uploadAttemptId = preparationId || uid("upload");
      if (preparationId) set(s => ({ preparingSends: [...s.preparingSends, { id: preparationId, sessionId: startingSession, text }] }));
      const cancelled = () => Boolean(preparationId && !get().preparingSends.some(pending => pending.id === preparationId));
      const checkPreparation = () => { if (cancelled()) throw new DOMException("Request preparation cancelled", "AbortError"); };
      try {
      if (get().composerChip.kind === "multitask") {
        get().clearComposerChip({ resetMode: false });
        try {
          await get().newSession({ parallel: true, keepPending: true, beforeOpen: checkPreparation });
          if (cancelled()) return;
          if (preparationId) set(s => ({ preparingSends: s.preparingSends.map(pending => pending.id === preparationId ? { ...pending, sessionId: s.sessionId } : pending) }));
        } catch (err) {
          if (cancelled()) return;
          set({
            runStatus: "error",
            statusLabel: "Error",
            error: err instanceof Error ? err.message : String(err),
          });
          return;
        }
      }

      if (get().attachmentUpload?.status === "uploading") return;
      if (files.length) set({ attachmentUpload: { sessionId: get().sessionId, status: "uploading", completed: 0, total: files.length, text, attemptId: uploadAttemptId } });
      let session: { sessionId: string; threadId: string };
      try {
        session = await get().ensureSession(undefined, () => !cancelled());
        if (cancelled()) return;
        await remoteActivations.get(session.sessionId);
        if (cancelled()) return;
        await machineActivations.get(session.sessionId);
        if (cancelled()) return;
      } catch (err) {
        if (cancelled()) return;
        set(s => ({ attachmentUpload: s.attachmentUpload?.attemptId === uploadAttemptId ? null : s.attachmentUpload, error: err instanceof Error ? err.message : String(err) }));
        return;
      }
      const { sessionId, threadId } = session;
      ensureRun(sessionId);

      const active = get().runs[sessionId];
      if (active?.sending) {
        const queued: QueuedMessage = {
          text,
          files,
        };
        updateRun(sessionId, (r) => ({
          ...r,
          queue: [...r.queue, queued],
          pendingFiles: [],
          status: "running",
        }));
        set(s => ({ draft: "", pendingFiles: [], attachmentUpload: s.attachmentUpload?.attemptId === uploadAttemptId ? null : s.attachmentUpload }));
        return;
      }

      // Mark busy synchronously before the awaits below, so a new-thread action
      // fired mid-send parks this run instead of deleting an in-flight turn.
      updateRun(sessionId, (r) => ({ ...r, sending: true,
        pendingFiles: [...new Set([...files, ...(get().sessionId === sessionId ? get().pendingFiles : r.pendingFiles)])] }));

      let attachments: string[] = [];
      const controller = new AbortController();
      if (files.length) uploadControllers.set(sessionId, controller);
      try {
        attachments = await uploadSessionFiles(sessionId, files, (completed, total) => {
          if (!cancelled() && get().attachmentUpload?.attemptId === uploadAttemptId)
            set({ attachmentUpload: { sessionId, status: "uploading", completed, total, text, attemptId: uploadAttemptId } });
        }, controller.signal);
      } catch (err) {
        if (cancelled()) return;
        updateRun(sessionId, r => ({ ...r, sending: false, pendingFiles: [...new Set([...files, ...r.pendingFiles])] }));
        const uploadCancelled = controller.signal.aborted;
        set({
          attachmentUpload: uploadCancelled ? null : { sessionId, status: "failed", completed: get().attachmentUpload?.completed || 0, total: files.length, text, attemptId: uploadAttemptId },
          ...(get().sessionId === sessionId ? {
            draft: get().draft || text,
            error: uploadCancelled ? null : `Upload failed. Your files are kept — tap Retry upload. ${err instanceof Error ? err.message : String(err)}`,
            runStatus: uploadCancelled ? "idle" : "error",
            statusLabel: uploadCancelled ? "Upload cancelled" : "Upload failed",
          } : {}),
        });
        return;
      } finally {
        if (uploadControllers.get(sessionId) === controller) uploadControllers.delete(sessionId);
      }
      if (cancelled()) {
        return;
      }
      set(s => ({ attachmentUpload: s.attachmentUpload?.attemptId === uploadAttemptId ? null : s.attachmentUpload }));
      updateRun(sessionId, (r) => ({ ...r, pendingFiles: r.pendingFiles.filter(file => !files.includes(file)) }));

      const display =
        text +
        (attachments.length
          ? `\n\nAttached files:\n${attachments.map((p) => `- \`${p}\``).join("\n")}`
          : "");

      const slashMode = parseModeSlash(text);
      if (slashMode) get().setAgentMode(slashMode);
      const mode = slashMode || get().agentMode;

      // runTurn synchronously creates its own pending bubble before awaiting.
      if (preparationId) set(s => ({ preparingSends: s.preparingSends.filter(pending => pending.id !== preparationId) }));
      await runTurn(sessionId, threadId, text, attachments, display, {
        agentMode: mode,
      });
      } finally {
        if (preparationId) set(s => ({ preparingSends: s.preparingSends.filter(pending => pending.id !== preparationId) }));
      }
    },

    cancelPreparingSend: () => {
      const pending = get().preparingSends.find(item => item.sessionId === get().sessionId);
      if (!pending) return;
      if (pending.sessionId) {
        uploadControllers.get(pending.sessionId)?.abort();
        updateRun(pending.sessionId, run => !run.abort ? { ...run, sending: false,
          pendingFiles: pending.sessionId === get().sessionId ? get().pendingFiles : run.pendingFiles } : run);
      }
      set(s => ({ preparingSends: s.preparingSends.filter(item => item.id !== pending.id),
        attachmentUpload: s.attachmentUpload?.status === "uploading" && s.attachmentUpload.attemptId === pending.id ? null : s.attachmentUpload,
        ...(s.draft ? {} : { draft: pending.text }) }));
    },

    stopGeneration: async () => {
      const { sessionId, threadId, abort } = get();
      if (sessionId && uploadControllers.has(sessionId)) {
        uploadControllers.get(sessionId)!.abort();
        return;
      }
      if (sessionId) {
        reattachCancellers.get(sessionId)?.({ cancelled: true });
        reattachCancellers.delete(sessionId);
        reattachTurns.delete(sessionId);
        peerMonitorCancellers.get(sessionId)?.();
        peerMonitorCancellers.delete(sessionId);
      }
      abort?.abort();
      if (sessionId) {
        const stoppedRun = get().runs[sessionId];
        const requestId = stoppedRun?.clientRequestId;
        if (requestId) markDelivery(requestId, "cancelled");
        // Mark cancelled before awaiting the backend so visibility/online
        // recovery cannot revive a user-requested Stop mid-request.
        updateRun(sessionId, run => {
          const stopped = cancelInFlightRun(run);
          return { ...stopped, statusLabel: "Stop requested · awaiting backend",
            messages: stopped.messages.map((message, index) => run.messages[index]?.streaming
              ? { ...message, statusLabel: "Stop requested · awaiting backend" } : message) };
        });
        try {
          await api("/api/chat/cancel", {
            method: "POST",
            signal: AbortSignal.timeout(8000),
            body: JSON.stringify({
              session_id: sessionId,
              thread_id: threadId,
              client_request_id: requestId,
              turn_id: stoppedRun?.transport?.turnId || undefined,
            }),
          });
          if (get().runs[sessionId]?.clientRequestId !== requestId) return;
          updateRun(sessionId, run => ({ ...run, statusLabel: "Stop requested · check backend status",
            messages: run.messages.map(message => message.statusLabel === "Stop requested · awaiting backend"
              ? { ...message, statusLabel: "Stop requested · check backend status" } : message) }));
        } catch {
          if (get().runs[sessionId]?.clientRequestId !== requestId) return;
          updateRun(sessionId, r => ({ ...r, statusLabel: "Stop requested · confirmation unavailable" }));
          get().showToast("Stop could not be confirmed by the server. Check the task before starting more work.");
        }
      } else {
        set({
          sending: false,
          abort: null,
          runStatus: "cancelled",
          statusLabel: "Cancelled",
        });
      }
    },

    deleteTurn: async (messageIndex: number) => {
      const { sessionId } = get();
      if (!sessionId) return;
      const message = get().runs[sessionId]?.messages[messageIndex];
      if (!message) return;
      try {
        const result = await api<{ ok: boolean; error?: string }>(`/api/sessions/${encodeURIComponent(sessionId)}/truncate`, {
          method: "POST",
          body: JSON.stringify({
            message_index: messageIndex,
            history_index: message.historyIndex,
            turn_id: message.turnId,
            role: message.role,
            message_text: message.role === "user" && message.historyIndex !== undefined ? message.text : undefined,
          }),
        });
        if (!result.ok) throw new Error(result.error || "Message was not deleted");
        // updateRun syncs top-level state only if this session is still
        // active — never truncate the wrong chat after a mid-await switch.
        updateRun(sessionId, (r) => ({
          ...r,
          messages: r.messages.slice(0, messageIndex),
        }));
        void get().refreshSessions();
        get().showToast("Turn deleted");
      } catch (err) {
        get().showToast(`Delete failed: ${err instanceof Error ? err.message : err}`);
      }
    },

    deleteMemory: async (id: string, content?: string) => {
      const { sessionId } = get();
      try {
        await api("/api/memory/delete", {
          method: "POST",
          body: JSON.stringify({
            id,
            content: content || "",
            session_id: sessionId || "",
          }),
        });
        get().showToast("Memory forgotten");
      } catch (err) {
        get().showToast(`Forget failed: ${err instanceof Error ? err.message : err}`);
      }
    },

    setCanvasOpen: (open) => set({ canvasOpen: open }),
    setCanvasExpanded: (expanded) => set({ canvasExpanded: expanded }),
    setComputerPanelMode: (mode) => set({ computerPanelMode: mode }),
    setBrowserPanelMode: (mode) => set({ browserPanelMode: mode }),
    setWorkspaceSurface: (surface) => set({ workspaceSurface: surface }),
    setLiveSurface: (surface) => set((state) => ({
      liveSurface: surface,
      liveSplit: surface === "computer" ? false : state.liveSplit,
      workspaceSurface: "live",
      browserPanelMode:
        surface === "browser" && state.browserPanelMode === "default"
          ? "expanded"
          : state.browserPanelMode,
      computerPanelMode:
        surface === "computer" && state.computerPanelMode === "default"
          ? "expanded"
          : state.computerPanelMode,
      // Agent/session restore must not cover chat on phone; header tabs still openPane().
      prefs: isNarrowViewport()
        ? state.prefs
        : { ...state.prefs, rightPanelCollapsed: false },
    })),
    pinWorkspaceSurface: (surface) => set({
      workspaceSurface: surface,
      surfacePinnedAt: Date.now(),
    }),
    pinLiveSurface: (surface) => {
      get().setLiveSurface(surface);
      set({ surfacePinnedAt: Date.now() });
    },
    revealWorkspaceSurface: (surface, opts) => {
      if (!shouldAgentFocus({
        pinnedAt: get().surfacePinnedAt,
        controlling: get().surfaceControlling,
        now: Date.now(),
        firstFrameOfTurn: Boolean(opts?.firstFrameOfTurn),
      })) return;
      set({ workspaceSurface: surface, ...(opts?.liveSurface ? { liveSurface: opts.liveSurface } : {}) });
    },
    setSurfaceControlling: (controlling) => set({ surfaceControlling: controlling }),
    setLiveSplit: (split) => set({ liveSplit: split, workspaceSurface: "live" }),
    setRemoteExecution: (state) => set({ remoteExecution: state }),
    selectCanvasItem: (path) => set({ canvasSelectedPath: path }),

    openCanvasItem: (path, opts) => {
      const sid = get().sessionId;
      if (!sid || !path) return;
      let resolved = String(path).replace(/\\/g, "/").replace(/^\/+/, "");
      // A clicked chat reference can be a bare or mis-prefixed filename (e.g.
      // `slide_01.png`, which the markdown renderer maps to artifacts/slide_01.png)
      // while the real artifact is nested deeper (artifacts/…/carousel/slide_01.png).
      // If the exact path is not a known canvas item, resolve it by basename
      // against the session's discovered artifacts so the lightbox shows the
      // actual image instead of an empty popup.
      const items = get().canvasItems;
      if (!items.some((i) => i.path === resolved && i.available !== false) &&
          items.some((i) => i.path === `workspace/${resolved}` && i.available !== false)) {
        resolved = `workspace/${resolved}`;
      }
      if (!items.some((i) => i.path === resolved)) {
        const base = (resolved.split("/").pop() || resolved).toLowerCase();
        const match = items.find(
          (i) => (i.path.split("/").pop() || "").toLowerCase() === base,
        );
        if (match) resolved = match.path;
      }
      get().upsertCanvasPaths([resolved]);
      set({
        canvasOpen: true,
        canvasPreviewRequest: { path: resolved },
        canvasSelectedPath: resolved,
        canvasExpanded: Boolean(opts?.expand),
        prefs: { ...get().prefs, rightPanelCollapsed: false },
        workspaceSurface: "artifacts",
        surfacePinnedAt: Date.now(),
      });
    },

    upsertCanvasPaths: (paths) => {
      const sid = get().sessionId;
      if (!sid || !paths.length) return;
      const hadItems = get().canvasItems.length > 0;
      set((s) => {
        const byPath = new Map(s.canvasItems.map((i) => [i.path, i]));
        const turnPaths = new Set(s.canvasTurnPaths);
        let changed = false;
        for (const raw of paths) {
          const path = String(raw || "").replace(/\\/g, "/").replace(/^\/+/, "");
          if (!path || !isShowcaseArtifact(path)) continue;
          if (!turnPaths.has(path)) {
            turnPaths.add(path);
            changed = true;
          }
          const existing = byPath.get(path);
          if (existing) {
            // Streaming tool cards can report the same artifact on every
            // update. Keep the existing object and timestamp stable so the
            // artifact panel does not reorder and repaint continuously.
            continue;
          }
          const item = toCanvasItem(sid, path, { modifiedAt: Date.now(), createdAt: Date.now() });
          if (item) {
            byPath.set(path, { ...item, available: false });
            changed = true;
          }
        }
        if (!changed) return s;
        const canvasItems = Array.from(byPath.values()).sort((a, b) => {
          const recent = (b.modifiedAt || 0) - (a.modifiedAt || 0);
          if (recent) return recent;
          const [ra, pa] = showcaseSortKey(a.path);
          const [rb, pb] = showcaseSortKey(b.path);
          return ra - rb || pa.localeCompare(pb);
        });
        // Auto-open canvas when first artifacts appear.
        const shouldAutoOpen = !hadItems && canvasItems.length > 0 && !s.canvasOpen;
        // Auto-select the most recently updated deliverable (prefer webpage/markdown over config).
        const bestTurnItem = canvasItems.find(
          (i) => turnPaths.has(i.path) && (i.kind === "webpage" || i.kind === "markdown" || i.kind === "image"),
        ) || canvasItems.find((i) => turnPaths.has(i.path));
        const selectedPath =
          s.canvasSelectedPath && byPath.has(s.canvasSelectedPath)
            ? s.canvasSelectedPath
            : bestTurnItem?.path ?? canvasItems[0]?.path ?? null;
        return {
          canvasItems,
          canvasTurnPaths: turnPaths,
          canvasOpen: shouldAutoOpen ? true : s.canvasOpen,
          // Live artifact events must not move a preview the user already
          // selected. Pick a recent deliverable only for the initial selection.
          canvasSelectedPath: selectedPath,
        };
      });
    },

    refreshArtifacts: async () => {
      const sid = get().sessionId;
      if (!sid) {
        set({ canvasItems: [], canvasSelectedPath: null, canvasTurnPaths: new Set() });
        return;
      }
      try {
        const data = await api<{ artifacts?: ArtifactEntry[] }>(
          `/api/sessions/${encodeURIComponent(sid)}/artifacts`,
        );
        // The user may have switched sessions while this request was in flight.
        // Never hydrate the active canvas with artifacts from the previous thread.
        if (get().sessionId !== sid) return;
        const rows = (data.artifacts || []).map(row => ({
          ...row, path: String(row.path || "").replace(/\\/g, "/"),
        }));
        const paths = rows.map(row => row.path);
        const delivered = new Set(get().messages.flatMap(message => deliveredPaths(message, paths)));
        const items: CanvasItem[] = [];
        for (const row of rows) {
          const path = row.path;
          if (!path || (!path.startsWith("inputs/") && !isShowcaseArtifact(path) && !delivered.has(path))) continue;
          const item = toCanvasItem(sid, path, {
            kindHint: row.kind,
            url: row.url,
            size: typeof row.size === "number" ? row.size : undefined,
            name: row.name,
            modifiedAt: typeof row.mtime === "number"
              ? row.mtime * 1000
              : typeof row.modified_at === "number"
                ? row.modified_at * 1000
                : undefined,
            createdAt: typeof row.created_at === "number"
              ? row.created_at * 1000
              : typeof row.mtime === "number"
                ? row.mtime * 1000
                : undefined,
          });
          if (item) items.push(item);
        }
        items.sort((a, b) => {
          const recent = (b.modifiedAt || 0) - (a.modifiedAt || 0);
          if (recent) return recent;
          const [ra, pa] = showcaseSortKey(a.path);
          const [rb, pb] = showcaseSortKey(b.path);
          return ra - rb || pa.localeCompare(pb);
        });
        set((s) => ({
          canvasItems: items,
          canvasSelectedPath:
            s.canvasSelectedPath &&
            items.some((i) => i.path === s.canvasSelectedPath)
              ? s.canvasSelectedPath
              : items[0]?.path || null,
        }));
      } catch (err) {
        if (get().sessionId !== sid) return;
        get().showToast(
          `Artifacts: ${err instanceof Error ? err.message : err}`,
        );
      }
    },

    resolveApproval: async (approved, feedback, scope = "once") => {
      const pending = get().pendingApproval;
      if (!pending?.approval_id) return;
      const sid = pending.sessionId || pending.session_id || get().sessionId;
      const isPlan = isPlanApproval(pending);
      const isQuestion = isQuestionApproval(pending);
      const isContinue = isContinueApproval(pending);
      const note = String(feedback || "").trim();
      const grant =
        approved && !isPlan && (scope === "session" || scope === "full")
          ? scope
          : "once";

      const tracked = !isQuestion && pending.action !== "save_skill";
      if (tracked && !beginApprovalResolution(sid, pending.approval_id)) return;
      set({ pendingApproval: null });
      if (sid) {
        updateRun(sid, (r) => ({
          ...r,
          waitingApproval: false,
          pendingApproval: null,
        }));
      }
      try {
        if (isQuestion) {
          get().cancelAskHumanSend();
          const answer =
            approved && note
              ? note
              : "Skip this question and continue with the best reasonable assumption.";
          const approvalId = pending.approval_id;
          set((state) => ({
            ...patchAskHumanResolution(
              state,
              sid || state.sessionId || "",
              approvalId,
              approved && note ? note : undefined,
              !approved,
            ),
            ...(sid === state.sessionId || !sid
              ? {
                  runStatus: "running" as const,
                  statusLabel: "Answer sent · continuing…",
                }
              : {}),
          }));
          if (sid && get().sessionId !== sid) {
            await get().openSession(sid);
          }
          await get().sendMessage(answer);
          return;
        }
        if (pending.action === "save_skill") {
          if (approved) {
            try {
              await api("/api/knowledge/skill", {
                method: "POST",
                body: JSON.stringify({
                  action: "save",
                  name: pending.proposal_name,
                  content: pending.proposal_content,
                }),
              });
              // The server replays the skill through a real verification run
              // before it stays active — no client-supplied pass/fail flag.
              if (!sid || sid === get().sessionId) get().showToast("Reusable skill saved");
            } catch (err) {
              // Restore the banner so the user can retry.
              set({ pendingApproval: pending });
              if (sid) updateRun(sid, (r) => ({ ...r, waitingApproval: true, pendingApproval: pending }));
              get().showToast(`Skill save failed: ${err instanceof Error ? err.message : err}`);
            }
          } else if (!sid || sid === get().sessionId) {
            get().showToast("Skill suggestion dismissed");
          }
          return;
        }
        await api("/api/approvals", {
          method: "POST",
          body: JSON.stringify({
            approval_id: pending.approval_id,
            approved: Boolean(approved),
            feedback: note,
            scope: grant,
          }),
        });
        if (tracked) finishApprovalResolution(sid, pending.approval_id, true);
        if (approved && grant !== "once") {
          const prefs = mergePrefs(get().prefs, { defaultAskMode: false });
          savePrefs(prefs);
          applyPrefsToDocument(prefs);
          set({
            prefs,
            autoApprove: true,
            permissionScope: grant,
          });
        }
        if (!sid || sid === get().sessionId) {
          let label: string;
          if (approved) {
            if (isPlan) label = "Build · executing…";
            else if (isContinue) label = "Continuing…";
            else if (grant === "full") label = "Full access · continuing…";
            else if (grant === "session") label = "Session grant · continuing…";
            else label = "Approved · continuing…";
          } else if (note) {
            label = isPlan
              ? "Suggestion · revising plan…"
              : "Suggestion · continuing…";
          } else {
            label = isContinue ? "Stopped" : "Denied · continuing…";
          }
          set({ runStatus: "running", statusLabel: label });
        }
      } catch (err) {
        if (tracked) finishApprovalResolution(sid, pending.approval_id, false);
        const errMsg = err instanceof Error ? err.message : String(err || "");
        const isExpired =
          errMsg.toLowerCase().includes("expired") ||
          errMsg.toLowerCase().includes("unknown");
        if (isExpired) {
          set({ pendingApproval: null });
          if (sid) {
            updateRun(sid, (r) => ({
              ...r,
              waitingApproval: false,
              pendingApproval: null,
            }));
          }
          get().showToast("Approval was expired or already resolved.");
        } else {
          if (!sid || sid === get().sessionId) set({ pendingApproval: pending });
          if (sid) updateRun(sid, (r) => ({ ...r, waitingApproval: true, pendingApproval: pending }));
          get().showToast(`Approval failed: ${errMsg}`);
        }
      }
    },

    appendLocalMessage: (role, text) => {
      const body = String(text || "").trim();
      if (!body) return;
      const sid = get().sessionId;
      const msg: ChatMessage = {
        id: uid(role === "user" ? "u" : "a"),
        role,
        text: body,
      };
      if (!sid) {
        set((s) => ({ messages: [...s.messages, msg] }));
        return;
      }
      updateRun(sid, (run) => ({
        ...run,
        messages: [...run.messages, msg],
      }));
    },

    effectiveSlashCommands: () =>
      filterSlashByCapabilities(get().slashCatalog, get().capabilities),
  };
});

// Persist immediately: mobile browsers can discard a background tab without
// delivering beforeunload. Also catches draft clears after send/new chat.
useAppStore.subscribe((state, previous) => {
  if (state.draft !== previous.draft) saveComposerDraft(state.draft);
});
