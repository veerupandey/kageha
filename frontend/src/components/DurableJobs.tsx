import { useCallback, useState, useSyncExternalStore } from "react";
import { createClientId } from "../lib/clientId";
import { subscribeWorkers, workerSnapshot } from "../lib/workerConnection";
import { agentKey, useAgentValue } from "../lib/agentContinuity";
import { useAppStore } from "../store";
import { Icon } from "../lib/icons";
import { WorkerTrace } from "./WorkerTrace";
import { cn } from "../lib/cn";
import { workerArtifactUrl } from "../lib/workerPresentation";

export interface ContributionSource { title: string; url?: string; artifact_path?: string; locator?: string; navigation?: { status: "linked" | "unpublished" | "unavailable"; kind?: "web" | "file"; href?: string } }

export interface AgentContribution { sources?: ContributionSource[]; finding: string; evidence: string[]; limitations: string[]; decisions: string[] }

export interface WorkerHandoff {
  preview_id: string; target: string; status: "preview" | "review_required" | "conflict" | "applying" | "applied" | "uncertain" | "rolled_back";
  review_required?: boolean;
  files: { path: string; change: string; before_text?: string | null; after_text?: string | null; truncated?: boolean; binary?: boolean }[];
  conflicts: string[];
}

export interface DurableJob {
  id: string; label: string; title?: string | null; kind: string; state: string; session_id: string;
  created_at: number; started_at: number | null; revision: number;
  can_cancel: boolean; attachable: boolean; artifacts: string[];
  error: { message?: string; code?: string; approval_id?: string };
  result: { contribution?: AgentContribution | null; message?: string; failure_detail?: string | null; job_reply_id?: string; speaker?: { id: string; name: string; avatar?: string }; chat_reply?: { attachments: string[] } };
  objective?: string;
  handoff?: WorkerHandoff | null;
  progress?: { turn_id?: string; label: string; detail?: string; steps: number; updated_at: number; phase?: string;
    activity?: string; summary?: string; tool_name?: string; surface?: string; retry_at?: number;
    current_tool?: { label?: string; id: string; name: string; status: string; started_at?: number };
    elapsed_s?: number; deadline_s?: number; last_action_at?: number; last_model_response_at?: number;
    wait_reason?: string; contribution?: AgentContribution; agent_update?: { summary: string; next_step?: string; evidence?: string[]; created_at?: number } };
  health?: { worker: "active" | "inactive" | "lease_expired"; lease_until?: number | null; observed_at: number; last_action_at?: number; last_model_response_at?: number };
  verification?: { status: "verified" | "not_verified" | "pending" };
  question?: { question: string; detail?: string; options?: string[]; yes_label?: string; no_label?: string };
  parent_session?: string;
  parent_job_id?: string | null;
  depth?: number;
  ended_at?: number | null;
  resume?: { allowed: boolean; reason?: string; requires_confirmation?: boolean };
  root_id?: string;
  spec?: { model_override?: string; effort_override?: string; context_mode?: string; keep_worktree?: boolean; continuation?: boolean; machine_active?: boolean; machine_owner_session?: string; isolation?: string; persona_id?: string; review_patch?: boolean;
    contract?: { expected_output: string; acceptance_criteria: string[]; constraints: string[]; verification: string; verification_reason: string } };
  communication?: { id: string; body: string; sender: string; created_at: number; delivered_at: number | null; acknowledged_at?: number | null; acknowledgement?: string }[];
  activity?: { id: number; label: string; created_at: number; failed?: boolean }[];
}

export function CommandPreview({ raw }: { raw: string }) {
  const [copied, setCopied] = useState(false);

  const match = raw.match(/approval required for ([a-z0-9_-]+):\s*([\s\S]*)/i) ||
                raw.match(/approval required:\s*([\s\S]*)/i);

  const toolName = match && match[2] !== undefined ? match[1] : "";
  const commandText = match && match[2] !== undefined ? match[2].trim() : match ? match[1].trim() : raw.trim();
  const isCommand = toolName.includes("command") || toolName.includes("bash") || toolName.includes("sh") || !toolName;

  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(commandText);
      }
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <div className="rounded-lg border border-amber-500/25 bg-canvas/80 overflow-hidden shadow-xs">
      <div className="flex items-center justify-between border-b border-line/50 bg-surface/70 px-2.5 py-1.5 text-[11px] text-muted">
        <div className="flex items-center gap-1.5">
          <Icon.Terminal size={12} className="text-accent shrink-0" />
          <span className="font-mono font-medium text-ink/90">{toolName || "shell"}</span>
          <span className="text-line-strong">·</span>
          <span className="text-[10px] text-muted">
            {toolName.includes("write") ? "Modifies file" : isCommand ? "Executes in workspace" : "Elevated operation"}
          </span>
        </div>
        <button
          type="button"
          onClick={handleCopy}
          className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] font-medium text-muted hover:bg-accent/10 hover:text-ink transition-colors cursor-pointer"
          title="Copy command to clipboard"
          aria-label="Copy command"
        >
          {copied ? (
            <>
              <Icon.Check size={11} className="text-emerald-400" />
              <span className="text-emerald-400">Copied</span>
            </>
          ) : (
            <>
              <Icon.Copy size={11} />
              <span>Copy</span>
            </>
          )}
        </button>
      </div>
      <pre className="max-h-28 overflow-y-auto whitespace-pre-wrap break-words p-2.5 font-mono text-xs text-muted leading-relaxed selection:bg-accent/25">
        {commandText}
      </pre>
    </div>
  );
}

export function JobQuestion({ job }: { job: DurableJob }) {
  const [savedAnswer, saveAnswer] = useAgentValue<{ text: string; messageId?: string; submittedText?: string } | null>(agentKey(job.parent_session || job.session_id, `answer:${job.id}:${job.question?.question || ""}`), null);
  const answer = typeof savedAnswer?.text === "string" ? savedAnswer.text : "";
  const setAnswer = (text: string) => saveAnswer(previous => ({ ...previous, text }));
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState(false);
  const [error, setError] = useState("");
  if (job.state !== "waiting_input" || (!job.question?.question && !job.error?.approval_id)) return null;
  const permission = Boolean(job.error?.approval_id);
  const decidePermission = async (approved: boolean, scope: "once" | "session" = "once") => {
    if (sending) return;
    setSending(true); setError("");
    try {
      const payload: { approved: boolean; approval_id?: string; scope?: string; expected_turn_id?: string } = {
        approved,
        approval_id: job.error.approval_id,
        expected_turn_id: job.progress?.turn_id,
      };
      if (scope === "session") {
        payload.scope = "session";
      }
      const response = await fetch(`/api/jobs/${job.id}/approve`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });
      if (!response.ok) throw new Error("Could not record your decision. Refresh the worker details and retry.");
      setSent(true);
    } catch (e) { setError(e instanceof Error ? e.message : "Could not record your decision"); }
    finally { setSending(false); }
  };
  const submit = async (value: string) => {
    if (!value.trim() || sending) return;
    setSending(true); setError("");
    try {
      const body = value.trim();
      const messageId = savedAnswer?.submittedText === body && typeof savedAnswer.messageId === "string" && savedAnswer.messageId ? savedAnswer.messageId : createClientId();
      saveAnswer({ text: value, messageId, submittedText: body });
      const response = await fetch(`/api/jobs/${job.id}/message`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ message: body, message_id: messageId }) });
      if (!response.ok) throw new Error("Could not send your answer. Please retry.");
      saveAnswer(current => current?.messageId === messageId && current.text.trim() === body ? null : current);
      setSent(true);
    } catch (e) { setError(e instanceof Error ? e.message : "Could not send your answer"); }
    finally { setSending(false); }
  };
  const options = job.question?.options?.length ? job.question.options : [job.question?.yes_label, job.question?.no_label].filter(Boolean) as string[];
  return <div className="mt-2.5 space-y-2.5 rounded-xl border border-amber-500/35 bg-gradient-to-b from-amber-500/[0.08] to-amber-500/[0.02] p-3 shadow-xs backdrop-blur-xs">
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div className="flex items-center gap-2 min-w-0 flex-1">
        <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-md bg-amber-500/20 text-amber-500 text-xs font-bold shadow-[0_0_8px_rgba(245,158,11,0.25)]" aria-hidden="true">?</span>
        <p className="whitespace-pre-wrap break-words text-sm font-medium leading-snug text-ink selection:bg-amber-500/25">{permission ? "Permission needed for this operation" : job.question?.question}</p>
      </div>
      {sent ? (
        <p role="status" className="inline-flex shrink-0 items-center gap-1.5 rounded-lg bg-emerald-500/10 px-3 py-1.5 text-xs font-medium text-emerald-400 border border-emerald-500/20"><span className="h-1.5 w-1.5 rounded-full bg-emerald-400 shadow-[0_0_6px_rgba(52,211,153,0.6)]" />{permission ? "Decision recorded" : "Answer sent · continuing"}</p>
      ) : permission ? (
        <div className="flex shrink-0 flex-wrap items-center gap-2">
          <button disabled={sending} onClick={() => void decidePermission(true, "once")} className="rounded-lg bg-emerald-600 px-3 py-1.5 text-xs font-semibold text-white shadow-xs hover:bg-emerald-500 active:scale-95 disabled:opacity-50 transition-all cursor-pointer">Approve once</button>
          <button disabled={sending} onClick={() => void decidePermission(true, "session")} className="rounded-lg border border-line bg-surface/70 px-3 py-1.5 text-xs font-medium text-ink hover:bg-accent/10 hover:border-accent/30 active:scale-95 disabled:opacity-50 transition-all cursor-pointer" title="Grant routine approvals for this session">Approve for session</button>
          <button disabled={sending} onClick={() => void decidePermission(false)} className="rounded-lg border border-line bg-surface/70 px-3 py-1.5 text-xs font-medium text-muted hover:bg-rose-500/10 hover:border-rose-500/30 hover:text-rose-400 active:scale-95 disabled:opacity-50 transition-all cursor-pointer">Decline</button>
        </div>
      ) : null}
    </div>
    {permission && job.question?.question && <p className="text-xs leading-relaxed text-ink">{job.question.question}</p>}
    {permission && job.error?.message && (
      <CommandPreview raw={job.error.message} />
    )}
    {!permission && <>
      {options.length > 0 && <div className="flex flex-wrap gap-2 pt-0.5">{options.map(option => <button key={option} disabled={sending} className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-xs font-medium text-amber-300 shadow-2xs hover:bg-amber-500/20 hover:border-amber-500/40 active:scale-95 disabled:opacity-50 transition-all cursor-pointer" onClick={() => void submit(option)}>{option}</button>)}</div>}
      <form className="flex gap-2 pt-0.5" onSubmit={e => { e.preventDefault(); void submit(answer); }}>
        <input aria-label={`Answer ${job.label}`} value={answer} onChange={e => setAnswer(e.target.value)} className="min-w-0 flex-1 rounded-lg border border-line/80 bg-canvas/70 px-3 py-1.5 text-sm text-ink placeholder:text-muted/60 focus:border-amber-400/80 focus:outline-hidden focus:ring-2 focus:ring-amber-400/20 transition-all" placeholder="Type your answer" disabled={sending} />
        <button className="rounded-lg bg-amber-500 px-3.5 py-1.5 text-xs font-semibold text-zinc-950 shadow-xs hover:bg-amber-400 active:scale-95 disabled:opacity-40 transition-all cursor-pointer" disabled={sending || !answer.trim()}>Send answer</button>
      </form>
    </>}
    {job.question?.detail && <details className="pt-1"><summary className="cursor-pointer text-xs font-medium text-accent hover:underline">Read worker update</summary><p className="mt-2 max-h-36 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-line/60 bg-surface/50 p-2.5 text-xs text-muted leading-relaxed">{job.question.detail}</p></details>}
    {error && <p role="alert" className="text-xs font-medium text-rose-400">{error}</p>}
  </div>;
}

export function useDurableJobs(sessionId: string | null) {
  const receiveJobReply = useAppStore(state => state.receiveJobReply);
  const subscribe = useCallback((listener: () => void) => subscribeWorkers(sessionId, listener, receiveJobReply), [sessionId, receiveJobReply]);
  const snapshot = useCallback(() => workerSnapshot(sessionId), [sessionId]);
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

export function DurableJobs({ jobs, error }: { jobs: DurableJob[]; error: string }) {
  const openSession = useAppStore(s => s.openSession);
  const parentSession = useAppStore(s => s.sessionId);
  const [message, setMessage] = useState<Record<string, string>>({});
  const [actionError, setActionError] = useState("");
  const action = async (job: DurableJob, name: string, approved?: boolean) => {
    try {
      const response = await fetch(`/api/jobs/${job.id}/${name}`, {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify(name === "message" ? { message: message[job.id] } : name === "approve" ? { approved, approval_id: job.error.approval_id } : name === "resolve" ? { retry: true } : {}),
      });
      if (!response.ok) throw new Error(`Could not ${name} this job`);
      if (name === "message") setMessage(m => ({ ...m, [job.id]: "" }));
      setActionError("");
    } catch (e) { setActionError(e instanceof Error ? e.message : "Action failed"); }
  };

  const completed = jobs.filter(j => j.state === "succeeded").length;
  const activeCount = jobs.filter(j => ["running", "waiting_input", "queued", "waiting_dependencies"].includes(j.state)).length;

  return (
    <div className="space-y-4 p-3.5">
      {/* Overview Stat Banner */}
      <div className="rounded-2xl border border-line/70 bg-gradient-to-b from-surface/90 to-surface/65 backdrop-blur-md p-3.5 shadow-2xs space-y-2.5">
        <div className="flex items-center justify-between gap-2">
          <p className="text-xs font-semibold text-ink flex items-center gap-2">
            <span className="flex h-5 w-5 items-center justify-center rounded-lg bg-accent/15 text-accent text-xs">
              <Icon.Jobs size={12} />
            </span>
            <span>Specialist Fleet</span>
          </p>
          <span className="text-xs text-muted font-medium tabular-nums">
            {completed} of {jobs.length} jobs complete
          </span>
        </div>

        <div className="grid grid-cols-3 gap-2 pt-0.5 text-center">
          <div className="rounded-xl border border-line/50 bg-canvas/40 p-2">
            <p className="text-[10px] uppercase font-semibold text-muted tracking-wider">Total</p>
            <p className="text-sm font-bold text-ink mt-0.5">{jobs.length}</p>
          </div>
          <div className="rounded-xl border border-line/50 bg-canvas/40 p-2">
            <p className="text-[10px] uppercase font-semibold text-emerald-400 tracking-wider flex items-center justify-center gap-1">
              {activeCount > 0 && <span className="h-1.5 w-1.5 rounded-full bg-emerald-400 animate-pulse" />}
              <span>Active</span>
            </p>
            <p className="text-sm font-bold text-ink mt-0.5">{activeCount}</p>
          </div>
          <div className="rounded-xl border border-line/50 bg-canvas/40 p-2">
            <p className="text-[10px] uppercase font-semibold text-muted tracking-wider">Done</p>
            <p className="text-sm font-bold text-ink mt-0.5">{completed}</p>
          </div>
        </div>

        {/* Apple Gradient Progress Track */}
        <div className="pt-0.5">
          <div className="h-1.5 w-full bg-line/60 rounded-full overflow-hidden">
            <div
              className="h-full bg-gradient-to-r from-accent via-sky-400 to-indigo-500 rounded-full transition-all duration-500"
              style={{ width: `${(completed / Math.max(jobs.length, 1)) * 100}%` }}
            />
          </div>
        </div>

        {parentSession && jobs.some(j => !["succeeded", "failed", "cancelled"].includes(j.state)) && (
          <button
            className="w-full mt-0.5 inline-flex items-center justify-center gap-1.5 rounded-full border border-rose-500/30 bg-rose-500/10 hover:bg-rose-500/20 px-3 py-1.5 text-xs font-medium text-rose-400 transition-colors cursor-pointer active:scale-[0.98]"
            onClick={() => void fetch("/api/chat/cancel", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ session_id: parentSession })
            })}
          >
            <Icon.Close size={12} />
            <span>Stop entire task</span>
          </button>
        )}
      </div>

      {(error || actionError) && (
        <p role="alert" className="rounded-xl border border-rose-500/30 bg-rose-500/10 p-3 text-xs font-medium text-rose-400">
          {error || actionError}
        </p>
      )}

      {/* Specialist Cards */}
      <div className="space-y-3">
        {jobs.map(job => {
          const isRunning = job.state === "running";
          const isWaiting = job.state === "waiting_input" || job.state === "reconciling";
          const isFailed = job.state === "failed";
          const isSucceeded = job.state === "succeeded";

          return (
            <article
              key={job.id}
              className="rounded-2xl border border-line/70 bg-gradient-to-b from-surface/90 to-surface/65 hover:border-accent/40 p-3.5 shadow-xs hover:shadow-md transition-all duration-200 space-y-3"
            >
              <div className="flex items-start justify-between gap-2.5">
                <div className="flex items-center gap-2.5 min-w-0 flex-1">
                  <div className="relative shrink-0">
                    <span
                      aria-hidden="true"
                      className="flex h-9 w-9 items-center justify-center rounded-xl bg-gradient-to-br from-accent/25 via-accent/15 to-accent/5 border border-accent/25 text-xs font-bold text-accent shadow-xs"
                    >
                      {(job.label || "A").charAt(0).toUpperCase()}
                    </span>
                    <span
                      className={cn(
                        "absolute -bottom-0.5 -right-0.5 h-2.5 w-2.5 rounded-full ring-2 ring-surface",
                        isRunning && "bg-emerald-400 shadow-[0_0_6px_rgba(52,211,153,0.7)] animate-pulse",
                        isWaiting && "bg-amber-400 shadow-[0_0_6px_rgba(251,191,36,0.7)] animate-pulse",
                        isFailed && "bg-rose-500 shadow-[0_0_6px_rgba(244,63,94,0.7)]",
                        isSucceeded && "bg-emerald-500",
                        !isRunning && !isWaiting && !isFailed && !isSucceeded && "bg-muted"
                      )}
                    />
                  </div>
                  <div className="min-w-0 flex-1">
                    <strong className="block truncate text-sm font-semibold text-ink leading-tight">
                      {job.label}
                    </strong>
                    <p className="mt-0.5 truncate text-[11px] text-muted leading-tight">
                      {job.kind} · {job.started_at ? `Started ${new Date(job.started_at * 1000).toLocaleTimeString()}` : "Queued"}
                    </p>
                  </div>
                </div>

                <span
                  className={cn(
                    "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[10px] font-medium border shrink-0 shadow-2xs",
                    isSucceeded ? "bg-emerald-500/10 text-emerald-400 border-emerald-500/20" :
                    isFailed ? "bg-rose-500/10 text-rose-400 border-rose-500/20" :
                    isWaiting ? "bg-amber-500/10 text-amber-400 border-amber-500/25" :
                    "bg-surface text-muted border-line/60"
                  )}
                >
                  <span
                    className={cn(
                      "h-1.5 w-1.5 rounded-full",
                      isRunning && "bg-emerald-400 animate-pulse",
                      isWaiting && "bg-amber-400 animate-pulse",
                      isFailed && "bg-rose-500",
                      isSucceeded && "bg-emerald-400",
                      !isRunning && !isWaiting && !isFailed && !isSucceeded && "bg-muted"
                    )}
                    aria-hidden="true"
                  />
                  <span>{job.state.replaceAll("_", " ")}</span>
                </span>
              </div>

              {job.kind === "agent" && job.spec && <p className="text-xs text-muted">
                Model: {job.spec.model_override || "Inherited routing"} · Context: {job.spec.continuation ? "Continued chat" : "Fresh brief"} · Files: {job.spec.isolation === "worktree" ? "Independent worktree" : "Shared project"}
              </p>}
              {job.progress?.turn_id && <WorkerTrace jobId={job.id} sessionId={job.session_id} turnId={job.progress.turn_id} parentSessionId={job.parent_session} />}
              {job.progress && (
                <div className="rounded-xl border border-line/50 bg-canvas/60 p-2.5 text-xs space-y-1">
                  <div className="flex items-center justify-between text-muted text-[11px]">
                    <span className="font-medium text-ink/90">{job.progress.label}</span>
                    <span className="tabular-nums text-faint">{job.progress.steps} steps</span>
                  </div>
                  {job.progress?.detail && (
                    <p className="text-muted leading-relaxed text-[11px]">{job.progress.detail}</p>
                  )}
                  {job.progress?.agent_update?.next_step && (
                    <p className="text-accent text-[11px] font-medium flex items-center gap-1">
                      <span>Next:</span>
                      <span>{job.progress.agent_update.next_step}</span>
                    </p>
                  )}
                </div>
              )}

              <JobQuestion key={job.question?.question} job={job} />

              {job.error?.message && (
                <p className="rounded-lg border border-rose-500/25 bg-rose-500/10 p-2 text-xs font-mono text-rose-400 whitespace-pre-wrap break-words">
                  {job.error.message}
                </p>
              )}

              {job.result?.message && (
                <div className="rounded-xl border border-line/50 bg-canvas/50 p-2.5 text-xs text-ink/90 leading-relaxed font-sans max-h-36 overflow-y-auto whitespace-pre-wrap break-words">
                  {job.result.message.slice(0, 1000)}
                </div>
              )}

              {job.artifacts.length > 0 && (
                <div className="flex flex-wrap gap-2 pt-0.5">
                  {job.artifacts.map((path, index) => {
                    const filename = path.split("/").pop() || "file";
                    const ext = (filename.includes(".") ? filename.split(".").pop() : "file")?.toUpperCase();
                    return (
                      <a
                        key={path}
                        href={workerArtifactUrl(job, path, index)}
                        target="_blank"
                        rel="noreferrer"
                        className="inline-flex items-center gap-2 rounded-xl border border-line/60 bg-canvas/60 hover:bg-surface hover:border-accent/40 px-2.5 py-1.5 text-xs text-ink transition-all shadow-2xs group/file active:scale-[0.98]"
                      >
                        <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-md bg-accent/15 text-accent font-mono text-[9px] font-bold" aria-hidden="true">
                          {ext}
                        </span>
                        <span className="font-mono text-xs text-accent group-hover/file:underline truncate max-w-[180px]">
                          {filename}
                        </span>
                        <Icon.Export size={10} className="text-muted group-hover/file:text-ink opacity-60 ml-auto shrink-0 transition-opacity" aria-hidden="true" />
                      </a>
                    );
                  })}
                </div>
              )}

              {job.state === "waiting_input" && job.error?.code === "approval_required" && (
                <div className="flex items-center gap-2 pt-1">
                  <button
                    onClick={() => void action(job, "approve", true)}
                    className="rounded-full bg-emerald-600 px-3.5 py-1.5 text-xs font-semibold text-white shadow-xs hover:bg-emerald-500 active:scale-95 transition-all cursor-pointer"
                  >
                    Approve once
                  </button>
                  <button
                    onClick={() => void action(job, "approve", false)}
                    className="rounded-full border border-line bg-surface/70 px-3.5 py-1.5 text-xs font-medium text-muted hover:bg-rose-500/10 hover:border-rose-500/30 hover:text-rose-400 active:scale-95 transition-all cursor-pointer"
                  >
                    Decline
                  </button>
                </div>
              )}

              {job.state === "reconciling" && (
                <button
                  className="rounded-full border border-accent/30 bg-accent/10 px-3 py-1 text-xs font-medium text-accent hover:bg-accent/20 transition-colors cursor-pointer"
                  onClick={() => void action(job, "resolve")}
                >
                  Retry explicitly (may charge again)
                </button>
              )}

              <div className="flex flex-wrap items-center gap-2 pt-1 border-t border-line/40">
                {job.attachable && (
                  <button
                    onClick={() => void openSession(job.session_id)}
                    className="inline-flex items-center gap-1.5 rounded-full border border-accent/30 bg-accent/10 hover:bg-accent/20 px-3 py-1 text-xs font-semibold text-accent transition-all active:scale-[0.97] cursor-pointer shadow-2xs"
                  >
                    <Icon.Fork size={11} />
                    <span>Open session</span>
                  </button>
                )}
                {job.can_cancel && (
                  <button
                    onClick={() => void action(job, "cancel")}
                    className="inline-flex items-center gap-1.5 rounded-full border border-line/60 bg-surface/60 hover:bg-rose-500/10 hover:border-rose-500/30 hover:text-rose-400 px-3 py-1 text-xs font-medium text-muted transition-all active:scale-[0.97] cursor-pointer shadow-2xs"
                  >
                    <Icon.Close size={11} />
                    <span>Cancel</span>
                  </button>
                )}
              </div>

              {job.kind === "agent" && ["queued", "running", "waiting_input", "waiting_dependencies"].includes(job.state) && (
                <form
                  className="flex gap-2 pt-1 border-t border-line/30"
                  onSubmit={e => { e.preventDefault(); void action(job, "message"); }}
                >
                  <input
                    aria-label={`Message ${job.label}`}
                    className="min-w-0 flex-1 rounded-full border border-line/80 bg-canvas/80 px-3.5 py-1.5 text-xs text-ink placeholder:text-muted/60 focus:border-accent focus:outline-hidden focus:ring-2 focus:ring-accent/20 transition-all"
                    value={message[job.id] || ""}
                    onChange={e => setMessage(m => ({ ...m, [job.id]: e.target.value }))}
                    placeholder="Send input or a correction"
                  />
                  <button
                    className="rounded-full bg-accent px-3.5 py-1.5 text-xs font-semibold text-white shadow-xs hover:bg-accent/90 active:scale-95 disabled:opacity-40 transition-all cursor-pointer"
                    disabled={!message[job.id]?.trim()}
                  >
                    Send
                  </button>
                </form>
              )}
            </article>
          );
        })}
      </div>
    </div>
  );
}
