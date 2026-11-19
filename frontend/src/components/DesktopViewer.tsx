import { memo, useEffect, useRef, useState } from "react";
import RFB from "@novnc/novnc";
import { ApiError, api } from "../api/client";
import { desktopWsUrl, rfbViewerMode } from "../lib/desktopBoot";

type DesktopInfo = {
  ok?: boolean;
  wsPath?: string;
  password?: string;
  adopted?: boolean;
  novncReady?: boolean;
};

type SizedRfb = RFB & { _fbWidth?: number; _fbHeight?: number };

function applyViewerMode(rfb: RFB, controlling: boolean, visible: boolean) {
  const mode = rfbViewerMode({ controlling, visible });
  rfb.viewOnly = mode.viewOnly;
  rfb.qualityLevel = mode.qualityLevel;
  rfb.compressionLevel = mode.compressionLevel;
  rfb.scaleViewport = mode.scaleViewport;
  rfb.resizeSession = mode.resizeSession;
}

function stampFramebuffer(host: HTMLElement | null, rfb: RFB) {
  if (!host) return;
  const sized = rfb as SizedRfb;
  const width = Number(sized._fbWidth || 0);
  const height = Number(sized._fbHeight || 0);
  if (width > 0 && height > 0) {
    host.dataset.fbWidth = String(width);
    host.dataset.fbHeight = String(height);
  }
}

export const DesktopViewer = memo(function DesktopViewer({
  sessionId,
  controlling,
  visible = true,
  ready,
  onConnected,
  onStatus,
}: {
  sessionId: string;
  controlling: boolean;
  visible?: boolean;
  ready: boolean;
  onConnected?: (connected: boolean) => void;
  onStatus?: (text: string) => void;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const rfbRef = useRef<RFB | null>(null);
  const controllingRef = useRef(controlling);
  controllingRef.current = controlling;
  const visibleRef = useRef(visible);
  visibleRef.current = visible;
  const onConnectedRef = useRef(onConnected);
  onConnectedRef.current = onConnected;
  const onStatusRef = useRef(onStatus);
  onStatusRef.current = onStatus;
  const [failed, setFailed] = useState("");
  const [connected, setConnected] = useState(false);
  const [actualSize, setActualSize] = useState(false);

  useEffect(() => {
    setConnected(false);
    setFailed("");
    onConnectedRef.current?.(false);
    onStatusRef.current?.("");
    if (!ready || !sessionId || !hostRef.current) return;
    let cancelled = false;
    let retry = 0;
    let timer = 0;
    let deadline = 0;
    let attempt = 0;
    let controller: AbortController | null = null;
    let rfb: RFB | null = null;
    const activationStarted = Date.now();

    const disconnect = () => {
      attempt += 1;
      window.clearTimeout(deadline);
      controller?.abort();
      controller = null;
      const current = rfb;
      rfb = null;
      rfbRef.current = null;
      try { current?.disconnect(); } catch { /* already gone */ }
    };

    const retryConnection = (message: string) => {
      if (cancelled) return;
      disconnect();
      setConnected(false);
      setFailed(message);
      onConnectedRef.current?.(false);
      onStatusRef.current?.(message);
      retry += 1;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void connect(), Math.min(8000, 800 * retry));
    };

    const connect = async () => {
      if (cancelled || !hostRef.current) return;
      disconnect();
      const generation = attempt;
      controller = new AbortController();
      deadline = window.setTimeout(() => {
        if (generation === attempt) retryConnection("Screen connection timed out. Retrying automatically.");
      }, 20_000);
      try {
        const info = await api<DesktopInfo>(
          `/api/sessions/${encodeURIComponent(sessionId)}/desktop`,
          { signal: controller.signal },
        );
        if (cancelled || generation !== attempt || !hostRef.current) return;
        if (!info.novncReady) throw new Error("Screen is still starting. Retrying automatically.");
        const next = new RFB(hostRef.current, desktopWsUrl(sessionId, info.wsPath), {
          credentials: { password: String(info.password || "") },
        });
        rfb = next;
        rfbRef.current = next;
        next.clipViewport = false;
        next.showDotCursor = true;
        next.background = "var(--color-canvas)";
        applyViewerMode(next, controllingRef.current, visibleRef.current);
        next.addEventListener("connect", () => {
          if (cancelled || generation !== attempt) return;
          window.clearTimeout(deadline);
          retry = 0;
          stampFramebuffer(hostRef.current, next);
          applyViewerMode(next, controllingRef.current, visibleRef.current);
          setFailed("");
          setConnected(true);
          onConnectedRef.current?.(true);
          onStatusRef.current?.("");
        });
        next.addEventListener("disconnect", () => {
          if (cancelled || generation !== attempt) return;
          retryConnection("Screen connection dropped. Reconnecting automatically.");
        });
        next.addEventListener("securityfailure", () => {
          if (cancelled || generation !== attempt) return;
          retryConnection("Screen authentication failed. Refreshing the connection.");
        });
      } catch (err) {
        if (cancelled || generation !== attempt) return;
        // The first screen request can beat session activation. Keep the
        // connecting state briefly; do not present that race as a failure.
        // Other errors (including a blocked computer) remain visible.
        const activating = err instanceof ApiError && err.status === 409
          && err.message === "task machine is not active"
          && Date.now() - activationStarted < 20_000;
        retryConnection(activating ? "" : err instanceof Error ? err.message : String(err));
      }
    };

    void connect();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
      disconnect();
      onConnectedRef.current?.(false);
    };
  }, [sessionId, ready]);

  useEffect(() => {
    const rfb = rfbRef.current;
    if (!rfb) return;
    applyViewerMode(rfb, controlling && connected, visible);
    rfb.scaleViewport = !actualSize;
    stampFramebuffer(hostRef.current, rfb);
  }, [controlling, visible, connected, actualSize]);

  return (
    <>
      <div
        ref={hostRef}
        className="desktop-rfb absolute inset-0 bg-canvas"
        data-testid="desktop-rfb"
        title="This computer's screen"
      />
      {visible && connected && <div className="ka-viewer-controls">
        <span>{controlling ? "You control this computer" : "Watching agent"}</span>
        <button type="button" aria-pressed={actualSize} onClick={() => setActualSize((value) => !value)}>{actualSize ? "Fit screen" : "Actual size"}</button>
      </div>}
      {failed ? (
        <p className="pointer-events-none absolute bottom-3 left-3 z-[2] rounded-md bg-black/70 px-2 py-1 text-[0.62rem] text-amber-200">
          {failed}
        </p>
      ) : null}
    </>
  );
});
