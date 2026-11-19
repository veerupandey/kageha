import { api } from "./client";
import type { SessionSummary } from "./types";

/** Publish the recent page promptly, then retrieve older pinned/project chats. */
export async function loadSessionPages(
  firstLimit: number,
  onPage: (sessions: SessionSummary[], complete: boolean) => void,
  signal: AbortSignal,
) {
  const collected = new Map<string, SessionSummary>();
  let offset = 0;
  let limit = Math.max(1, Math.min(firstLimit, 200));
  while (!signal.aborted) {
    const data = await api<{ sessions: SessionSummary[]; next_offset?: number | null }>(
      `/api/sessions?limit=${limit}${offset ? `&offset=${offset}` : ""}`, { signal });
    if (signal.aborted) return;
    if (!Array.isArray(data.sessions)) throw new Error("Session list was incomplete. Please retry.");
    for (const session of data.sessions) collected.set(session.session_id, session);
    const next = data.next_offset;
    const complete = next == null;
    if (!complete && (!Number.isSafeInteger(next) || next <= offset)) throw new Error("Invalid session pagination cursor");
    onPage([...collected.values()], complete);
    if (complete) return;
    offset = next;
    limit = 200;
  }
}
