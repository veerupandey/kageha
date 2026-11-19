import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api } from "../../api/client";
import type { PendingApproval } from "../../api/types";

type SessionState = { archived?: boolean; lifecycle_status?: string; lifecycle?: { status?: string; turn_id?: string | null; error?: string | null; archive?: boolean }; pending_approval?: PendingApproval | null };

/** Explicitly end a session and handle any SessionEnd hook approval. */
export function SessionLifecycleDialog({ sessionId, title, onDismiss, onChanged }: { sessionId: string; title: string; onDismiss: () => void; onChanged: () => Promise<void> }) {
  const [state, setState] = useState<SessionState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const inFlight = useRef(false);
  const path = `/api/sessions/${encodeURIComponent(sessionId)}`;
  const refresh = useCallback(async () => {
    const value=await api<SessionState>(path);
    if (!inFlight.current) setState(value);
    return value;
  }, [path]);
  useEffect(() => {
    let stopped=false;
    void refresh().catch(e => { if (!stopped) setError(e instanceof Error ? e.message : String(e)); });
    return () => { stopped=true; };
  }, [refresh]);
  async function action(suffix: string, body: Record<string, unknown> = {}, method = "POST") {
    inFlight.current=true; setBusy(true); setError("");
    try {
      await api(path+suffix,{method,body:JSON.stringify(body)});
      inFlight.current=false;
      const value=await refresh();await onChanged();
      if (value.lifecycle_status === "closing") {
        // A state change restarts the polling effect, including after a later approval.
        setState({...value});
      }
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally {inFlight.current=false;setBusy(false);}
  }
  // Once a close starts after the initial fetch, continue polling until it resolves.
  useEffect(() => {
    if (state?.lifecycle_status !== "closing") return;
    const timer=setInterval(() => { if (!inFlight.current) void refresh().then(value => { if(value.lifecycle_status!=="closing") void onChanged(); }).catch(e => setError(e instanceof Error ? e.message : String(e))); },1000);
    return () => clearInterval(timer);
  }, [state?.lifecycle_status, refresh, onChanged]);
  async function decide(approved: boolean) {
    const approval=state?.pending_approval;if(!approval?.approval_id)return;
    inFlight.current=true;setBusy(true);setError("");
    try {
      await api("/api/approvals", {method:"POST", body:JSON.stringify({approval_id:approval.approval_id, approved, scope:"once"})});
      inFlight.current = false;
      const value = await refresh();
      if (value.lifecycle_status !== "closing") await onChanged();
    }
    catch(e){setError(e instanceof Error ? e.message : String(e));}
    finally {inFlight.current=false;setBusy(false);}
  }
  const status=state?.lifecycle_status || "open";
  const pending=state?.pending_approval?.turn_id && state.pending_approval.turn_id===state.lifecycle?.turn_id ? state.pending_approval : null;
  return createPortal(<div className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/50 p-4" role="presentation">
    <section role="dialog" aria-modal="true" aria-label={`Session lifecycle for ${title}`} className="w-full max-w-lg space-y-3 rounded-2xl border border-line bg-surface p-5 text-sm text-ink">
      <h2 className="font-semibold">Session lifecycle · {title}</h2>
      <p className="text-muted">Closing runs enabled SessionEnd hooks and pauses new turns until the session is reopened. Archiving remains separate.</p>
      {error && <p role="alert">{error}</p>}
      {!state ? <p role="status">Loading session…</p> : <p role="status">Status: {status}{state.lifecycle?.error ? ` · ${state.lifecycle.error}` : ""}</p>}
      {status === "closing" && pending && <div className="space-y-2 rounded-lg border border-line p-3"><p className="font-medium">Approval required: {pending.label || pending.action || "SessionEnd hook"}</p><p className="whitespace-pre-wrap break-words">{Array.isArray(pending.detail) ? pending.detail.join("\n") : pending.detail}</p><div className="flex gap-3"><button disabled={busy} onClick={() => void decide(true)} className="text-accent underline">Approve once</button><button disabled={busy} onClick={() => void decide(false)} className="underline">Decline</button></div></div>}
      <div className="flex flex-wrap gap-3">
        {state && status === "open" && <button disabled={busy} onClick={() => void action("/close")} className="text-accent underline">Close session</button>}
        {status === "closing" && <button disabled={busy} onClick={() => void action("/close/cancel")} className="underline">Cancel close</button>}
        {["failed","interrupted"].includes(status) && <button disabled={busy} onClick={() => void (state?.lifecycle?.archive ? action("",{archived:true},"PATCH") : action("/close"))} className="text-accent underline">Resume {state?.lifecycle?.archive ? "archive" : "close"}</button>}
        {["closed","failed","interrupted"].includes(status) && <button disabled={busy} onClick={() => void (state?.archived ? action("",{archived:false},"PATCH") : action("/reopen"))} className="text-accent underline">Reopen session</button>}
        <button disabled={busy} onClick={onDismiss}>Done</button>
      </div>
    </section>
  </div>,document.body);
}
