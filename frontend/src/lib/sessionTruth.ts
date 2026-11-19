import { useSyncExternalStore } from "react";
import { api } from "../api/client";

export type ExecutionTarget = "local" | "machine" | "remote";
export type ExecutionDestination = {
  id?: string | null;
  label?: string | null;
  host?: string | null;
  workspace?: string | null;
  capabilities?: { shell: boolean; files: boolean; browser: boolean; desktop: boolean; jobs: boolean };
};
export type FileWorkspace = { schema_version: 1; mode: "private" | "shared" | "worktree"; root: string; source_root: string; revision: string; legacy: boolean };
export type SessionTruth = {
  schema_version: 1;
  session_id: string;
  observed_at: number;
  revision: number;
  selected_target: ExecutionTarget | null;
  selected_destination?: ExecutionDestination;
  remote_host_id?: string;
  target_conflict: boolean;
  workspace?: FileWorkspace;
  turn: null | {
    id: string; status: string; phase: string; updated_at: number;
    steps_done: number; steps_total: number; failed_calls: number;
    steps_superseded?: number;
    outcomes_pending?: number;
    actual_target: ExecutionTarget | null; actual_target_source: string | null;
    actual_destination?: ExecutionDestination;
    resource_wait?: { resource_kind: string; owner_session_id: string; started_at: number } | null;
  };
};
export type TruthSnapshot = {
  data: SessionTruth | null;
  status: "checking" | "current" | "stale" | "unavailable";
  checkedAt: number;
};
const empty: TruthSnapshot = { data: null, status: "checking", checkedAt: 0 };
const connections = new Map<string, SessionTruthConnection>();
const targets = new Set(["local", "machine", "remote", null]);

export function validateSessionTruth(value: SessionTruth, sessionId: string): void {
  if (!value || value.schema_version !== 1 || value.session_id !== sessionId ||
      !Number.isSafeInteger(value.revision) || value.revision < 0 ||
      !Number.isFinite(value.observed_at) || typeof value.target_conflict !== "boolean" ||
      !targets.has(value.selected_target) ||
      (value.workspace !== undefined && (!value.workspace || value.workspace.schema_version !== 1 ||
        !["private", "shared", "worktree"].includes(value.workspace.mode) || typeof value.workspace.root !== "string" ||
        typeof value.workspace.source_root !== "string" || typeof value.workspace.revision !== "string" || typeof value.workspace.legacy !== "boolean")) ||
      (value.turn !== null && (!value.turn || typeof value.turn.id !== "string" ||
        typeof value.turn.status !== "string" || typeof value.turn.phase !== "string" ||
        !targets.has(value.turn.actual_target) ||
        ![value.turn.updated_at, value.turn.steps_done, value.turn.steps_total, value.turn.failed_calls].every(Number.isFinite) ||
        value.turn.steps_done < 0 || value.turn.steps_total < value.turn.steps_done || value.turn.failed_calls < 0 ||
        [value.turn.steps_superseded, value.turn.outcomes_pending].some(n => n !== undefined && (!Number.isSafeInteger(n) || n < 0))))) {
    throw new Error("Incomplete backend state");
  }
}

/** Shared, bounded reads. No status is inferred from model prose or socket EOF. */
export class SessionTruthConnection {
  snapshot: TruthSnapshot = empty;
  private listeners = new Set<() => void>();
  private timer?: ReturnType<typeof setTimeout>;
  private expiry?: ReturnType<typeof setTimeout>;
  private request?: AbortController;
  private sessionId: string;
  constructor(sessionId: string) { this.sessionId = sessionId; }
  private publish(snapshot: TruthSnapshot) {
    this.snapshot = snapshot;
    this.listeners.forEach(listener => listener());
  }
  private invalidate = () => {
    this.request?.abort();
    this.request = undefined;
    clearTimeout(this.timer);
    this.publish({ ...this.snapshot, status: this.snapshot.data ? "stale" : "unavailable" });
  };
  private wake = () => {
    if (Date.now() - this.snapshot.checkedAt > 12_000) this.invalidate();
    void this.load();
  };
  private visible = () => { if (!document.hidden) this.wake(); };
  refresh() { this.invalidate(); void this.load(); }
  async load() {
    if (this.request || !this.listeners.size || document.hidden) return;
    if (navigator.onLine === false) { this.invalidate(); return; }
    clearTimeout(this.timer);
    const request = new AbortController();
    this.request = request;
    const timeout = setTimeout(() => request.abort(), 4_000);
    try {
      const data = await api<SessionTruth>(`/api/sessions/${encodeURIComponent(this.sessionId)}/state`,
        { signal: request.signal, cache: "no-store" });
      if (this.request !== request || request.signal.aborted || !this.listeners.size) return;
      validateSessionTruth(data, this.sessionId);
      if (this.snapshot.data && data.revision < this.snapshot.data.revision) throw new Error("Older backend state");
      this.publish({ data, status: "current", checkedAt: Date.now() });
      clearTimeout(this.expiry);
      this.expiry = setTimeout(() => {
        this.publish({ ...this.snapshot, status: "stale" });
      }, 12_000);
    } catch {
      if (this.request === request && this.listeners.size) {
        this.publish({ ...this.snapshot, status: this.snapshot.data ? "stale" : "unavailable" });
      }
    } finally {
      clearTimeout(timeout);
      if (this.request === request) {
        this.request = undefined;
        if (this.listeners.size && !document.hidden) this.timer = setTimeout(() => void this.load(), 3_000);
      }
    }
  }
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    if (this.listeners.size === 1) {
      window.addEventListener("online", this.wake);
      window.addEventListener("offline", this.invalidate);
      window.addEventListener("focus", this.wake);
      document.addEventListener("visibilitychange", this.visible);
      this.wake();
    }
    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size) return;
      this.request?.abort(); this.request = undefined;
      clearTimeout(this.timer); clearTimeout(this.expiry);
      window.removeEventListener("online", this.wake);
      window.removeEventListener("offline", this.invalidate);
      window.removeEventListener("focus", this.wake);
      document.removeEventListener("visibilitychange", this.visible);
      // StrictMode immediately re-subscribes the same store after cleanup.
      // Keep its shared identity through that cycle so explicit refresh still
      // reaches it and another subscriber cannot create a second poller.
      queueMicrotask(() => {
        if (!this.listeners.size && connections.get(this.sessionId) === this) connections.delete(this.sessionId);
      });
    };
  };
  getSnapshot = () => this.snapshot;
}

const noSubscribe = () => () => {};
export function refreshSessionTruth(sessionId: string) { connections.get(sessionId)?.refresh(); }
export function useSessionTruth(sessionId: string | null | undefined): TruthSnapshot {
  if (sessionId && !connections.has(sessionId)) connections.set(sessionId, new SessionTruthConnection(sessionId));
  const connection = sessionId ? connections.get(sessionId) : undefined;
  return useSyncExternalStore(connection?.subscribe || noSubscribe, connection?.getSnapshot || (() => empty));
}
