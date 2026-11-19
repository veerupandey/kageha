import { useState } from "react";
import { api } from "../api/client";
import type { BrowserState } from "../api/types";
import { useAppStore } from "../store";

export function SecureLoginCard({ detail }: { detail: string }) {
  const sessionId = useAppStore((s) => s.sessionId);
  const resolve = useAppStore((s) => s.resolveApproval);
  const openBrowser = useAppStore((s) => s.setBrowserPanelMode);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  let website = "Connected website";
  let purpose = "Complete the requested task";
  let reconnectRequired = false;
  let accountStep = false;
  let rustTab = "";
  let bindingId = "";
  try {
    const parsed = JSON.parse(detail);
    website = String(parsed.website || website);
    purpose = String(parsed.purpose || purpose);
    reconnectRequired = parsed.reconnect_required === true;
    accountStep = parsed.account_step === true;
    rustTab = String(parsed.rust_tab || "");
    bindingId = String(parsed.binding_id || "");
  } catch { /* Older clients may provide a plain-text detail. */ }
  async function accountTabState(path: string) {
    const state = await api<BrowserState>(`${path}/state`);
    if (state.backend !== "rust" || !state.tabs.some(tab => tab.active && tab.tabHandle === rustTab)
        || state.bindingId?.split(":").slice(0, 2).join(":") !== bindingId.split(":").slice(0, 2).join(":")) {
      throw new Error("Return to the original sign-in tab before approving access.");
    }
    if (accountStep && (!state.url || new URL(state.url).origin !== new URL(website).origin)) {
      throw new Error("Return to the account website before resuming.");
    }
    return state;
  }
  async function signIn() {
    if (!sessionId) return;
    setBusy(true);
    setError("");
    openBrowser("expanded");
    useAppStore.getState().setPrefs({ rightPanelCollapsed: false });
    useAppStore.getState().setWorkspaceSurface("live");
    useAppStore.getState().setLiveSurface("browser");
    try {
      const path = `/api/sessions/${encodeURIComponent(sessionId)}/browser`;
      const state = rustTab ? await accountTabState(path) : undefined;
      if (state?.lockedBy !== "user") {
        await api(`${path}/lease`, {
          method: "POST", body: JSON.stringify({ action: "take", bindingId: state?.bindingId }),
        });
      }
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  async function approve() {
    if (!rustTab) {
      await resolve(true, "", "once");
      return;
    }
    if (!sessionId) return;
    setBusy(true);
    setError("");
    try {
      const path = `/api/sessions/${encodeURIComponent(sessionId)}/browser`;
      const state = await accountTabState(path);
      if (state.lockedBy === "user") {
        await api(`${path}/lease`, { method: "POST", body: JSON.stringify({ action: "return",
          bindingId: state.bindingId, controlGeneration: state.controlGeneration }) });
      }
      await resolve(true, "", "once");
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  return <div className="border-t border-accent/20 bg-canvas px-4 py-4 md:px-6">
    <div className="mx-auto max-w-3xl rounded-xl border border-line p-4">
      <p className="text-sm font-semibold text-ink">{reconnectRequired ? "Approve browser reconnection" : accountStep ? "Continue in browser" : "Sign in to continue"}</p>
      <p className="mt-1 break-all text-xs text-muted">{website}</p>
      <p className="mt-2 text-sm text-ink">{purpose}</p>
      <p className="mt-2 text-xs text-muted">{reconnectRequired ? "The browser connection was lost. Approve access so the agent can reconnect for this task. If sign-in is needed afterward, complete it directly in the browser." : accountStep ? "Complete the account step in the browser, then return here to resume. Enter passwords and verification codes directly on the site." : "Sign in and complete verification directly in the browser. Browser tools are paused. After signing in, return to chat to approve access."}</p>
      <p className="mt-2 text-xs text-muted">{rustTab ? "Approval returns control to the agent on the original sign-in tab. You can take control or revoke browser access from the panel." : "Approval lets the agent read, click, type and navigate in this connected browser for this session, including reconnects and backend restarts. Revoke access from the browser panel at any time."}</p>
      <label className="mt-3 flex gap-2 text-xs text-ink"><input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} />{reconnectRequired ? "I approve reconnecting the browser and browser access for this task." : accountStep ? "I finished the account step and approve browser access for this task." : "I have finished signing in and approve browser access for this task."}</label>
      {error && <p role="alert" className="mt-2 text-xs text-rose-500">{error}</p>}
      <div className="mt-3 flex flex-wrap gap-2">
        {!reconnectRequired && <button className="rounded-lg border border-line px-3 py-2 text-sm" disabled={busy} onClick={() => void signIn()}>{accountStep ? "Open browser tab" : "Open sign-in browser"}</button>}
        <button className="rounded-lg bg-accent px-3 py-2 text-sm text-white disabled:opacity-40" disabled={!confirmed || busy} onClick={() => void approve()}>Approve access and resume</button>
        <button className="rounded-lg px-3 py-2 text-sm text-muted" onClick={() => void resolve(false)}>Cancel</button>
      </div>
    </div>
  </div>;
}
