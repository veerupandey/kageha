import { useCallback, useEffect, useRef } from "react";
import { api } from "../api/client";
import { useAppStore } from "../store";
import { notifyPeerAlert } from "./peerNotifications";

type PairRequest = {
  request_id: string;
  requester?: { name?: string };
};

type Grant = {
  grant_id: string;
  capability_id: string;
  state: string;
  provider_id: string;
};

type PeerIndex = { local_identity?: { agent_id?: string } };

/** Poll cadence when the SSE push stream is down (fallback path). */
const POLL_MS_DISCONNECTED = 6000;
/** Poll cadence when SSE is connected — just a correctness backstop, since
 * push already tells us the moment something changes. */
const POLL_MS_CONNECTED = 30000;
const SSE_RETRY_MS = 4000;

/**
 * Ambient, AirDrop-style awareness for incoming peer requests.
 *
 * Mounted once at the app root (not inside the Agent Network panel) so a
 * pairing request or grant request surfaces as a toast + sidebar badge no
 * matter what the user is doing. Primary transport is a push SSE stream
 * (/api/peers/events) for near-instant notification; REST polling of the
 * same endpoints the panel already uses remains as a correctness backstop
 * and as the fallback transport if the stream drops.
 */
export function usePeerAlerts(): void {
  const showToast = useAppStore((s) => s.showToast);
  const peersEnabled = useAppStore((s) => s.meta?.features?.peers !== false);
  const setPeerPendingCount = useAppStore((s) => s.setPeerPendingCount);
  const seenPairingIds = useRef<Set<string>>(new Set());
  const seenGrantIds = useRef<Set<string>>(new Set());
  const primed = useRef(false);
  const sseConnected = useRef(false);

  const tick = useCallback(async () => {
    try {
      const [pairingResult, grantResult, peerResult] = await Promise.all([
        api<{ requests: PairRequest[] }>("/api/peers/pairing"),
        api<{ grants: Grant[] }>("/api/peers/grants"),
        api<PeerIndex>("/api/peers"),
      ]);
      const localAgentId = peerResult.local_identity?.agent_id || "";
      const pairing = pairingResult.requests || [];
      // Grants awaiting *this* agent's approval — the requester side has
      // its own "requested" rows too, but those aren't ours to review.
      const pendingGrants = (grantResult.grants || []).filter(
        (grant) => grant.state === "requested" && grant.provider_id === localAgentId,
      );

      if (primed.current) {
        for (const request of pairing) {
          if (seenPairingIds.current.has(request.request_id)) continue;
          const name = request.requester?.name || "An agent";
          notifyPeerAlert(showToast, `${name} wants to pair`);
        }
        for (const grant of pendingGrants) {
          if (seenGrantIds.current.has(grant.grant_id)) continue;
          notifyPeerAlert(showToast, `Peer requested access to ${grant.capability_id}`);
        }
      }

      seenPairingIds.current = new Set(pairing.map((r) => r.request_id));
      seenGrantIds.current = new Set(pendingGrants.map((g) => g.grant_id));
      primed.current = true;
      setPeerPendingCount(pairing.length + pendingGrants.length);
    } catch {
      /* Peer network may be disabled or unreachable — stay silent. */
    }
  }, [showToast, setPeerPendingCount]);

  useEffect(() => {
    if (!peersEnabled) return;
    let cancelled = false;
    let pollTimer = 0;
    let sseRetryTimer = 0;
    let source: EventSource | null = null;

    const schedulePoll = () => {
      window.clearInterval(pollTimer);
      const interval = sseConnected.current ? POLL_MS_CONNECTED : POLL_MS_DISCONNECTED;
      pollTimer = window.setInterval(() => void tick(), interval);
    };

    const connectSse = () => {
      if (cancelled || typeof EventSource === "undefined") return;
      source = new EventSource("/api/peers/events");
      source.addEventListener("open", () => {
        sseConnected.current = true;
        schedulePoll();
      });
      source.addEventListener("peer.pairing_requested", () => void tick());
      source.addEventListener("peer.grant_requested", () => void tick());
      source.addEventListener("error", () => {
        sseConnected.current = false;
        schedulePoll();
        source?.close();
        source = null;
        if (!cancelled) sseRetryTimer = window.setTimeout(connectSse, SSE_RETRY_MS);
      });
    };

    void tick();
    schedulePoll();
    connectSse();

    return () => {
      cancelled = true;
      window.clearInterval(pollTimer);
      window.clearTimeout(sseRetryTimer);
      source?.close();
    };
  }, [tick, peersEnabled]);
}
