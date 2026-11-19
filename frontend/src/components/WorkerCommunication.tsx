import { useEffect, useRef, useState } from "react";
import { createClientId } from "../lib/clientId";
import { agentKey, useAgentValue } from "../lib/agentContinuity";
import { AgentDisclosure } from "./AgentDisclosure";
import type { DurableJob } from "./DurableJobs";

const NO_MESSAGES: NonNullable<DurableJob["communication"]> = [];

export function WorkerCommunication({ job, variant = "all" }: { job: DurableJob; variant?: "all" | "compose" | "history" }) {
  const scope = job.parent_session || job.session_id;
  const draftKey = agentKey(scope, `draft:${job.id}`);
  const [savedDraft, saveDraft] = useAgentValue<{ text: string; messageId?: string; submittedText?: string } | null>(draftKey, null);
  const draft = typeof savedDraft?.text === "string" ? savedDraft.text : "";
  const [composeEntry, setComposeEntry] = useState({ key: draftKey, open: Boolean(draft) });
  const composing = composeEntry.key === draftKey ? composeEntry.open : Boolean(draft);
  const setComposing = (open: boolean) => setComposeEntry({ key: draftKey, open });
  const setDraft = (text: string) => saveDraft(previous => ({ ...previous, text }));
  const [sending, setSending] = useState(false);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");

  const inputRef = useRef<HTMLTextAreaElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const wasComposing = useRef(false);
  useEffect(() => {
    if (composing) inputRef.current?.focus();
    else if (wasComposing.current) triggerRef.current?.focus();
    wasComposing.current = composing;
  }, [composing]);
  const messages = job.communication || NO_MESSAGES;
  useEffect(() => {
    if (variant === "history" || typeof savedDraft?.text !== "string" || !savedDraft.messageId || !messages.some(message => message.id === savedDraft.messageId && message.body === savedDraft.submittedText)) return;
    saveDraft(current => current?.messageId === savedDraft.messageId && current?.text.trim() === savedDraft.submittedText ? null : current);
  }, [messages, savedDraft, saveDraft, variant]);
  const showHistory = variant !== "compose";
  const showComposer = variant !== "history";
  const canGuide = ["queued", "running", "waiting_dependencies", "retry_wait"].includes(job.state);
  const send = async () => {
    if (!draft.trim() || sending) return;
    setSending(true); setError(""); setNotice("");
    try {
      const body = draft.trim();
      const messageId = savedDraft?.submittedText === body && typeof savedDraft.messageId === "string" && savedDraft.messageId ? savedDraft.messageId : createClientId();
      saveDraft({ text: draft, submittedText: body, messageId });
      const response = await fetch(`/api/jobs/${job.id}/message`, {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ message: body, message_id: messageId, expected_turn_id: job.progress?.turn_id }),
      });
      if (!response.ok) throw new Error("Could not send guidance. Your text is kept here; try again.");
      saveDraft(current => current?.messageId === messageId && current.text.trim() === body ? null : current); setNotice(`Message queued for ${job.label}.`); setComposing(false);
    } catch (e) { setError(e instanceof Error ? e.message : "Could not send guidance"); }
    finally { setSending(false); }
  };
  if ((!showHistory || !messages.length) && (!showComposer || (!canGuide && !draft && !notice && !error))) return null;
  return <div className="min-w-0 space-y-3 text-sm">
    {showHistory && messages.length > 0 && <AgentDisclosure scope={scope} name={`messages:${job.id}`}>
      <summary className="cursor-pointer text-xs font-medium text-accent hover:underline">Messages · {messages.length}</summary>
      <ol className="mt-2 max-h-48 space-y-2 overflow-y-auto" aria-label={`Communication with ${job.label}`}>
        {messages.map(message => <li key={message.id} className="rounded-lg border border-line/60 bg-canvas/60 p-2.5 shadow-2xs">
          <div className="flex flex-wrap items-center justify-between gap-1 text-[11px] text-muted">
            <span className="font-medium text-ink/80">{message.sender === "parent" ? `Kageha → ${job.label}` : message.sender === "user" ? `You → ${job.label}` : `Message → ${job.label}`}</span>
            <span>{message.acknowledged_at ? "Acknowledged" : message.delivered_at ? "Delivered" : ["failed", "succeeded", "cancelled"].includes(job.state) ? "Not delivered" : "Queued"} · <time dateTime={new Date(message.created_at * 1000).toISOString()}>{new Date(message.created_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></span>
          </div>
          <p className="mt-1.5 whitespace-pre-wrap break-words text-xs leading-relaxed text-ink">{message.body}</p>
          {message.acknowledgement && <p className="mt-1 text-xs text-muted">{job.label}: {message.acknowledgement}</p>}
        </li>)}
      </ol>
    </AgentDisclosure>}
    {showComposer && !canGuide && draft && <section className="space-y-2 rounded-xl border border-line/60 bg-surface/50 p-3"><p className="text-xs text-muted">This draft hasn’t been sent. You can copy it or discard it.</p><textarea aria-label={`Unsent message to ${job.label}`} readOnly value={draft} rows={3} className="w-full rounded-lg border border-line bg-canvas p-2 text-xs text-ink" /><button className="min-h-8 text-xs text-muted hover:text-ink transition-colors" onClick={() => saveDraft(null)}>Discard draft</button></section>}
    {showComposer && canGuide && !composing && <button ref={triggerRef} className="inline-flex min-h-9 max-w-full items-center gap-1.5 break-words rounded-lg border border-accent/25 bg-accent/10 px-3 py-1.5 text-xs font-semibold text-accent shadow-2xs hover:bg-accent/15 hover:border-accent/40 active:scale-95 transition-all" onClick={() => { setComposing(true); setNotice(""); }}>Message {job.label}</button>}
    {showComposer && canGuide && composing && <form className="space-y-2.5 rounded-xl border border-line/70 bg-surface/50 p-3 shadow-xs" onSubmit={e => { e.preventDefault(); void send(); }}>
      <label className="block text-xs font-semibold uppercase tracking-wider text-muted" htmlFor={`worker-guidance-${job.id}`}>Message {job.label}</label>
      <textarea ref={inputRef} id={`worker-guidance-${job.id}`} rows={2} value={draft} disabled={sending} onChange={e => setDraft(e.target.value)} placeholder="Add context or suggest a change…" className="w-full resize-y rounded-lg border border-line/80 bg-canvas/70 px-3 py-2 text-sm text-ink placeholder:text-muted/60 focus:border-accent focus:outline-hidden focus:ring-2 focus:ring-accent/20 transition-all" />
      <p className="text-[11px] text-muted/80">Your message will be delivered when the agent can receive it. It won’t interrupt an action in progress.</p>
      <div className="flex items-center gap-2 pt-0.5"><button type="submit" disabled={sending || !draft.trim()} className="rounded-lg bg-accent px-3 py-1.5 text-xs font-semibold text-surface shadow-xs hover:bg-accent/90 active:scale-95 disabled:opacity-40 transition-all">{sending ? "Sending…" : "Send message"}</button><button type="button" disabled={sending} className="rounded-lg px-2.5 py-1.5 text-xs font-medium text-muted hover:text-ink transition-colors" onClick={() => setComposing(false)}>Cancel</button></div>
    </form>}
    {showComposer && notice && <p role="status" className="text-xs font-medium text-accent">{notice}</p>}
    {showComposer && error && <p role="alert" className="text-xs font-medium text-rose-400">{error}</p>}
  </div>;
}
