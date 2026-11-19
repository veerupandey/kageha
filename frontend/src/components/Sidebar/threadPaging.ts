/** Sidebar thread window: first paint size and each scroll page. */
export const THREAD_PAGE = 30;

export function activeThreads<T extends { archived?: boolean; is_subagent?: boolean; session_id?: string; parent_session_id?: string | null }>(sessions: T[]): T[] {
  return topLevelThreads(sessions).filter((s) => !s.archived);
}

export function topLevelThreads<T extends { is_subagent?: boolean; session_id?: string; parent_session_id?: string | null }>(sessions: T[]): T[] {
  return sessions.filter((s) => !s.is_subagent
    && !(s.parent_session_id && s.session_id && s.parent_session_id !== s.session_id));
}

export function clampVisible(n: number, total: number, page = THREAD_PAGE): number {
  if (total <= page) return page;
  return Math.min(Math.max(n, page), total);
}

export function growVisible(n: number, total: number, page = THREAD_PAGE): number {
  return Math.min(n + page, total);
}
