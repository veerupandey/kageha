import type { ThemeMode } from "../lib/themes";

export type AgentMode = "normal" | "plan" | "goal";

export type RunStatus =
  | "idle"
  | "running"
  | "success"
  | "error"
  | "cancelled"
  | "interrupted"
  | "waiting_approval";

export type ComposerChipKind = "mode" | "multitask";

export type SlashCommandKind =
  | "mode"
  | "multitask"
  | "prefs"
  | "browser"
  | "computer"
  | "machine"
  | "project"
  | "skill"
  | string;

export interface Project {
  id: string;
  name: string;
  description?: string;
  color?: string;
  created_at?: number;
  updated_at?: number;
  tags?: string[];
  session_count?: number;
  tagged_message_count?: number;
}

export interface SessionSummary {
  session_id: string;
  thread_id?: string;
  title?: string | null;
  status?: string;
  turn_status?: string;
  turn_phase?: string | null;
  active?: boolean;
  /** The chat's running turn (`running` or `waiting_for_unlock`), else null. */
  active_turn?: { turn_id?: string; status?: string; phase?: string } | null;
  updated_at?: string | number | null;
  objective?: string | null;
  pinned?: boolean;
  archived?: boolean;
  lifecycle_status?: "open" | "closing" | "closed" | "failed" | "interrupted";
  /** Internal worker sessions are shown in the parent task's team activity. */
  is_subagent?: boolean;
  parent_session_id?: string | null;
  agent_id?: string | null;
  /** Persona bound when this session's first turn was accepted. */
  persona_id?: string | null;
  project_id?: string | null;
  project?: Project | null;
  tagged_turns?: number[];
}

export interface BrowserActivityTarget {
  browserId: string;
  tabId: string;
}

export interface ToolCard {
  id: string;
  name: string;
  argsPreview?: string;
  status?: string;
  durationMs?: number | null;
  artifactRefs?: string[];
  resultPreview?: string;
  outputPreview?: string;
  errorPreview?: string;
  attemptId?: string;
  sideEffect?: string;
  policyGrant?: string;
  browserTarget?: BrowserActivityTarget;
  observedSource?: { url: string; title?: string };
  researchRecovery?: { recovered: true; reason?: string; attemptedBackends: string[] };
  connectorOutcome?: { connector: "gmail"; state: string; operation: string; recovery?: "manage_access"; showRecovery?: boolean; revision?: number; agents?: string[]; turnId?: string };
  /** spawn_subagents / spawn_task_graph: structured nodes (args_preview is too short to carry them). */
  subagentGraph?: SubagentGraphNode[];
}

export interface SubagentGraphNode {
  id: string;
  job_id?: string;
  task: string;
  name?: string;
  depends_on?: string[];
  /** Scheduler status once reported: done, failed, blocked, running, pending, ... */
  status?: string;
  result?: string;
  error?: string;
}

export interface ComputerFrame {
  url: string;
  path?: string;
  action?: string;
  app?: string;
  caption?: string;
}

export interface PendingApproval {
  profile_scoped?: boolean;
  approval_id: string;
  turn_id?: string;
  sessionId?: string | null;
  session_id?: string | null;
  action?: string;
  risk_class?: string;
  detail?: string | string[];
  label?: string;
  question?: string;
  yes_label?: string;
  no_label?: string;
  options?: string[];
  secret?: boolean;
  proposal_name?: string;
  proposal_content?: string;
}

/** One human-readable Activity / Trace row from the live turn stream. */
export interface ActivityStep {
  label: string;
  detail?: string[];
  kind?: string;
  interesting?: boolean;
}

/** Chronological assistant-turn block shown inline in the conversation. */
export type TranscriptBlock =
  | {
      type: "reasoning";
      id: string;
      durationMs?: number;
      text?: string;
      streaming?: boolean;
    }
  | { type: "tool"; id: string; card: ToolCard }
  | { type: "shell"; id: string; card: ToolCard }
  | { type: "narrative"; id: string; text: string }
  | { type: "progress"; id: string; text: string }
  | { type: "peer_result"; id: string; path?: string; text: string }
  | { type: "status"; id: string; label: string; detail?: string[] }
  | {
      type: "ask_human";
      id: string;
      approvalId: string;
      question: string;
      options: string[];
      status: "pending" | "answered" | "deferred";
      answer?: string;
    };

export interface AgentSpeaker { id: string; name: string; avatar?: string }

export type RecoveryMode = "recheck" | "continue";
export type RecoverySelection = { mode: RecoveryMode; outcomeIds: string[]; turnId?: string };
export type OutcomeEvidence = { id: string; label: string; reference: string; detail: string; timestamp: number };
export type OutcomeCheck = {
  id: string; label: string; status: "pass" | "fail" | "blocked" | "unresolved";
  historical?: boolean; evidence?: OutcomeEvidence[];
};

export interface ChatMessage {
  resume?: { allowed: boolean; reason: string | null };
  /** Persisted author identity; never inferred from the current composer. */
  speaker?: AgentSpeaker;
  id: string;
  role: "user" | "assistant" | "system";
  text: string;
  streaming?: boolean;
  steering?: "pending" | "applied";
  steeringId?: string;
  /** Full steering input, including uploaded attachment paths. */
  steeringText?: string;
  /** Transcript position when this correction was sent. */
  steeringBoundary?: { blockCount: number; tailTextLength: number; afterSequence?: number };
  /** Display segments retain their original message for history actions. */
  sourceMessageId?: string;
  statusLabel?: string;
  /** Optional subtitle under the live pulse (first status/event detail line). */
  statusDetail?: string;
  steps?: ActivityStep[];
  modelUsageSteps?: ActivityStep[];
  toolCards?: ToolCard[];
  computerFrames?: ComputerFrame[];
  approval?: PendingApproval | null;
  /** Ordered Hyperagent-style timeline for this assistant turn. */
  blocks?: TranscriptBlock[];
  /** Reducer cursor: assembled answer text already committed to narrative blocks. */
  transcriptCursor?: number;
  /** Raw journal cursor stays monotonic even when an answer is shortened. */
  narrativeSourceCursor?: number;
  answerRevision?: { original: string; replacement: string };
  transcriptSeq?: number;
  reasoningStartedAt?: number;
  /** Executor ownership epoch for replayable live presentation items. */
  streamGeneration?: number;
  streamCursor?: { turnId?: string; sequence: number };
  activeStreamItems?: Partial<Record<"text" | "reasoning", string>>;
  /** Turn metadata — populated on completion */
  turnSteps?: number;
  turnCostUsd?: number;
  /**
   * When a run ends in a terminal failure (model/provider unavailable,
   * watchdog kill, budget/step limit), this holds the user-facing cause so
   * the UI can render a distinct failure banner instead of a plain reply.
   */
  failureCause?: string;
  /** The redacted underlying error of an unrecognized failure (`done.failure_detail`). */
  failureDetail?: string;
  verificationOutcomes?: OutcomeCheck[];
  /** Current Escalation_Ladder step, from the turn's latest `escalation` or `completion_check` event. */
  ladderStep?: string;
  /** Raw journal status; kept across stream completion and replay. */
  resultStatus?: string;
  /** Durable journal turn id when history was loaded from `_turns`. */
  turnId?: string;
  /** Position in persisted history, before replay inserts assistant rows. */
  historyIndex?: number;
  /** Session-relative files on this bubble: user uploads (`inputs/…`) or this-turn deliverables (`artifacts/…`). */
  attachments?: string[];
}

export interface QueuedMessage {
  text: string;
  files: File[];
  needsFiles?: boolean;
  /** Restored after reload; requires an explicit send. */
  needsReview?: boolean;
}

export interface TodoBoard {
  objective?: string;
  done: number;
  total: number;
  items: { id: string; text: string; done: boolean; evidence?: string; status?: string; outcome_ids?: string[]; depends_on?: string[] }[];
  outcomes?: { id: string; text: string; done: boolean; evidence?: string; status?: string }[];
  revision?: number;
  turn_id?: string;
  current_stage_id?: string;
  verified?: number;
  outcome_total?: number;
  complete?: boolean;
  notice?: string;
  version?: number;
}

/** Compact live contract for how the current agent run is operating. */
export interface AgentOperatingState {
  mode?: AgentMode;
  loopMode?: string;
  effort?: string;
  maxTokens?: number;
  activeSkills: string[];
  nativePacks: string[];
  mcpStatus?: string;
  route?: "native" | "skill" | "mcp";
  currentTool?: string;
}

export interface SessionRun {
  clientRequestId?: string;
  sessionId: string;
  threadId: string | null;
  /** Machine routing is sticky for this session until explicitly disabled. */
  machineActive: boolean;
  /** Remote (SSH) execution is sticky for this session until disabled. */
  remoteActive: boolean;
  /** Saved remote-host id this session is bound to (empty = default host). */
  remoteHostId: string;
  /** Friendly name of the bound remote host (for header/composer display). */
  remoteHostName: string;
  messages: ChatMessage[];
  sending: boolean;
  status: RunStatus;
  statusLabel: string;
  queue: QueuedMessage[];
  abort: AbortController | null;
  waitingApproval: boolean;
  needsAttention: boolean;
  pendingFiles: File[];
  /** Live plan/todo board for this session, never shared across threads. */
  todoBoard: TodoBoard | null;
  /** Live mode/capability routing summary for the monitoring panel. */
  operatingState: AgentOperatingState;
  /** Persona used for this run; restored when switching back to a parked tab. */
  personaId?: string;
  /** Model pin for this session; restored when switching back to a parked tab
   *  (Requirement: model changes must not leak between sessions/tabs). */
  modelOverride?: string;
  /** Plan Build / tool HITL for this session, parked with the tab. */
  pendingApproval?: PendingApproval | null;
  /** Live transport diagnostics for Monitor/Trace — not shown in chat. */
  transport?: TransportState;
  /** When this tab sent the current turn. Server reports from before it do not settle it. */
  startedAt?: number;
}

export interface TransportState {
  mode: "sse" | "polling";
  lastSequence: number;
  lastFrameAt: number;
  reconnectCount: number;
  /** Journal sequences are per turn; recovery must not reuse another turn's cursor. */
  turnId?: string;
}

export interface ComposerChip {
  kind: ComposerChipKind | null;
  /** Mode id or "multitask". */
  value: string | null;
}

export type Density = "comfortable" | "compact";

export type { ThemeMode } from "../lib/themes";

export interface UserPrefs {
  density: Density;
  /** Ask before risky tools on boot / new sessions. */
  defaultAskMode: boolean;
  defaultAgentMode: AgentMode;
  reduceMotion: boolean;
  theme: ThemeMode;
  /** Auto-speak assistant replies via Gemini TTS. */
  voiceReply: boolean;
  /** Hands-free voice conversation: auto-send transcript, speak reply, re-listen. */
  voiceConversation: boolean;
  /** Optional WebUI role pins; empty values keep the configured ladders. */
  mainModel: string;
  plannerModel: string;
  workerModel: string;
  /** Explicit model thinking depth; empty = Auto (task classifier). */
  reasoningEffort: "" | "low" | "medium" | "high";
  /** Preferred image-generation backend for media tools; auto = model-capability routing. */
  mediaModel: string;
  /** Default capability choices. Session sends these and backend persists them. */
  vision: "auto" | "native" | "gemini_analyze";
  webSearch: "native" | "openai" | "anthropic" | "auto" | "ddg" | "zai" | "tavily" | "brave" | "exa" | "perplexity" | "gemini" | "codex" | "antigravity";
  imageGeneration: "openai" | "auto" | "codex" | "antigravity" | "nano-banana" | "fal" | "cloudflare";
  /** Text-to-speech provider (auto = first configured). */
  tts: string;
  /** Saved reference voice profile for cloning-capable providers. */
  ttsVoice?: string;
  /** Speech-to-text provider (auto = first configured). */
  stt: "auto" | "gemini-stt" | "openai" | "cloudflare";
  /** Persisted desktop navigation sidebar state. */
  sidebarCollapsed: boolean;
  /** Persisted desktop artifact/monitor panel state. */
  rightPanelCollapsed: boolean;
}

export interface BrowserTab {
  index: number;
  tabHandle?: string;
  url: string;
  title: string;
  active: boolean;
}

export interface BrowserAnnotation {
  id: string;
  x: number;
  y: number;
  comment: string;
  url: string;
  createdAt: number;
}

export interface BrowserState {
  inputStatus?: { ready: boolean; reason?: string | null; storage?: string; receipt_count?: number; pending_count?: number; revision?: string };
  decisionRoute?: { turn_id: string; status: string; model?: string | null;
    mode?: "off" | "shadow" | "active"; reason?: string | null; elapsed_ms?: number; verified_actions?: number;
    last_action?: string; execution?: string; score_metric?: string; cutoff?: number; batch_cutoff?: number; minimum_score?: number | null } | null;
  previewAcknowledgment?: boolean;
  previewOnly?: boolean;
  stateUnavailable?: boolean;
  selectedProfile?: string;
  availableProfiles?: string[];
  canClaimUserTabs?: boolean;
  userTabCandidates?: { tabHandle: string; url: string; title: string }[];
  privateCanary?: boolean;
  runtimeCanary?: { runtime: "python" | "rust"; canTryRust: boolean; unverifiedInput?: boolean } | null;
  runtimeRecovery?: string | null;
  controlGeneration?: number;
  layout?: { mode?: "panel" | "native"; width?: number; height?: number; pending?: boolean };
  selection?: { revision: string; backend: string; source: "user" } | null;
  browserSessions?: { id: string; backend: string; mode: "personal" | "private" | "machine" | "remote"; selected: boolean; connected: boolean; title?: string }[];
  pendingBrowserActions?: { id: string; attemptId: string; action: string; state: string; blocksCurrentTab?: boolean; canRequestReview?: boolean }[];
  browserActionHold?: { blocksCurrentTab: boolean; unknownTarget: boolean; total: number; truncated: boolean };
  loginAccess?: { status: "waiting" | "active" | "revoked" | "expired"; website?: string; purpose?: string; expiresAt?: number };
  sessionId: string;
  bindingId?: string;
  targetIdentity?: string | null;
  expectedTargetIdentity?: string | null;
  revision?: number;
  selectedBackend?: string;
  connectionStatus?: "connected" | "disconnected" | "restoring" | "target_unavailable";
  authentication?: "unverified" | "login_prompt";
  observationId?: string | null;
  observationStatus?: "recorded" | "invalidated" | "unavailable";
  documentGeneration?: number | null;
  connected: boolean;
  backend: "lightpanda" | "chromium" | "comet" | "chrome" | "cdp" | "docker" | "embedded" | string;
  url: string;
  title: string;
  tabs: BrowserTab[];
  lockedBy: "agent" | "user" | null;
  canControl: boolean;
  frameVersion?: number;
  frame?: { path?: string; url?: string; version?: number; bindingId?: string };
  viewport?: { width: number; height: number };
  result?: string;
  error?: string;
  annotations?: BrowserAnnotation[];
  owned?: boolean;
  agentLocked?: boolean;
  highlight?: { target?: string; x: number; y: number; width: number; height: number } | null;
  novncUrl?: string;
  loginRequired?: boolean;
  loginReason?: string;
  recommendedBackend?: string;
  screencast?: {
    available?: boolean;
    streaming?: boolean;
    subscribers?: number;
    deviceWidth?: number;
    deviceHeight?: number;
    pageScaleFactor?: number;
    offsetTop?: number;
  };
  decision?: {
    backend: string;
    reason: string;
    privateProfile: boolean;
    requiresConsent: boolean;
  };
}

export interface ComputerState {
  backgroundTargetInUse?: boolean;
  serviceEpoch?: string;
  interruptionReason?: string | null;
  runtime?: string;
  sessionId: string;
  provider?: "local_mac" | "docker_desktop" | string;
  available: boolean;
  connected: boolean;
  app?: string;
  bundleId?: string;
  lastAction?: string;
  agentBound?: boolean;
  lockedBy?: "user" | "agent" | null;
  canControl?: boolean;
  previewAvailable?: boolean;
  inputAuthorized?: boolean;
  frameVersion?: number;
  frame?: { path?: string; thumb_path?: string; action?: string; width?: number; height?: number };
  error?: string;
}

export interface RemoteExecutionState {
  peerId: string;
  peerName?: string;
  taskId: string;
  capabilityId: string;
  surface: "browser" | "computer" | "worker";
  status: "requested" | "running" | "completed" | "failed" | "canceled";
}

export interface ArtifactEntry {
  path: string;
  name?: string;
  size?: number;
  kind?: string;
  modified_at?: string | number | null;
  mtime?: number | null;
  created_at?: number | null;
  url?: string;
  [key: string]: unknown;
}

export interface MemoryItem {
  id?: string;
  kind?: string;
  state?: string;
  content?: string;
  summary?: string;
  task?: string;
  session_id?: string;
  provenance?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface MemorySearchResult {
  items: MemoryItem[];
  kinds?: string[];
  trace_id?: string | null;
  context?: string;
}

export interface KnowledgeBaseSummary {
  id: string;
  engine?: string;
  chunking?: "hierarchical" | "fixed" | string;
  sources?: string[];
  stats?: { chunks?: number; sources?: number; embedding?: Record<string, unknown> };
}

export interface JobSummary {
  id: string;
  status?: string;
  objective?: string;
  can_cancel?: boolean;
  attachable?: boolean;
  session_id?: string | null;
  run_id?: string | null;
  [key: string]: unknown;
}

export interface JobsCounts {
  queued?: number;
  running?: number;
  done?: number;
  [key: string]: number | undefined;
}

export interface BonAttempt {
  index: number;
  label?: string;
  running?: boolean;
  ok?: boolean;
  status?: string;
  error?: string;
  score?: number;
  worktree?: string;
  branch?: string;
  artifacts?: string[];
  message?: string;
  [key: string]: unknown;
}

export interface BonLiveState {
  n: number;
  attempts: BonAttempt[];
  placeholders: Record<number, BonAttempt>;
  winner_index: number | null;
  objective?: string;
}

export interface ReviewFinding {
  severity?: string;
  summary?: string;
  [key: string]: unknown;
}

export interface ReviewResult {
  ok?: boolean;
  findings?: ReviewFinding[];
  diff_stat?: string;
  message?: string;
  [key: string]: unknown;
}

export interface SlashCommand {
  id: string;
  label: string;
  description: string;
  kind: SlashCommandKind;
  title?: string;
  /** Example syntax shown by autocomplete when the command expects input. */
  usage?: string;
}

export interface SlashCatalogResponse {
  ok?: boolean;
  commands?: SlashCommand[];
  capabilities?: Partial<SlashCatalogCapabilities>;
}

export interface SlashCatalogCapabilities {
  comet: boolean;
  browser: boolean;
  computer: boolean;
  models: boolean;
  permissions: boolean;
  cmd: boolean;
  memory: boolean;
  labs: boolean;
  artifacts: boolean;
}

export interface WebUiCapabilities {
  projectFiles: boolean | null;
  /** null = probe pending / unknown — treat as available for slash filtering */
  cometApi: boolean | null;
  browserApi: boolean | null;
  computerApi: boolean | null;
  machineApi: boolean | null;
  modelApi: boolean | null;
  slashCatalogApi: boolean;
}

export interface MetaPayload {
  brand?: string;
  resume_allowance_usd?: number;
  memory_kinds?: string[];
  memory_states?: string[];
  memory_scopes?: string[];
  media_exts?: string[];
  project_root?: string;
  features?: Record<string, boolean>;
  project?: Record<string, unknown> | null;
  hitl?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface ModelInfo {
  id: string;
  free?: boolean;
  pricing_note?: string;
  provider?: string;
  model?: string;
  label?: string;
  family?: string;
  ready?: boolean;
  latest?: boolean;
    vision?: boolean;
  video?: boolean;
  reasoning?: boolean;
  /** Native/grounded web search on the chat model (Codex, Gemini, OpenAI). */
  web_search?: boolean;
  capabilities?: string[];
  reasoning_efforts?: string[];
}

export interface ToastMessage {
  id: string;
  text: string;
  createdAt: number;
  actionLabel?: string;
  actionId?: string;
}

export interface ChatStreamBody {
  client_request_id?: string;
  /**
   * Sent only by the "Resume task" button: continue the latest request from its
   * checkpoint. The server refuses it (never starts a new request) when no resumable
   * checkpoint or budget remains. Typed text is never treated as a resume.
   */
  resume?: boolean;
  /** Explicit reference to the task being resumed, never a replacement objective. */
  resume_turn_id?: string;
  recovery_mode?: RecoveryMode;
  recovery_outcome_ids?: string[];
  thread_id: string;
  session_id: string;
  message: string;
  attachments?: string[];
  auto_approve?: boolean;
  auto_build?: boolean;
  agent_mode?: AgentMode;
  loop_mode?: string;
  max_steps?: number;
  model?: string;
  /** Explicit reasoning effort (low|medium|high). Omit for Auto. */
  effort?: string;
  /** Preferred image-generation backend for this turn; auto selects by capability. */
  media_model?: string;
  capability_preferences?: Record<string, string | number>;
  model_roles?: { planner?: string; executor?: string };
  knowledge_bases?: string[];
  /** Selected AgentPersona id for this turn (defaults to "main" server-side). */
  agent_id?: string;
  /** Selected team for a new conversation; the server pins its dispatch configuration. */
  team_id?: string;
  recipient_id?: string;
  machine_active?: boolean;
  execution_target?: "local" | "machine" | "remote";
  remote_host_id?: string;
}

export interface StreamHandlers {
  onStatus?: (label: string, data: Record<string, unknown>) => void;
  onDelta?: (text: string) => void;
  onMessage?: (text: string, partial?: boolean) => void;
  onEvent?: (data: Record<string, unknown>) => void;
  onToolCard?: (data: Record<string, unknown>) => void;
  onComputerFrame?: (data: Record<string, unknown>, event: string) => void;
  onDone?: (data: Record<string, unknown>) => void;
  onError?: (error: string) => void;
}

export interface NewSessionOptions {
  /** Open as a parallel multitask tab (park previous). */
  parallel?: boolean;
  /** Carry composer File attachments onto the new session. */
  keepPending?: boolean;
  /** Abort a pending transition before a newly created session is opened. */
  beforeOpen?: () => void;
}

export interface ToolPolicyOverride {
  allow?: string[];
  deny?: string[];
  risk_class_overrides?: Record<string, string>;
}

/** One model name, or a list tried in order. */
export type ModelChain = string | string[];

export interface ModelPreferences {
  planner?: ModelChain | null;
  executor?: ModelChain | null;
  reviewer?: ModelChain | null;
  fast_worker?: string | null;
  embedding?: string | null;
}

/** A custom agent definition served by `GET/POST/PUT/DELETE /api/agents`. */
export interface KnowledgeRetriever {
  kind?: "zvec" | "mcp" | "http" | string;
  server?: string | null;
  tool?: string | null;
  query_param?: string;
  url?: string | null;
  method?: string;
  headers?: Record<string, string>;
  arguments?: Record<string, unknown>;
}

export interface AgentKnowledgeConfig {
  inherit_workspace?: boolean;
  extra_kb_ids?: string[];
  retriever?: KnowledgeRetriever | null;
}

export interface AgentPersona {
  schema_version: number;
  id: string;
  name: string;
  avatar?: string | null;
  description?: string;
  system_prompt?: string;
  pinned_skills?: string[];
  skills_restricted_to_pinned?: boolean;
  tool_policy?: ToolPolicyOverride | null;
  model_preferences?: ModelPreferences | null;
  knowledge?: AgentKnowledgeConfig | null;
  created_at?: string;
  updated_at?: string;
  /** True only for the built-in Default_Persona (id "main"). */
  builtin?: boolean;
}

export interface EffectiveAgentPersona {
  agent_id: string;
  persona: AgentPersona;
  effective: {
    system_prompt?: string;
    pinned_skills: string[];
    skills_restricted_to_pinned: boolean;
    tool_policy: ToolPolicyOverride;
    model_preferences: ModelPreferences;
    memory_scope: string;
    knowledge?: AgentKnowledgeConfig;
  };
}

export interface AgentPersonaHistoryEntry {
  id: string;
  timestamp: string;
}


/** A team of agents served by GET/POST/PUT/DELETE /api/teams. */
export interface AgentTeam {
  schema_version?: number;
  id: string;
  name: string;
  description?: string;
  /** Ordered agent ids — first is the coordinator for 'coordinate' mode. */
  agents: string[];
  /** coordinate | parallel | sequence — always present from server */
  mode: string;
  created_at?: string;
  updated_at?: string;
}
