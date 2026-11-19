export interface MemoryAccess {
  namespace: string;
  actor: string;
}
export interface MemoryRecord {
  id: string;
  namespace: string;
  owner: string;
  kind: string;
  body: string;
  metadata: Record<string, unknown>;
  acl: string[];
  revision: number;
  created_ms: number;
  updated_ms: number;
  provenance: { actor: string; origin: string; source_id: string } | null;
  tombstone: boolean;
}
export interface PutMemory {
  request_id: string;
  access: MemoryAccess;
  id: string;
  kind: string;
  body: string;
  metadata?: Record<string, unknown>;
  acl?: string[];
  expected_revision?: number | null;
}
export interface Descriptor {
  protocol_version: number;
  instance_id: string;
  component: string;
  adapter: string;
  capabilities: string[];
}
export type MemoryCall =
  | { operation: "put"; request: PutMemory }
  | { operation: "get"; access: MemoryAccess; id: string }
  | {
      operation: "delete";
      request: {
        request_id: string;
        access: MemoryAccess;
        id: string;
        expected_revision?: number | null;
      };
    }
  | { operation: "search"; access: MemoryAccess; query: string; limit: number }
  | {
      operation: "list";
      access: MemoryAccess;
      after: string | null;
      limit: number;
    };
export class MemoryClient {
  constructor(endpoint: string, token: string, writer?: MemoryFence | null);
  withWriter(fence: MemoryFence): MemoryClient;
  writerStatus(signal?: AbortSignal): Promise<MemoryFence | null>;
  claimWriter(
    owner: string,
    expectedEpoch: number,
    requestId: string,
    signal?: AbortSignal,
  ): Promise<MemoryFence>;
  retireWriter(fence: MemoryFence, signal?: AbortSignal): Promise<null>;
  info(signal?: AbortSignal): Promise<Descriptor>;
  call(
    call: Extract<MemoryCall, { operation: "put" | "delete" }>,
    signal?: AbortSignal,
  ): Promise<MemoryRecord>;
  call(
    call: Extract<MemoryCall, { operation: "get" }>,
    signal?: AbortSignal,
  ): Promise<MemoryRecord | null>;
  call(
    call: Extract<MemoryCall, { operation: "search" }>,
    signal?: AbortSignal,
  ): Promise<MemoryRecord[]>;
  call(
    call: Extract<MemoryCall, { operation: "list" }>,
    signal?: AbortSignal,
  ): Promise<{ records: MemoryRecord[]; next: string | null }>;
}
export interface MemoryFence {
  owner: string;
  epoch: number;
  retired: boolean;
  claim_id: string;
}

export * from "./agent.js";
