import { api } from "../api/client";

// Successful files are reused on retry within this page, scoped to the session.
const uploaded = new WeakMap<File, Map<string, string>>();
export const UPLOAD_TIMEOUT_MS = 60_000;

/** Completed-file progress; wait for the entire batch before offering retry. */
export async function uploadSessionFiles(
  sessionId: string,
  files: File[],
  onProgress?: (completed: number, total: number) => void,
  signal?: AbortSignal,
): Promise<string[]> {
  if (!files.length) return [];
  let completed = 0;
  onProgress?.(0, files.length);
  const results = await Promise.allSettled(files.map(async file => {
    let path = uploaded.get(file)?.get(sessionId);
    if (!path) {
      const body = new FormData();
      body.append("file", file, file.name);
      const data = await api<{ path?: string }>(
        `/api/sessions/${encodeURIComponent(sessionId)}/upload`,
        { method: "POST", body, signal: AbortSignal.any([
          ...(signal ? [signal] : []), AbortSignal.timeout(UPLOAD_TIMEOUT_MS),
        ]) },
      );
      if (!data.path) throw new Error(`Upload did not complete: ${file.name}`);
      path = data.path;
      const sessions = uploaded.get(file) || new Map<string, string>();
      sessions.set(sessionId, path);
      uploaded.set(file, sessions);
    }
    onProgress?.(++completed, files.length);
    return path;
  }));
  if (signal?.aborted) throw new DOMException("Upload cancelled", "AbortError");
  const failure = results.find(result => result.status === "rejected");
  if (failure?.status === "rejected") throw failure.reason;
  return results.map(result => (result as PromiseFulfilledResult<string>).value);
}
