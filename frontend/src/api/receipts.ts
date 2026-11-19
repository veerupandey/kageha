import { ApiError, api } from "./client";
export type RequestReceipt = {
  receipt: { state: "received" | "accepted" | "finished" | "interrupted" | "cancel_requested" | "rejected"; turn_id: string; result_status?: string; message?: string };
  turn?: { turn_id: string; status: string; phase: string } | null;
};

/** Read-only: never retries the chat POST, including after server restart. */
export async function recoverReceipt(sessionId: string, requestId: string, opts: {
  stopped: () => boolean; onReceived?: () => void; waitMs?: number;
}): Promise<RequestReceipt | null> {
  const deadline = Date.now() + (opts.waitMs ?? 30_000);
  let last: RequestReceipt | null = null;
  while (!opts.stopped() && Date.now() < deadline) {
    try {
      last = await api<RequestReceipt>(`/api/sessions/${encodeURIComponent(sessionId)}/requests/${encodeURIComponent(requestId)}`,
        { signal: AbortSignal.timeout(Math.max(1, Math.min(5000, deadline - Date.now()))) });
    } catch (err) {
      if (opts.stopped()) return null;
      if (err instanceof ApiError && err.status < 500 && err.status !== 404 && err.status !== 429) return last;
      // A request can still be entering admission while the stream reconnects.
      // Brief proxy failures and a temporarily absent receipt share this window.
      await new Promise(resolve => setTimeout(resolve, Math.min(1000, Math.max(0, deadline - Date.now()))));
      continue;
    }
    // Stop or a replacement request may arrive while the GET is in flight.
    if (opts.stopped()) return null;
    if (!last?.receipt) return null;
    if (last.receipt.turn_id || last.receipt.state !== "received") return last;
    opts.onReceived?.();
    await new Promise(resolve => setTimeout(resolve, Math.min(1000, Math.max(0, deadline - Date.now()))));
  }
  return opts.stopped() ? null : last;
}
