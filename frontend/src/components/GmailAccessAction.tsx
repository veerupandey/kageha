import { useEffect, useState } from "react";
import { api } from "../api/client";
import type { ToolCard } from "../api/types";
import { useAppStore } from "../store";

export const gmailAccessChanged = "kageha-gmail-access-changed";

export function gmailToolLabel(outcome: ToolCard["connectorOutcome"]) {
  if (!outcome) return "";
  if (outcome.recovery) return "Gmail access needed";
  if (outcome.operation === "status") return "Access checked";
  if (outcome.state !== "ok") return "";
  return outcome.operation === "search" ? "Mail searched" :
    outcome.operation === "thread" ? "Conversation read" :
    outcome.operation === "message" ? "Message read" : "";
}

export function GmailAccessAction({ outcome }: { outcome: NonNullable<ToolCard["connectorOutcome"]> }) {
  const sessionId = useAppStore(s => s.sessionId);
  const openSettings = useAppStore(s => s.openSettings);
  const continueWithGmail = useAppStore(s => s.continueWithGmail);
  const current = useAppStore(s => s.messages.findLast(message => message.role === "assistant")?.turnId === outcome.turnId);
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!sessionId || !current || !outcome.turnId) return;
    const abort = new AbortController();
    const refresh = () => void api<{status: string}>(`/api/sessions/${encodeURIComponent(sessionId)}/gmail-access`, {signal: abort.signal})
      .then(result => { if (!abort.signal.aborted) setReady(result.status === "ready"); })
      .catch(() => { if (!abort.signal.aborted) setReady(false); });
    refresh();
    const timer = setInterval(refresh, 8000);
    window.addEventListener(gmailAccessChanged, refresh);
    return () => { abort.abort(); clearInterval(timer); window.removeEventListener(gmailAccessChanged, refresh); };
  }, [sessionId, current, outcome.turnId]);
  return <div className="ka-connector-recovery flex flex-wrap items-center gap-2 px-3 py-2 text-xs text-muted">
    <span>{ready && current ? "Gmail access is ready." : outcome.state === "agent_not_assigned"
      ? "Assign this agent to the connected mailbox." : outcome.state === "disconnected"
      ? "Connect a Gmail mailbox." : "Verify the existing Gmail connection."}</span>
    <button type="button" className="ka-icon-btn px-2" onClick={() => openSettings("integrations", {searchKey: "gmail-access"})}>Manage Gmail access</button>
    {ready && current && sessionId && outcome.turnId && <button type="button" className="ka-icon-btn px-2" disabled={busy}
      onClick={async () => {
        setBusy(true); setError("");
        try { await continueWithGmail(sessionId, outcome.turnId!); }
        catch (error) { setError(error instanceof Error ? error.message : "Could not continue."); }
        finally { setBusy(false); }
      }}>Continue with Gmail</button>}
    {error && <span role="alert">{error}</span>}
  </div>;
}
