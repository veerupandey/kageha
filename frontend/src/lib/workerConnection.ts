import type { DurableJob } from "../components/DurableJobs";
import { subscribeSessionEvents } from "./sessionEvents";
import * as legacy from "./legacyWorkerConnection";

type Snapshot = { jobs: DurableJob[]; error: string; loaded: boolean };
type ReplyReceiver = (
  sessionId: string,
  replyId: string,
  message: string,
  attachments: string[],
  speaker?: { id: string; name: string; avatar?: string },
) => void;
const empty: Snapshot = { jobs: [], error: "", loaded: false };
const connections = new Map<string, WorkerConnection>();

/** Worker state is a projection of the same journal stream used by chat tools. */
class WorkerConnection {
  snapshot = empty;
  private listeners = new Map<() => void, ReplyReceiver>();
  private delivered = new Set<string>();
  private snapshotFloor = 0;
  private sequence = new Map<string, number>();
  private stopStream?: () => void;
  private stopLegacy?: () => void;
  readonly session: string;
  constructor(session: string) {
    this.session = session;
  }
  private publish(snapshot: Snapshot) {
    this.snapshot = snapshot;
    for (const listener of this.listeners.keys()) listener();
    for (const job of snapshot.jobs) {
      if (!this.listeners.size) break;
      const id = job.result?.job_reply_id;
      const receive = this.listeners.values().next().value;
      if (!id || !job.result?.message || !receive || this.delivered.has(id))
        continue;
      try {
        receive(
          this.session,
          id,
          job.result.message,
          job.result.chat_reply?.attachments || job.artifacts,
          job.result.speaker,
        );
        this.delivered.add(id);
      } catch {
        /* Retry only this local delivery on a later projection. */
      }
    }
  }
  private normalize(job: DurableJob): DurableJob {
    return job.state === "waiting_input" && !job.question?.question
      ? {
          ...job,
          question: {
            question: "The worker needs your guidance to continue.",
            detail: job.result?.message,
          },
        }
      : job;
  }
  private merge(jobs: DurableJob[], replace: boolean) {
    const previous = new Map(this.snapshot.jobs.map((job) => [job.id, job]));
    const merged = replace ? new Map<string, DurableJob>() : new Map(previous);
    for (const job of jobs) {
      if (!job?.id || !job.state) continue;
      const prior = previous.get(job.id);
      merged.set(
        job.id,
        prior && Number(prior.revision || 0) > Number(job.revision || 0)
          ? prior
          : this.normalize(job),
      );
    }
    this.publish({ jobs: [...merged.values()], loaded: true, error: "" });
  }
  private start() {
    this.stopStream = subscribeSessionEvents(this.session, {
      onSnapshot: (snapshot) => {
        this.snapshotFloor = Math.max(this.snapshotFloor, snapshot.sequence);
        this.merge(snapshot.workers, true);
      },
      onEvent: (event) => {
        if (
          event.kind !== "worker" ||
          !event.job ||
          typeof event.job !== "object"
        )
          return;
        const job = event.job as DurableJob;
        const sequence = Number(event.sequence || 0);
        if (
          sequence <= this.snapshotFloor ||
          sequence <= (this.sequence.get(job.id) || 0)
        )
          return;
        this.sequence.set(job.id, sequence);
        this.merge([job], false);
      },
      onError: (error) => this.publish({ ...this.snapshot, error }),
      onUnsupported: () => {
        if (this.stopLegacy || !this.listeners.size) return;
        this.stopLegacy = legacy.subscribeWorkers(
          this.session,
          () => this.publish(legacy.workerSnapshot(this.session)),
          (session, id, message, attachments, speaker) => {
            const receive = this.listeners.values().next().value;
            if (receive && !this.delivered.has(id)) {
              receive(session, id, message, attachments, speaker);
              this.delivered.add(id);
            }
          },
        );
      },
    });
  }
  refresh(ids: string[]) {
    if (this.stopLegacy) legacy.refreshWorkers(this.session, ids);
  }
  subscribe(listener: () => void, receive: ReplyReceiver) {
    this.listeners.set(listener, receive);
    if (this.listeners.size === 1) this.start();
    let stopped = false;
    return () => {
      if (stopped) return;
      stopped = true;
      this.listeners.delete(listener);
      if (this.listeners.size) return;
      this.stopStream?.();
      this.stopLegacy?.();
      if (connections.get(this.session) === this)
        connections.delete(this.session);
    };
  }
}
export function workerSnapshot(sessionId: string | null): Snapshot {
  return sessionId ? connections.get(sessionId)?.snapshot || empty : empty;
}
export function refreshWorkers(sessionId: string, ids: string[]) {
  connections.get(sessionId)?.refresh(ids);
}
export function subscribeWorkers(
  sessionId: string | null,
  listener: () => void,
  receive: ReplyReceiver,
) {
  if (!sessionId) return () => {};
  let connection = connections.get(sessionId);
  if (!connection) {
    connection = new WorkerConnection(sessionId);
    connections.set(sessionId, connection);
  }
  return connection.subscribe(listener, receive);
}
