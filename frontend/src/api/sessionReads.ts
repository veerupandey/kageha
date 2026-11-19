import { api } from "./client";
import type { ChatMessage, PendingApproval, Project } from "./types";

export interface SessionDetail {
  session_id: string;
  thread_id?: string;
  title?: string | null;
  status?: string;
  messages?: Array<{ role: string; text: string; turn_id?: string; history_index?: number;
    job_reply_id?: string; message_id?: string; attachments?: string[];
    result_status?: string; resume?: ChatMessage["resume"] | null; speaker?: ChatMessage["speaker"];
    steering?: "pending" | "applied"; steering_boundary?: ChatMessage["steeringBoundary"] }>;
  pending_approval?: PendingApproval | null;
  active_turn?: { turn_id?: string; status?: string; phase?: string; speaker?: ChatMessage["speaker"] } | null;
  live_user_message?: string;
  live_message?: string;
  turns?: Array<{ id?: string; turn_id?: string; status?: string; phase?: string; objective?: string }>;
  persona_id?: string | null;
  todo_board?: Record<string, unknown> | null;
  model_override?: string | null;
  project_id?: string | null;
  project?: Project | null;
}

// Navigation, hover prefetch and recovery share a bounded read. Each consumer
// owns its cancellation; cancelling a hover must not cancel a selected thread.
const reads = new Map<string, { controller: AbortController; promise: Promise<unknown>; users: number }>();
export function readSession<T>(sessionId: string, signal?: AbortSignal, lightweight = false): Promise<T> {
  const path = `/api/sessions/${encodeURIComponent(sessionId)}${lightweight ? "?include_history=0" : ""}`;
  if (signal?.aborted) return Promise.reject(new Error("Thread loading aborted"));
  let read = reads.get(path);
  if (!read) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 20_000);
    const promise = api<T>(path, { signal: controller.signal }).finally(() => {
      clearTimeout(timer);
      if (reads.get(path)?.controller === controller) reads.delete(path);
    });
    read = { controller, promise, users: 0 };
    reads.set(path, read);
  }
  const shared = read;
  shared.users++;
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const finish = (error: unknown, value?: unknown) => {
      if (settled) return;
      settled = true;
      signal?.removeEventListener("abort", abort);
      if (--shared.users === 0) {
        if (reads.get(path) === shared) reads.delete(path);
        shared.controller.abort();
      }
      if (error) reject(error);
      else resolve(value as T);
    };
    const abort = () => finish(new Error("Thread loading aborted"));
    signal?.addEventListener("abort", abort, { once: true });
    shared.promise.then(value => finish(null, value), error => finish(error));
  });
}
