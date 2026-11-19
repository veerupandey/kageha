export interface CredentialRef {
  namespace: string;
  id: string;
}
export interface CredentialAccess {
  purpose: string;
  destination: string;
}
export type CredentialState =
  | "active"
  | "revoked"
  | "deleted"
  | "refresh_prepared"
  | "refresh_dispatching"
  | "refresh_uncertain";
export interface CredentialMetadata {
  reference: CredentialRef;
  kind: string;
  revision: number;
  authority_generation: number;
  expires_at_ms: number | null;
  state: CredentialState;
  refreshable?:boolean;
}
export interface CredentialOperation {
  operation_id: string;
  metadata: CredentialMetadata;
}
export type CredentialTransport = (
  route: string,
  body: unknown,
) => Promise<any>;
export class SecretLease {
  readonly metadata: CredentialMetadata;
  readonly validUntilMs: number;
  constructor(
    value: Uint8Array,
    metadata: CredentialMetadata,
    validUntilMs: number,
  );
  expose(): Uint8Array;
  dispose(): void;
  toJSON(): unknown;
}
export class CredentialClient {
  constructor(transport: CredentialTransport);
  metadata(
    reference: CredentialRef,
    access: CredentialAccess,
  ): Promise<CredentialMetadata>;
  resolve(
    reference: CredentialRef,
    access: CredentialAccess,
  ): Promise<SecretLease>;
  validate(lease: SecretLease, access: CredentialAccess): Promise<void>;
  put(
    reference: CredentialRef,
    access: CredentialAccess,
    write: {
      operationId: string;
      expectedRevision: number;
      kind: string;
      value: Uint8Array;
      expiresAtMs?: number | null;
    },
  ): Promise<CredentialOperation>;
  revoke(
    reference: CredentialRef,
    access: CredentialAccess,
    write: {
      operationId: string;
      expectedRevision: number;
      deleteRecord?: boolean;
    },
  ): Promise<CredentialOperation>;
  operation(
    reference: CredentialRef,
    access: CredentialAccess,
    operationId: string,
  ): Promise<CredentialOperation | null>;
}
export function unixCredentialTransport(options: {
  socket: string;
  tokenFile: string;
  timeoutMs?: number;
}): Promise<CredentialTransport>;
/** Supply a trusted Node fetch implementation configured with a workload certificate and private trust roots. */
export function httpsCredentialTransport(
  endpoint: string,
  authenticatedFetch: typeof fetch,
): CredentialTransport;
export interface RefreshTicket {
  reference: CredentialRef;
  lease_id: string;
  revision: number;
  authority_generation: number;
  expires_at_ms: number;
}
export interface ReplacementStatus {
  id: string;
  stage:
    | "requested"
    | "validated"
    | "preparing"
    | "transferring"
    | "checkpointed"
    | "committed"
    | "verified"
    | "retired"
    | "blocked"
    | "cancelled";
  source_generation: number;
  destination: string;
  blocker: string | null;
}
export type ReplacementDecision = {
  id: string;
  destination: string;
  allow_browser_transfer?: boolean;
};
export type ReplacementAction =
  | { action: "binding" }
  | { action: "request"; operation_id: string; decision: ReplacementDecision }
  | { action: "status" | "resume" | "retire" | "cancel"; operation_id: string }
  | {
      action: "rollback";
      operation_id: string;
      previous_operation_id: string;
      decision: ReplacementDecision;
    };
export interface CredentialClient {
  beginRefresh(
    reference: CredentialRef,
    access: CredentialAccess,
    expectedRevision: number,
    operationId: string,
  ): Promise<RefreshTicket>;
  dispatchRefresh(
    ticket: RefreshTicket,
    access: CredentialAccess,
    operationId: string,
  ): Promise<{ ticket: RefreshTicket; grant: SecretLease }>;
  completeRefresh(
    ticket: RefreshTicket,
    access: CredentialAccess,
    operationId: string,
    grant: Uint8Array,
    expiresAtMs?: number | null,
  ): Promise<CredentialOperation>;
  recoverRefresh(
    reference: CredentialRef,
    access: CredentialAccess,
    expectedRevision: number,
    operationId: string,
  ): Promise<CredentialOperation>;
  failRefresh(
    ticket: RefreshTicket,
    access: CredentialAccess,
    operationId: string,
  ): Promise<CredentialOperation>;
  audits(
    namespace: string,
    access: CredentialAccess,
    after?: number,
  ): Promise<CredentialAudit[]>;
  replacement(
    access: CredentialAccess,
    action: ReplacementAction,
  ): Promise<
    | ReplacementStatus
    | { backend: string; vault_id: string; generation: number }
  >;
}
export interface CredentialAudit {
  sequence: number;
  at_ms: number;
  principal: string;
  action: string;
  reference: CredentialRef | null;
  outcome: string;
}
