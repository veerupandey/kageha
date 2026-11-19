export type RunState =
  | "staged"
  | "queued"
  | "running"
  | "cancelling"
  | "cancelled"
  | "succeeded"
  | "failed"
  | "interrupted";
export interface RunSpec {
  id: string;
  session: string;
  turn: string;
  request: string;
  agent: string;
  objective: string;
  depth: number;
  parent: string | null;
  parent_run: string | null;
  input: {
    messages: Record<string, unknown>[];
    max_model_calls?: number;
    timeout_ms?: number;
    [key: string]: unknown;
  };
}
export interface RunRecord {
  spec: RunSpec;
  state: RunState;
  revision: number;
  lease: string | null;
  completion: { state: RunState; result: unknown } | null;
  cancel_requested: boolean;
}
export interface RunEvent {
  sequence: number;
  record: RunRecord;
}
export interface Authority {
  session: string;
  turn: string;
  binding: string;
  generation: number;
  resource: string;
  input_digest: string;
}
export type GrantScope = "once" | "session" | "full";
export interface ApprovalRecord {
  id: string;
  request: {
    authority: Authority;
    action: string;
    class: string;
    detail: string;
  };
  state:
    | "pending"
    | "approved"
    | "denied"
    | "consumed"
    | "cancelled"
    | "interrupted";
  revision: number;
  decision: boolean | null;
  scope: GrantScope;
  automatic: boolean;
}
export interface ApprovalDecision {
  expected_revision: number;
  authority: Authority;
  approved: boolean;
  scope: GrantScope;
}
export interface ApprovalFilter {
  session?: string;
  turn?: string;
  pending?: boolean;
  after?: number;
}
export class AgentError extends Error {
  constructor(
    message: string,
    details?: { status?: number | null; code?: string; retryable?: boolean },
  );
  status: number | null;
  code: string;
  retryable: boolean;
}
export class AgentClient {
  constructor(
    endpoint: string,
    token: string,
    options?: { timeoutMs?: number },
  );
  info(signal?: AbortSignal): Promise<Record<string, unknown>>;
  run(spec: RunSpec, signal?: AbortSignal): Promise<RunRecord>;
  status(run: string, signal?: AbortSignal): Promise<RunRecord>;
  steer(
    run: string,
    message: { id: string; body: string },
    signal?: AbortSignal,
  ): Promise<{ queued: boolean; id: string }>;
  cancel(run: string, signal?: AbortSignal): Promise<RunRecord>;
  events(
    run: string,
    options?: { after?: number; signal?: AbortSignal },
  ): AsyncGenerator<RunEvent, void, unknown>;
  approvals(
    filter?: ApprovalFilter,
    signal?: AbortSignal,
  ): Promise<{ approvals: ApprovalRecord[]; next_cursor: number | null }>;
  approval(id: string, signal?: AbortSignal): Promise<ApprovalRecord>;
  decide(
    id: string,
    decision: ApprovalDecision,
    signal?: AbortSignal,
  ): Promise<ApprovalRecord>;
}
