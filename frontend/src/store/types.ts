import type {
  AgentMode,
  AgentPersona,
  AgentPersonaHistoryEntry,
  AgentTeam,
  EffectiveAgentPersona,
  ChatMessage,
  RecoverySelection,
  ComposerChip,
  MetaPayload,
  ModelInfo,
  NewSessionOptions,
  PendingApproval,
  Project,
  QueuedMessage,
  RemoteExecutionState,
  RunStatus,
  SessionRun,
  SessionSummary,
  SlashCommand,
  TodoBoard,
  ToastMessage,
  UserPrefs,
  WebUiCapabilities,
} from "../api/types";
import type { CanvasItem } from "../lib/artifactMedia";
import type { ServerTurn } from "../lib/turnTruth";
import type { SearchSetup } from "../lib/searchSetup";

export type { UserPrefs };

export interface AskHumanUndoState {
  timerId: number;
  toastId: string;
  sessionId: string;
  pending: PendingApproval;
}

export type SettingsTab = "integrations" | "channels" | "appearance" | "models" | "decisions" | "persistence" | "performance" | "search" | "media" | "extensions";

export type ComputerPanelMode = "default" | "expanded" | "popup" | "fullscreen";
export type BrowserPanelMode = "default" | "expanded" | "popup" | "fullscreen";
export type WorkspaceSurface = "live" | "monitor" | "desktops" | "artifacts" | "review";
export type LiveSurface = "computer" | "browser" | "machine";

/**
 * Adaptive-loop control decision (loop/adaptive.py ControlDecision), mirrored
 * verbatim from the backend so the chip/panel labels stay in lockstep with
 * the reasoning loop without a translation table drifting out of sync.
 */
export type TaskControlState =
  | ""
  | "continue"
  | "retry"
  | "switch_tool"
  | "replan_stage"
  | "replan_task"
  | "delegate"
  | "ask_user"
  | "repair"
  | "huddle"
  | "advance"
  | "stop_success";

export interface AppState {
  sessions: SessionSummary[];
  projects: Project[];
  selectedProjectId: string | null;
  sessionId: string | null;
  threadId: string | null;
  sessionTitle: string | null;
  sessionProject: Project | null;
  /** Active-session convenience mirror of runs[sessionId].messages */
  messages: ChatMessage[];
  runStatus: RunStatus;
  statusLabel: string;
  agentMode: AgentMode;
  /** Isolated Linux computer for this session's bash (opt-in). */
  machineActive: boolean;
  executionTargetPending?: boolean;
  /** Docker failed; bash is denied until /machine stop. */
  machineBlocked: boolean;
  machineDetail: string;
  machinePaused: boolean;
  /** Agent bash is streaming into the Live terminal log. */
  machineShellRunning: boolean;
  /** Last `$ command` shown in the collapsed terminal drawer. */
  machineShellLine: string;
  /** Sticky noVNC URL so the desktop stays up after the first connect. */
  machineNovncUrl: string;
  /** True only after this chat's VNC viewer actually connects. */
  machineScreenConnected: boolean;
  /** True when this session reused a still-running box. */
  machineAdopted: boolean;
  /** pulling image / starting XFCE / connecting screen. */
  machineBootPhase: string;
  /** In-box Chromium is up (screen can be live first). */
  machineBrowserReady: boolean;
  /** Remote (SSH) execution is active for this session (opt-in). */
  remoteActive: boolean;
  /** Saved remote-host id bound to this session (empty = default host). */
  remoteHostId: string;
  /** Friendly name of the bound remote host. */
  remoteHostName: string;
  /** Live connectivity of the bound remote host: "" | connecting | connected | blocked. */
  remoteHealth: string;
  /** Human-readable remote status/error detail. */
  remoteDetail: string;
  /** Risk tier of the bound remote host: "" | normal | production. */
  remoteRiskTier: string;
  /** Last adaptive-loop control decision (loop/adaptive.py decide_control). */
  taskControl: TaskControlState;
  /** Human-readable reason the loop chose taskControl (e.g. "3 critical defects"). */
  taskControlReason: string;
  /** Classified kind of the most recent tool/action failure, if any (loop/task_state.py FailureKind). */
  taskFailureKind: string;
  /** Suggested remedy for taskFailureKind — what the agent should do differently. */
  taskFailureChange: string;
  /** Count of failures recorded so far this turn — feeds the anti-loop badge. */
  taskFailureCount: number;
  /** Current plan stage id/status, for the detail panel. */
  taskStageId: string;
  taskStageStatus: string;
  /**
   * Tool name of the most recent guardrail halt that has ALSO halted in a
   * prior turn this session (loop/task_state.py chronic_tool_halts). Empty
   * when the most recent halt was a first-time stumble — a chronic bug
   * repeating across turns looks identical to a one-off failure unless the
   * UI distinguishes them.
   */
  taskChronicTool: string;
  /** How many prior turns that tool has halted on, including this one (>=1 when taskChronicTool is set). */
  taskChronicCount: number;
  /** Exclusive X11 lease: agent drives until You takes control. */
  desktopLeaseOwner: "" | "agent" | "user";
  autoApprove: boolean;
  /** ask | session | full — mirrors CLI /permissions */
  permissionScope: "ask" | "session" | "full";
  sending: boolean;
  draft: string;
  error: string | null;
  abort: AbortController | null;
  modelOverride: string;
  /** Selected AgentPersona id for the current/next session (Requirement 2.2). */
  activePersonaId: string;
  /** Available AgentPersona entries fetched from GET /api/agents (Default_Persona first). */
  personas: AgentPersona[];
  /** Set when the last `refreshPersonas()` call failed; cleared on the next success. */
  personasError: string | null;
  /** Agent teams fetched from GET /api/teams. */
  teams: AgentTeam[];
  /** Set when the last `refreshTeams()` call failed; cleared on the next success. */
  teamsError: string | null;
  /** Id of the currently active team, if one was selected from the sidebar. */
  activeTeamId: string | null;
  /** Shared create/edit dialog so sidebar and composer can open the same editor. */
  personaEditor: { mode: "create" } | { mode: "edit"; personaId: string } | null;
  /** Shared Settings dialog. Null when closed. */
  settingsTab: SettingsTab | null;
  /** When opening Search settings from an unready chip, highlight/focus this key card id. */
  searchKeyFocus: string | null;
  pendingFiles: File[];
  /** Local pre-admission work; never evidence that a request reached the server. */
  preparingSends: { id: string; sessionId: string | null; text: string }[];
  attachmentUpload: { sessionId: string | null; status: "uploading" | "failed"; completed: number; total: number; text: string; attemptId?: string } | null;
  pendingApproval: PendingApproval | null;
  askHumanUndo: AskHumanUndoState | null;
  composerChip: ComposerChip;
  tabs: string[];
  runs: Record<string, SessionRun>;
  /** The latest server report of each chat's turn status (design section 16). */
  serverTurns: Record<string, ServerTurn>;
  slashCatalog: SlashCommand[];
  capabilities: WebUiCapabilities;
  meta: MetaPayload | null;
  toasts: ToastMessage[];
  /** Bumped whenever a toast's "Review" action should open Agent Network. */
  peerAlertNavRequestId: number;
  /** Count of pending peer pairing requests + grant approvals awaiting review. */
  peerPendingCount: number;
  models: string[];
  /** Full catalog from GET /api/models (family, label, latest). */
  modelCatalog: ModelInfo[];
  modelCredentialsLocked: boolean;
  /** Backend-configured default model (distinct from a per-session override). */
  defaultModel: string;
  /** Active web search / vision hop from GET /api/provider-setup. */
  searchSetup: SearchSetup | null;
  bootError: string | null;
  connectionOnline: boolean;
  sessionLoading: boolean;
  sessionRefreshing: string | null;
  sessionRefreshError: { sessionId: string; message: string } | null;
  sessionOpenFailure: { sessionId: string; message: string } | null;
  sessionsError: string | null;
  /** Chat whose "Resume task" request is being submitted, so its button shows progress. */
  resumingSessionId: string | null;
  prefs: UserPrefs;

  /** Artifact canvas (images / video / pdf / office files). */
  canvasOpen: boolean;
  canvasPreviewRequest: { path: string } | null;
  canvasExpanded: boolean;
  canvasItems: CanvasItem[];
  canvasSelectedPath: string | null;
  /** Paths updated/created in the current agent turn (for highlighting). */
  canvasTurnPaths: Set<string>;

  /** Live todo/milestone board from agent progress events. */
  todoBoard: TodoBoard | null;
  /** Shared live desktop view mode used by header, activity, and panel. */
  computerPanelMode: ComputerPanelMode;
  browserPanelMode: BrowserPanelMode;
  workspaceSurface: WorkspaceSurface;
  liveSurface: LiveSurface;
  /** Timestamp of the last user-initiated surface switch. */
  surfacePinnedAt: number | null;
  /** True while the human holds a browser/computer lease. */
  surfaceControlling: boolean;
  /** First live frame of the current turn may reveal Live once. */
  liveRevealedThisTurn: boolean;
  /** Machine + host browser both live — show a split Live canvas. */
  liveSplit: boolean;
  remoteExecution: RemoteExecutionState | null;

  setDraft: (value: string) => void;
  setConnectionOnline: (online: boolean) => void;
  clearError: () => void;
  retryBoot: () => Promise<void>;
  retryLastTurn: () => Promise<void>;
  resumeLastTask: (recovery?: RecoverySelection) => Promise<void>;
  retryLastTurnWithGuidance: (guidance: string) => Promise<void>;
  setAgentMode: (mode: AgentMode) => void;
  setMachineActive: (active: boolean, options?: { upgrade?: boolean; synced?: boolean }) => Promise<void>;
  /** Activate/deactivate remote (SSH) execution for this session on a host. */
  setRemoteActive: (active: boolean, hostId?: string, hostName?: string) => Promise<void>;
  setMachinePaused: (paused: boolean) => void;
  setMachineShellRunning: (running: boolean) => void;
  setMachineShellLine: (line: string) => void;
  setMachineNovncUrl: (url: string) => void;
  setTaskStatus: (next: {
    taskControl?: TaskControlState;
    taskControlReason?: string;
    taskFailureKind?: string;
    taskFailureChange?: string;
    taskFailureCount?: number;
    taskStageId?: string;
    taskStageStatus?: string;
  }) => void;
  /** Records a chronic guardrail halt (see tool_guardrail event's chronic_halt_count). */
  setTaskChronicHalt: (tool: string, count: number) => void;
  /** Whether TaskStatusChip's anchored detail popover is open. */
  taskStatusPanelOpen: boolean;
  setTaskStatusPanelOpen: (open: boolean) => void;
  takeDesktopControl: (opts?: { heartbeat?: boolean }) => Promise<void>;
  releaseDesktopControl: () => Promise<void>;
  pauseMachine: () => Promise<void>;
  resumeMachine: () => Promise<void>;
  setAskMode: (ask: boolean) => void;
  setPermissionsMode: (mode: "ask" | "auto" | "full") => Promise<void>;
  /** Re-syncs autoApprove/permissionScope from the server's process-wide grant
   *  (e.g. on boot, after a page refresh) so the UI doesn't show a stale
   *  locally-persisted scope while the backend is on a different one. */
  syncPermissions: () => Promise<void>;
  setPrefs: (patch: Partial<UserPrefs>) => void;
  setModelOverride: (model: string) => void;
  setActivePersonaId: (id: string) => void;
  openPersonaEditor: (spec: { mode: "create" } | { mode: "edit"; personaId: string }) => void;
  closePersonaEditor: () => void;
  openSettings: (tab: SettingsTab, opts?: { searchKey?: string }) => void;
  clearSearchKeyFocus: () => void;
  closeSettings: () => void;
  refreshPersonas: () => Promise<void>;
  refreshTeams: () => Promise<void>;
  setActiveTeamId: (id: string | null) => void;
  createPersona: (payload: Partial<AgentPersona>) => Promise<AgentPersona>;
  updatePersona: (id: string, payload: Partial<AgentPersona>) => Promise<AgentPersona>;
  getPersonaEffective: (id: string) => Promise<EffectiveAgentPersona>;
  getPersonaHistory: (id: string) => Promise<AgentPersonaHistoryEntry[]>;
  rollbackPersona: (id: string, snapshotId: string) => Promise<AgentPersona>;
  deletePersona: (id: string) => Promise<void>;
  setComposerChip: (kind: ComposerChip["kind"], value: string | null) => void;
  clearComposerChip: (opts?: { resetMode?: boolean }) => void;
  showToast: (text: string, opts?: { actionLabel?: string; actionId?: string }) => void;
  /** Optional hands-free voice callback: reopen the mic for the next turn.
   * Registered by the composer while a voice conversation is active. */
  voiceRelisten?: (() => void) | null;
  setVoiceRelisten: (fn: (() => void) | null) => void;
  dismissToast: (id: string) => void;
  /** Requests AppShell open the Agent Network panel (used by a toast action). */
  requestPeerAlertNav: () => void;
  setPeerPendingCount: (count: number) => void;
  submitAskHumanAnswer: (answer: string) => void | Promise<void>;
  continueWithGmail: (sessionId: string, sourceTurn: string) => Promise<void>;
  deferAskHuman: () => Promise<void>;
  submitCredential: (value: string) => Promise<void>;
  cancelCredential: () => Promise<void>;
  cancelAskHumanSend: () => void;

  addPendingFiles: (files: FileList | File[] | null | undefined) => void;
  removePendingFile: (index: number) => void;
  clearPendingFiles: () => void;

  refreshSessions: (opts?: { limit?: number; background?: boolean }) => Promise<void>;
  prefetchSession: (sessionId: string) => void;
  loadProjects: (query?: string) => Promise<Project[]>;
  createProject: (name: string, description?: string, color?: string) => Promise<Project>;
  setSelectedProject: (projectId: string | null) => void;
  setSessionProject: (sessionId: string, projectId: string | null) => Promise<void>;
  tagTurnProject: (sessionId: string, turnIndex: number, projectId: string, tag?: string) => Promise<void>;
  probeCapabilities: () => Promise<void>;
  loadSlashCatalog: () => Promise<void>;
  loadMeta: () => Promise<void>;
  loadModels: () => Promise<void>;
  loadSearch: () => Promise<boolean>;
  setSearch: (patch: { backend?: string; vision?: string }) => Promise<void>;
  boot: () => Promise<void>;

  openSession: (sessionId: string) => Promise<void>;
  receiveJobReply: (sessionId: string, id: string, text: string, attachments: string[], speaker?: import("../api/types").AgentSpeaker) => void;
  /** Recheck active_turn and resume polling after park, tab focus, or reconnect. */
  recoverLiveProgress: (sessionId?: string, opts?: { lightweight?: boolean }) => Promise<void>;
  /** Records a server report of a chat's turn; a report that its turn ended settles a stale local run. */
  noteServerTurn: (sessionId: string, turn: ServerTurn | null) => void;
  /** Reloads the sidebar's turn statuses and the current chat's status (tab visible or focused). */
  refreshSessionStatus: () => Promise<void>;
  followRunChange: (sessionId: string, turnId: string) => void;
  /** Fresh chat in place — previous stays in sidebar history. */
  newChat: () => Promise<void>;
  /** Create session; pass `{ parallel: true }` for multitask tab. */
  newSession: (opts?: NewSessionOptions) => Promise<void>;
  closeTab: (sessionId: string) => Promise<void>;
  ensureSession: (beforeOpen?: (sessionId: string) => Promise<void>, canOpen?: () => boolean) => Promise<{ sessionId: string; threadId: string }>;
  renameSession: (title: string, sessionId?: string) => Promise<void>;
  deleteSession: (sessionId: string) => Promise<void>;
  forkSession: (sessionId: string) => Promise<void>;
  exportSession: (sessionId: string) => Promise<void>;
  importSession: (file: File) => Promise<void>;
  archiveSession: (sessionId: string, archived: boolean) => Promise<SessionSummary | undefined>;
  pinSession: (sessionId: string, pinned: boolean) => Promise<void>;

  sendMessage: (textOverride?: string) => Promise<void>;
  cancelPreparingSend: () => void;
  sendNowMessage: (textOverride?: string, queuedMessage?: QueuedMessage) => Promise<void>;
  steerMessage: (textOverride?: string, queuedMessage?: QueuedMessage) => Promise<void>;
  removeQueuedMessage: (index: number) => void;
  stopGeneration: () => Promise<void>;
  /** Delete messages from the given index onward (truncate history). */
  deleteTurn: (messageIndex: number) => Promise<void>;
  /** Forget a memory item by ID or content. */
  deleteMemory: (id: string, content?: string) => Promise<void>;
  resolveApproval: (
    approved: boolean,
    feedback?: string,
    scope?: "once" | "session" | "full",
  ) => Promise<void>;

  setCanvasOpen: (open: boolean) => void;
  setCanvasExpanded: (expanded: boolean) => void;
  setComputerPanelMode: (mode: ComputerPanelMode) => void;
  setBrowserPanelMode: (mode: BrowserPanelMode) => void;
  setWorkspaceSurface: (surface: WorkspaceSurface) => void;
  setLiveSurface: (surface: LiveSurface) => void;
  pinWorkspaceSurface: (surface: WorkspaceSurface) => void;
  pinLiveSurface: (surface: LiveSurface) => void;
  revealWorkspaceSurface: (surface: WorkspaceSurface, opts?: { firstFrameOfTurn?: boolean; liveSurface?: LiveSurface }) => void;
  setSurfaceControlling: (controlling: boolean) => void;
  setLiveSplit: (split: boolean) => void;
  setRemoteExecution: (state: RemoteExecutionState | null) => void;
  selectCanvasItem: (path: string | null) => void;
  openCanvasItem: (path: string, opts?: { expand?: boolean }) => void;
  refreshArtifacts: () => Promise<void>;
  upsertCanvasPaths: (paths: string[]) => void;

  /** Local transcript note (browser/computer slash results, system notices). */
  appendLocalMessage: (
    role: "assistant" | "system" | "user",
    text: string,
  ) => void;

  effectiveSlashCommands: () => SlashCommand[];
}
