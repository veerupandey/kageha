export interface UrlState {
  sessionId?: string | null;
  agentId?: string | null;
  surface?: "computer" | "browser" | null;
}

export function parseUrlState(location?: Location): UrlState {
  if (typeof window === "undefined" && !location) return {};
  const loc = location || window.location;
  const search = new URLSearchParams(loc.search);
  const querySession = search.get("session") || search.get("s") || search.get("thread");
  const pathSession = loc.pathname.startsWith("/c/") ? decodeURIComponent(loc.pathname.slice(3)) : null;
  const sessionId = querySession || pathSession || null;
  const agentId = search.get("agent") || null;
  const rawSurface = search.get("surface");
  const surface = rawSurface === "computer" || rawSurface === "browser" ? rawSurface : undefined;
  return { sessionId, agentId, ...(surface ? { surface } : {}) };
}

export function updateUrl(state: UrlState, options: { replace?: boolean } = {}): void {
  if (typeof window === "undefined" || !window.history) return;
  try {
    const current = new URL(window.location.href);
    if (state.sessionId !== undefined) {
      if (state.sessionId) {
        current.searchParams.set("session", state.sessionId);
      } else {
        current.searchParams.delete("session");
        current.searchParams.delete("s");
        current.searchParams.delete("thread");
        if (current.pathname.startsWith("/c/")) {
          current.pathname = "/";
        }
      }
    }
    if (state.agentId !== undefined) {
      if (state.agentId) {
        current.searchParams.set("agent", state.agentId);
      } else {
        current.searchParams.delete("agent");
      }
    }
    if (state.surface !== undefined) {
      if (state.surface) current.searchParams.set("surface", state.surface);
      else current.searchParams.delete("surface");
    }
    const newRelativeUrl = current.pathname + (current.search ? current.search : "");
    const currentRelativeUrl = window.location.pathname + window.location.search;
    if (newRelativeUrl === currentRelativeUrl) return;

    if (options.replace) {
      window.history.replaceState({ ...window.history.state, ...state }, "", newRelativeUrl);
    } else {
      window.history.pushState({ ...window.history.state, ...state }, "", newRelativeUrl);
    }
  } catch {
    // Ignore history API errors in restricted sandbox or test environments
  }
}

/** A URL changes presentation only; it never grants control or starts work. */
export function applyUrlSurface(state: UrlState, store: {
  sessionId: string | null;
  pinLiveSurface: (surface: "computer" | "browser") => void;
  setPrefs?: (patch: {rightPanelCollapsed: boolean}) => void;
}): boolean {
  if (!state.surface || !state.sessionId || store.sessionId !== state.sessionId) return false;
  store.pinLiveSurface(state.surface);
  store.setPrefs?.({rightPanelCollapsed: false});
  return true;
}
