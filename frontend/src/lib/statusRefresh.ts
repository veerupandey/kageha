/**
 * Reloads turn statuses from the server when the tab becomes visible or the
 * window gains focus (Requirement 18.3): a stream that dropped while the tab
 * was hidden cannot leave a chat looking like it is still running. Focus and
 * visibility usually fire together; one refresh per `minIntervalMs` runs.
 */
export function watchSessionStatus(
  refresh: () => void,
  opts: { minIntervalMs?: number; doc?: Document; win?: Window; now?: () => number } = {},
): () => void {
  const doc = opts.doc ?? document;
  const win = opts.win ?? window;
  const now = opts.now ?? Date.now;
  const minInterval = opts.minIntervalMs ?? 1000;
  let last = -Infinity;
  const run = () => {
    if (doc.visibilityState !== "visible") return;
    const at = now();
    if (at - last < minInterval) return;
    last = at;
    refresh();
  };
  doc.addEventListener("visibilitychange", run);
  win.addEventListener("focus", run);
  return () => {
    doc.removeEventListener("visibilitychange", run);
    win.removeEventListener("focus", run);
  };
}
