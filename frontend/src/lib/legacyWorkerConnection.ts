import { api } from "../api/client";
import type { DurableJob } from "../components/DurableJobs";

type Snapshot = { jobs: DurableJob[]; error: string; loaded: boolean };
type ReplyReceiver = (sessionId: string, replyId: string, message: string, attachments: string[], speaker?: { id: string; name: string; avatar?: string }) => void;
const empty: Snapshot = { jobs: [], error: "", loaded: false };
const connections = new Map<string, WorkerConnection>();
const terminal = new Set(["succeeded", "failed", "cancelled"]);
const online = () => navigator.onLine !== false;

/** One bounded request per chat, regardless of how many panels observe it. */
class WorkerConnection {
  snapshot = empty;
  listeners = new Map<() => void, ReplyReceiver>();
  private timer?: ReturnType<typeof setTimeout>;
  private request?: AbortController;
  private failures = 0;
  private refreshPending = false;
  private delivered = new Set<string>();
  private announced = new Set<string>();
  private sessionId: string;
  constructor(sessionId: string) {
    this.sessionId = sessionId;
    window.addEventListener("online", this.wake);
    window.addEventListener("offline", this.offline);
    window.addEventListener("focus", this.wake);
    document.addEventListener("visibilitychange", this.visibility);
  }
  private visibility = () => { if (!document.hidden) this.wake(); };
  private offline = () => {
    clearTimeout(this.timer);
    this.timer = undefined;
    // Release the old read immediately. Some transports do not settle their
    // aborted promise promptly; they must not hold up the online event.
    this.request?.abort();
    this.request = undefined;
    this.publish({ ...this.snapshot, error: "You’re offline. Worker updates will resume when connected." });
  };
  private wake = () => {
    clearTimeout(this.timer);
    this.timer = undefined;
    if (!this.request) void this.load();
  };
  refresh(ids: string[]) {
    if (!ids.some(id => !this.announced.has(id))) return;
    ids.forEach(id => this.announced.add(id));
    if (this.failures && this.timer !== undefined) return;
    if (this.request) this.refreshPending = true;
    else this.wake();
  }
  private publish(value: Snapshot) {
    this.snapshot = value;
    for (const listener of this.listeners.keys()) listener();
  }
  private async load() {
    if (!this.listeners.size || !online()) return;
    const request = new AbortController();
    this.request = request;
    try {
      const data = await api<{ jobs: DurableJob[] }>(
        `/api/jobs?session_id=${encodeURIComponent(this.sessionId)}&limit=1000&view=ui`, { signal: request.signal });
      if (request.signal.aborted) return;
      // A truncated/proxy response must not masquerade as an empty task.
      if (!data || !Array.isArray(data.jobs)) throw new Error("Worker update was incomplete. Retrying…");
      const jobs = data.jobs.filter(job => Boolean(job.state)).map(job =>
        job.state === "waiting_input" && !job.question?.question
          ? { ...job, question: { question: "The worker needs your guidance to continue.", detail: job.result?.message } }
          : job);
      this.failures = 0;
      this.publish({ jobs, error: "", loaded: true });
      for (const job of jobs) {
        if (request.signal.aborted || !this.listeners.size) break;
        const receive = this.listeners.values().next().value;
        const id = job.result?.job_reply_id;
        if (receive && id && job.result.message && !this.delivered.has(id)) {
          try {
            receive(this.sessionId, id, job.result.message, job.result.chat_reply?.attachments || job.artifacts, job.result.speaker);
            this.delivered.add(id);
          } catch {
            // Keep this reply eligible for retry; one local projection failure
            // must not block other replies or be reported as a network outage.
          }
        }
      }
    } catch (error) {
      if (request.signal.aborted) return;
      this.failures += 1;
      this.publish({ ...this.snapshot, error: error instanceof Error ? error.message : "Background work unavailable" });
    } finally {
      const current = this.request === request;
      if (current) this.request = undefined;
      if (current && !request.signal.aborted && this.listeners.size && online()) {
        const active = this.snapshot.jobs.some(job => !terminal.has(job.state));
        const delay = document.hidden ? 30_000 : this.failures
          ? Math.min(30_000, 2000 * 2 ** Math.min(this.failures - 1, 4)) : active ? 2000 : 10_000;
        this.timer = setTimeout(() => {
          this.timer = undefined;
          void this.load();
        }, this.refreshPending && !this.failures ? 0 : delay);
        this.refreshPending = false;
      }
    }
  }
  subscribe(listener: () => void, receive: ReplyReceiver) {
    this.listeners.set(listener, receive);
    if (this.listeners.size === 1) this.wake();
    let subscribed = true;
    return () => {
      if (!subscribed) return;
      subscribed = false;
      this.listeners.delete(listener);
      if (this.listeners.size) return;
      this.request?.abort();
      clearTimeout(this.timer);
      window.removeEventListener("online", this.wake);
      window.removeEventListener("offline", this.offline);
      window.removeEventListener("focus", this.wake);
      document.removeEventListener("visibilitychange", this.visibility);
      connections.delete(this.sessionId);
    };
  }
}

export function workerSnapshot(sessionId: string | null): Snapshot {
  return sessionId ? connections.get(sessionId)?.snapshot || empty : empty;
}
/** Called when the chat stream records new durable work; never submits work. */
export function refreshWorkers(sessionId: string, ids: string[]) { connections.get(sessionId)?.refresh(ids); }
export function subscribeWorkers(sessionId: string | null, listener: () => void, receive: ReplyReceiver) {
  if (!sessionId) return () => {};
  let connection = connections.get(sessionId);
  if (!connection) {
    connection = new WorkerConnection(sessionId);
    connections.set(sessionId, connection);
  }
  return connection.subscribe(listener, receive);
}
