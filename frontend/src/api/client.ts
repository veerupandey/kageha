export class ApiError extends Error {
  status: number;
  data: unknown;

  constructor(message: string, status: number, data?: unknown) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.data = data;
  }
}

export class ApiTimeoutError extends Error {
  constructor(path: string) {
    const action = /^\/api\/sessions(?:\?|$)/.test(path) ? "Refreshing the chat list"
      : /^\/api\/jobs(?:\?|$)/.test(path) ? "Refreshing specialist updates"
      : /^\/api\/sessions\/[^/]+\/upload$/.test(path) ? "Uploading the attachment" : "Loading app updates";
    super(`${action} took too long. This does not confirm that your task failed.`);
    this.name = "TimeoutError";
  }
}

/**
 * The server answers 401 {"error":"auth_required"} when a remote (LAN /
 * tunnel) session has no valid login cookie. Send the user to the login
 * page once instead of leaving every panel failing silently.
 */
export function redirectToLoginIfNeeded(status: number, data: unknown): boolean {
  if (status !== 401) return false;
  const code = (data as { error?: unknown } | null)?.error;
  if (code !== "auth_required") return false;
  if (typeof window === "undefined" || window.location.pathname === "/login") return true;
  const next = window.location.pathname + window.location.search;
  window.location.assign(`/login?next=${encodeURIComponent(next)}`);
  return true;
}

export async function api<T = Record<string, unknown>>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  const headers = new Headers(options.headers || {});
  const isForm = typeof FormData !== "undefined" && options.body instanceof FormData;
  if (!isForm && !headers.has("Content-Type") && options.body) {
    headers.set("Content-Type", "application/json");
  }
  // Read-only recovery must not wait forever on a half-open HTTP connection.
  // Caller cancellation still wins; writes keep their explicit semantics.
  const readOnly = !options.method || options.method.toUpperCase() === "GET";
  for (let attempt = 0; ; attempt += 1) {
    const signal = readOnly
      ? AbortSignal.any([...(options.signal ? [options.signal] : []), AbortSignal.timeout(15_000)])
      : options.signal;
    try {
      const res = await fetch(path, { ...options, signal, headers });
      const data = (await res.json().catch(err => {
        if (signal?.aborted) throw err;
        return {};
      })) as Record<string, unknown>;
      if (!res.ok) {
        redirectToLoginIfNeeded(res.status, data);
        throw new ApiError(String(data.error || `Request failed (${res.status})`), res.status, data);
      }
      return data as T;
    } catch (err) {
      if (signal?.aborted && signal.reason?.name === "TimeoutError") {
        // Retry only reads with a fresh deadline, never a submission or an
        // expired caller deadline. Caller cancellation always wins.
        if (readOnly && attempt === 0 && !options.signal?.aborted) continue;
        throw new ApiTimeoutError(path);
      }
      throw err;
    }
  }
}
