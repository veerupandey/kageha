import { useEffect, useState } from "react";
import type { ChatMessage } from "../api/types";
import { streamSessionEvents } from "../api/stream";
import { applyStreamFrameToMessage } from "../lib/transcript";
import { normalizeToolCard } from "../store/helpers";
import { Transcript } from "./Transcript/Transcript";

/** A read-only observer: closing/reconnecting never submits or stops work. */
export function WorkerTrace({ jobId, sessionId, turnId, parentSessionId, inline = false }: { jobId: string; sessionId: string; turnId: string; parentSessionId?: string; inline?: boolean }) {
  const [open, setOpen] = useState(inline);
  const [message, setMessage] = useState<ChatMessage>({ id: jobId, role: "assistant", turnId, text: "" });
  const [error, setError] = useState("");
  const [live, setLive] = useState(false);
  useEffect(() => {
    if (!open || !turnId) return;
    const controller = new AbortController();
    let cursor = 0;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let current: ChatMessage = { id: jobId, role: "assistant", turnId, text: "" };
    setMessage(current);
    const connect = async () => {
      try {
        await streamSessionEvents(`/api/sessions/${encodeURIComponent(sessionId)}/events/stream?turn_id=${encodeURIComponent(turnId)}&after_sequence=${cursor}${parentSessionId ? `&parent_session_id=${encodeURIComponent(parentSessionId)}&job_id=${encodeURIComponent(jobId)}` : ""}`, packet => {
          if (controller.signal.aborted) return;
          setError("");
          const frames = Array.isArray(packet.events) ? packet.events : [];
          for (const frame of frames) {
            if (frame.turn_id !== turnId || (frame.payload?.job_id && frame.payload.job_id !== jobId)) continue;
            if (typeof frame.sequence === "number" && frame.sequence <= cursor) continue;
            current = applyStreamFrameToMessage(current, { ...frame, card: frame.tool_card ? normalizeToolCard(frame.tool_card) : undefined });
            cursor = Math.max(cursor, Number(frame.sequence) || 0);
          }
          setMessage(current);
          setLive(Boolean((packet.live as { active_turn?: unknown } | undefined)?.active_turn));
        }, controller.signal);
        if (!controller.signal.aborted) setLive(false);
      } catch {
        if (controller.signal.aborted) return;
        setError("Connection interrupted. Reconnecting to saved worker activity…");
        retry = setTimeout(() => void connect(), 2000);
      }
    };
    void connect();
    return () => { controller.abort(); clearTimeout(retry); };
  }, [open, jobId, sessionId, turnId, parentSessionId]);
  return <div className={inline ? "space-y-2" : "rounded-lg border border-line/50 p-2.5"}>
    {!inline && <button type="button" aria-expanded={open} className="text-xs text-accent" onClick={() => setOpen(value => !value)}>{open ? "Hide worker activity" : "Show worker activity"}</button>}
    {open && <div className="mt-2">
      {inline && !message.blocks?.some(block => block.type === "tool" || block.type === "shell") && !error && <p className="text-xs text-muted">{live ? "Waiting for the first tool call…" : "No tool calls recorded."}</p>}
      {error && <p role="status" className="text-xs text-muted">{error}</p>}
      <Transcript compactCompleted={!inline} blocks={inline ? (message.blocks || []).filter(block => block.type === "tool" || block.type === "shell") : message.blocks || []} messageId={`worker-${jobId}`} streaming={live} liveLabel={live ? "Working…" : undefined} />
    </div>}
  </div>;
}
