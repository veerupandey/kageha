import { useEffect, useState } from "react";
import { api } from "../../api/client";

type Panel = {
  enabled: boolean;
  worker_available?: boolean;
  state?: {
    revision: number;
    focus?: "browser" | "computer" | "files" | "activity" | "checklist" | null;
    focus_revision?: number;
    owner: "agent" | "user";
    view: { component: "checklist"; props: { title: string; items: { id: string; label: string }[] } } | null;
  };
  selection?: { payload: { id: string } } | null;
};

export function useReviewPanel(sessionId: string | null) {
  const [record, setRecord] = useState<{ sessionId: string; panel: Panel }>();
  const [pending, setPending] = useState<string>();
  const [error, setError] = useState<{ sessionId: string; message: string }>();
  const path = sessionId ? `/api/sessions/${encodeURIComponent(sessionId)}/ui-panel` : "";
  const panel = record?.sessionId === sessionId ? record.panel : undefined;

  useEffect(() => {
    if (!sessionId) return;
    const activeSession = sessionId;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      let next: Panel | undefined;
      try {
        const response = await api<Panel>(path, { signal: controller.signal });
        next = response;
        if (!controller.signal.aborted) {
          setRecord(previous => previous?.sessionId === activeSession
            && (previous.panel.state?.revision ?? 0) > (response.state?.revision ?? 0)
            ? previous : { sessionId: activeSession, panel: response });
        }
      } catch {
        // A later poll can recover the connection.
      }
      if (!controller.signal.aborted && next?.enabled !== false) timer = setTimeout(poll, 1200);
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [path, sessionId]);

  async function act(action: string, id?: string) {
    if (!sessionId || !panel?.state || panel.worker_available === false || pending === sessionId) return;
    setPending(sessionId);
    setError(undefined);
    try {
      const next = await api<Panel>(path, {
        method: "POST", body: JSON.stringify({ action, id, revision: panel.state.revision }),
      });
      setRecord(previous => previous?.sessionId !== sessionId
        || (previous.panel.state?.revision ?? 0) > (next.state?.revision ?? 0)
        ? previous : { sessionId, panel: next });
    } catch (err) {
      const message = err instanceof Error ? err.message : "Could not update the checklist.";
      setError({ sessionId, message: message === "stale_revision" ? "The checklist changed. Try again after it refreshes." : message });
    } finally {
      setPending(previous => previous === sessionId ? undefined : previous);
    }
  }

  return { panel, act, pending: pending === sessionId, error: error?.sessionId === sessionId ? error.message : "" };
}

export function ReviewPanel({ panel, act, pending, error }: ReturnType<typeof useReviewPanel>) {
  const state = panel?.state;
  const props = state?.view?.props;
  const unavailable = panel?.worker_available === false;
  if (!state || !props) return <p className="p-4 text-sm text-muted">No review checklist is open.</p>;

  return <section className="min-h-0 flex-1 overflow-auto p-4" aria-label="Review checklist">
    <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
      <h2 className="text-sm font-semibold text-ink">{props.title}</h2>
      <button type="button" disabled={pending || unavailable} className="rounded border border-line px-2 py-1 text-xs"
        onClick={() => void act(state.owner === "user" ? "return_control" : "take_control")}>
        {state.owner === "user" ? "Return control" : "Take control"}
      </button>
    </div>
    {state.owner === "user" && <p className="mb-3 text-xs text-muted" role="status">You have control</p>}
    <ul className="space-y-2">
      {props.items.map(item => {
        const selected = panel?.selection?.payload.id === item.id;
        return <li key={item.id}>
          <button type="button" disabled={pending || unavailable} aria-pressed={selected}
            className="w-full rounded border border-line px-3 py-2 text-left text-sm hover:bg-line/30 aria-pressed:border-accent"
            onClick={() => void act("select", item.id)}>{item.label}</button>
        </li>;
      })}
    </ul>
    {unavailable ? <p role="status" className="mt-3 text-sm text-muted">Review controls are temporarily unavailable.</p>
      : error && <p role="alert" className="mt-3 text-sm text-rose-500">{error}</p>}
  </section>;
}
