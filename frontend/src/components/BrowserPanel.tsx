import { BrowserDecisionRoute } from "./BrowserDecisionRoute";
import { useCallback, useEffect, useRef, useState } from "react";
import type { MouseEvent, WheelEvent } from "react";
import * as Popover from "@radix-ui/react-popover";
import { api } from "../api/client";
import type { BrowserState } from "../api/types";
import {
  browserStatePollMs,
  browserActionNeedsReview,
  browserCanvasSize,
  browserStreamUrl,
  browserProfileLabel,
  browserBootCopy,
  canScreencast,
  panelViewportFor,
  isBlankBrowserUrl,
  normalizeBrowserUrl,
  isEmbeddableNovncUrl,
  isEmbeddedHost,
  showEmbeddedGuest,
  highlightRectFromFit,
  isExternalBrowser,
  needsConsentChoice,
  openOutsideKind,
  openOutsideTitle,
  formatTabTitle,
  pageProtocolInfo,
  getArtifactUrl,
  previewPointFromFit,
  screencastViewport,
  shouldSamplePointer,
  stageAddressLabel,
  streamRetryDelayMs,
  streamRetryKey,
} from "../lib/browserPreview";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { useAppStore } from "../store";
import { StageBoot, StageIdentity, StageWindow } from "./shared/Stage";
import { BrowserSessionSelect } from "./BrowserSessionSelect";
import { BrowserActionNotice } from "./BrowserActionNotice";
import { BrowserLiveSurface } from "./BrowserLiveSurface";
import { publishBrowserControls, useRequestedBrowserTarget } from "../lib/browserWorkspace";
import { agentKey, useAgentValue } from "../lib/agentContinuity";

// Stable handles survive tab reordering; indexes support older backends.
function tabTarget(tab: BrowserState["tabs"][number]) {
  return tab.tabHandle ? { tabHandle: tab.tabHandle } : { index: tab.index };
}

const EMPTY: BrowserState = {
  sessionId: "",
  connected: false,
  backend: "lightpanda",
  url: "",
  title: "",
  tabs: [],
  lockedBy: null,
  canControl: true,
};

interface BrowserPanelProps {
  onCollapse?: () => void;
  embeddedInWorkspace?: boolean;
  visible?: boolean;
}

export function BrowserPanel({ onCollapse, embeddedInWorkspace = false, visible }: BrowserPanelProps) {
  const sessionId = useAppStore((s) => s.sessionId);
  const currentSessionRef = useRef(sessionId);
  const sessionGeneration = useRef(0);
  if (currentSessionRef.current !== sessionId) sessionGeneration.current += 1;
  currentSessionRef.current = sessionId;
  const runStatus = useAppStore((s) => s.runStatus);
  const pendingApproval = useAppStore((s) => s.pendingApproval);
  const machineActive = useAppStore((s) => s.machineActive);
  const workspaceSurface = useAppStore((s) => s.workspaceSurface);
  const liveSurface = useAppStore((s) => s.liveSurface);
  // True when this panel is actually visible in the workspace (not hidden behind
  // Artifacts/Trace). Used to suppress side-effects while the panel is off-screen.
  const panelVisible = visible ?? (workspaceSurface === "live" && liveSurface === "browser" && !machineActive);
  const machineNovncUrl = useAppStore((s) => s.machineNovncUrl);
  const panelMode = useAppStore((s) => s.browserPanelMode);
  const setPanelMode = useAppStore((s) => s.setBrowserPanelMode);
  const setSurfaceControlling = useAppStore((s) => s.setSurfaceControlling);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const webpagePath = useAppStore((s) => {
    const webpages = s.canvasItems.filter((item) => item.kind === "webpage");
    return webpages.length === 1 ? webpages[0].path : null;
  });
  const [storedState, setState] = useState<BrowserState>(EMPTY);
  const hasSessionState = storedState.sessionId === sessionId;
  const state = hasSessionState ? storedState : EMPTY;
  const rustAccountHandoff = state.backend === "rust"
    && pendingApproval?.action === "request_secure_login"
    && (pendingApproval.sessionId || pendingApproval.session_id || sessionId) === sessionId;
  let accountTab = "";
  if (rustAccountHandoff && typeof pendingApproval.detail === "string") {
    try { accountTab = String(JSON.parse(pendingApproval.detail).rust_tab || ""); }
    catch { /* The approval card handles incomplete details. */ }
  }
  const onAccountTab = Boolean(accountTab && state.tabs.some(tab => tab.active && tab.tabHandle === accountTab));
  const stateRef = useRef(state);
  stateRef.current = state;
  const requestSequence = useRef(0);
  const appliedSequence = useRef(0);
  const stateRead = useRef<{ sessionId: string; controller: AbortController } | null>(null);
  const [paintedBinding, setPaintedBinding] = useState<string | undefined>();
  const [paintedTarget, setPaintedTarget] = useState<string | undefined>();
  const [address, setAddress] = useState("");
  const [error, setError] = useState("");
  const operationError = useRef("");
  const previewRef = useRef<HTMLImageElement | HTMLCanvasElement | null>(null);
  const setPreviewElement = useCallback((node: HTMLImageElement | HTMLCanvasElement | null) => { previewRef.current = node; }, []);
  const previewBoxRef = useRef<HTMLDivElement | null>(null);
  const addressRef = useRef<HTMLInputElement | null>(null);
  const tabListRef = useRef<HTMLDivElement | null>(null);
  const pointerDown = useRef(false);
  const heldPointer = useRef<{ session: string; binding: string; button: string; x: number; y: number } | null>(null);
  const inputQueue = useRef<Promise<unknown>>(Promise.resolve());
  const moveSequence = useRef(0);
  const lastMoveAt = useRef(0);
  const lastMovePt = useRef({ x: 0, y: 0 });
  const [previewMetrics, setPreviewMetrics] = useState<{
    boxW: number;
    boxH: number;
    natW: number;
    natH: number;
  } | null>(null);
  const [storedPaintedUrl, setPaintedUrl] = useState("");
  const paintedUrl = hasSessionState ? storedPaintedUrl : "";
  // Key the stream failed under (see streamRetryKey), or null.
  const [failedStreamKey, setFailedStreamKey] = useState<string | null>(null);
  // Consecutive stream errors; reset by the first frame that loads.
  const [streamFailures, setStreamFailures] = useState(0);
  // A successful recovery must keep the same image/connection alive.
  const [streamAttempt, setStreamAttempt] = useState(0);
  const activeStreamUrl = useRef("");
  const [paintedStream, setPaintedStream] = useState("");
  const lastPaintReport = useRef({ binding: "", at: 0 });
  const [hydrated, setHydrated] = useState(false);
  const [promoting, setPromoting] = useState("");
  const [copied, setCopied] = useState(false);
  const [reloading, setReloading] = useState(false);
  const [controlChanging, setControlChanging] = useState(false);
  const controlTransfer = useRef(false);
  const [userClicks, setUserClicks] = useState<Array<{ id: number; x: number; y: number }>>([]);
  const [deviceMode, setDeviceMode] = useState<"desktop" | "mobile">("desktop");
  const [previewZoom, setPreviewZoom] = useState<"responsive" | "fit" | "fit-width" | number>("responsive");
  const [hdEnabled, setHdEnabled] = useAgentValue(agentKey(sessionId || "", "browser-hd"), true);
  const layoutRequest = useRef("");
  const [viewMode, setViewMode] = useState<"auto" | "iframe" | "stream">("auto");
  const [panelFit, setPanelFit] = useState(0);
  const [tabCandidates, setTabCandidates] = useState<BrowserState | undefined>();
  const [previewBoxSize, setPreviewBoxSize] = useState({ width: 0, height: 0 });
  const lastNovncRef = useRef("");
  const rustClick = useRef<{ binding: string; generation: number; x: number; y: number; button: string } | null>(null);
  const releasePointer = useCallback(() => {
    rustClick.current = null;
    const held = heldPointer.current;
    if (!held) return;
    heldPointer.current = null;
    pointerDown.current = false;
    inputQueue.current = inputQueue.current.catch(() => {}).then(() => api(
      `/api/sessions/${encodeURIComponent(held.session)}/browser/action`, {
        method: "POST", body: JSON.stringify({ action: "mouseup", x: held.x, y: held.y,
          button: held.button, bindingId: held.binding }),
      },
    )).catch(() => {});
  }, []);
  const applyState = useCallback((next: BrowserState, sequence: number, generation: number) => {
    if (generation !== sessionGeneration.current || next.sessionId !== currentSessionRef.current) return false;
    const prev = stateRef.current;
    if (sequence < appliedSequence.current) return false;
    if (next.stateUnavailable && prev.sessionId === next.sessionId && prev.backend === next.backend && prev.bindingId) {
      appliedSequence.current = sequence;
      const retained = { ...prev, stateUnavailable: true, error: next.error };
      stateRef.current = retained;
      setState(retained);
      setError(operationError.current);
      return true;
    }
    if (prev.bindingId?.split(":")[0] === next.bindingId?.split(":")[0]
        && (next.revision ?? 0) < (prev.revision ?? 0)) return false;
    appliedSequence.current = sequence;
    const sameBinding = prev.bindingId === next.bindingId;
    if (!sameBinding) {
      operationError.current = "";
      releasePointer();
      inputQueue.current = Promise.resolve();
    }
    const accepted = sameBinding && !next.frame?.url && prev.frame?.url
      ? { ...next, frame: prev.frame } : next;
    stateRef.current = accepted;
    setState(accepted);
    if (!sameBinding) {
      setPanelFit(0);
      setPaintedUrl("");
    setPaintedTarget(undefined);
    setPaintedBinding(undefined);
      setPaintedBinding(undefined);
      setPreviewMetrics(null);
      setReloading(false);
      setCopied(false);
      pointerDown.current = false;
    }
    // A draft belongs to its browser/tab binding. Keep it while polling the
    // same page, but never carry it into a different tab or profile.
    if (!sameBinding || document.activeElement !== addressRef.current) setAddress(next.url || "");
    setError(next.error || operationError.current);
    return true;
  }, [releasePointer]);

  useEffect(() => () => {
    sessionGeneration.current += 1;
    releasePointer();
  }, [releasePointer]);

  const measurePreview = useCallback(() => {
    const image = previewRef.current;
    if (!image) return;
    const rect = image.getBoundingClientRect();
    if (rect.width < 1 || rect.height < 1) return;
    setPreviewMetrics({
      boxW: rect.width,
      boxH: rect.height,
      natW: "naturalWidth" in image ? image.naturalWidth : image.width,
      natH: "naturalHeight" in image ? image.naturalHeight : image.height,
    });
  }, []);

  useEffect(() => {
    measurePreview();
  }, [deviceMode, measurePreview]);

  const previewPoint = (event: MouseEvent<HTMLElement>) => {
    const image = previewRef.current;
    const viewport = screencastViewport(state);
    if (!image) return { x: 0, y: 0 };
    return previewPointFromFit({
      clientX: event.clientX,
      clientY: event.clientY,
      rect: image.getBoundingClientRect(),
      naturalWidth: ("naturalWidth" in image ? image.naturalWidth : image.width) || viewport.width,
      naturalHeight: ("naturalHeight" in image ? image.naturalHeight : image.height) || viewport.height,
      viewport,
      fit: "contain",
      clamp: state.backend !== "rust",
    });
  };

  const refresh = useCallback(async () => {
    if (!sessionId || promoting) return;
    if (stateRead.current?.sessionId === sessionId) return;
    stateRead.current?.controller.abort();
    const controller = new AbortController();
    const request = { sessionId, controller };
    stateRead.current = request;
    const requestedSession = sessionId;
    const generation = sessionGeneration.current;
    const sequence = ++requestSequence.current;
    try {
      const next = await api<BrowserState>(`/api/sessions/${encodeURIComponent(sessionId)}/browser`, { signal: controller.signal });
      if (controller.signal.aborted || currentSessionRef.current !== requestedSession) return;
      if (applyState(next, sequence, generation)) setHydrated(true);
    } catch (err) {
      if (!controller.signal.aborted && currentSessionRef.current === requestedSession && generation === sessionGeneration.current) {
        setError(err instanceof Error ? err.message : "Could not restore this chat's browser view.");
      }
    } finally {
      if (stateRead.current === request) stateRead.current = null;
    }
  }, [sessionId, applyState, promoting]);

  useEffect(() => () => {
    stateRead.current?.controller.abort();
    stateRead.current = null;
  }, [sessionId]);

  useEffect(() => {
    setState(EMPTY);
    setAddress("");
    setError("");
    operationError.current = "";
    setPaintedUrl("");
    setPaintedTarget(undefined);
    setPaintedBinding(undefined);
    setFailedStreamKey(null);
    setStreamFailures(0);
    setStreamAttempt(0);
    setHydrated(false);
    setPromoting("");
    setReloading(false);
    setControlChanging(false);
    controlTransfer.current = false;
    pointerDown.current = false;
    setPaintedStream("");
    setPreviewMetrics(null);
    setPreviewZoom("responsive");
    layoutRequest.current = "";
    setPanelFit(0);
    setPaintedBinding(undefined);
    lastNovncRef.current = "";
    stateRef.current = EMPTY;
  }, [sessionId]);

  const agentDriving = state.lockedBy === "agent" || Boolean(state.agentLocked);
  const actionNeedsReview = browserActionNeedsReview(state.pendingBrowserActions, state.browserActionHold);
  const agentRunning = !actionNeedsReview && !rustAccountHandoff && state.connected && agentDriving && runStatus === "running";
  const artifactUrl = getArtifactUrl(address || state.url || "", sessionId);
  const showIframe = Boolean(artifactUrl && viewMode !== "stream");
  const blankPage = isBlankBrowserUrl(state.url || "");
  // Watching uses the existing profile; DOM-only sessions have no stream.
  const liveViewport = state.layout?.mode ? state.viewport : undefined;
  const currentStreamKey = streamRetryKey(state) + (liveViewport ? `:${liveViewport.width}x${liveViewport.height}` : "");
  const streamFailed = failedStreamKey === currentStreamKey;
  const streaming = canScreencast({
    backend: state.backend,
    connected: panelVisible && !showIframe && !blankPage && state.connected && Boolean(state.bindingId)
      && (state.backend !== "rust" || state.screencast?.available),
    novnc: state.novncUrl,
    streamFailed,
  });
  const liveStreamUrl = streaming && sessionId ? browserStreamUrl(sessionId, streamAttempt, state.bindingId)
    + (liveViewport ? `&viewport=${liveViewport.width}x${liveViewport.height}` : "") : "";
  activeStreamUrl.current = liveStreamUrl;
  const streamReady = Boolean(liveStreamUrl && paintedStream === liveStreamUrl);

  useEffect(() => {
    if (!sessionId || !state.previewAcknowledgment || !state.bindingId || !panelVisible) return;
    const withdraw = () => {
      lastPaintReport.current = { binding: "", at: 0 };
      void api(`/api/sessions/${encodeURIComponent(sessionId)}/browser/paint`, {
        method: "POST", body: JSON.stringify({ bindingId: state.bindingId, displayed: false }),
      }).catch(() => {});
    };
    const visibility = () => { if (document.visibilityState === "hidden") withdraw(); };
    document.addEventListener("visibilitychange", visibility);
    return () => {
      document.removeEventListener("visibilitychange", visibility);
      withdraw();
    };
  }, [sessionId, state.previewAcknowledgment, state.bindingId, panelVisible]);

  useEffect(() => {
    if (!panelVisible || blankPage) {
      setPaintedStream("");
      setPaintedBinding(undefined);
      return;
    }
    if (!state.connected) return;
    if (!liveStreamUrl || streamReady) return;
    // Keep verified pixels while the replacement connection decodes a frame.
    const deadline = window.setTimeout(() => {
      if (activeStreamUrl.current !== liveStreamUrl) return;
      setFailedStreamKey(currentStreamKey);
      setStreamFailures(count => count + 1);
      setStreamAttempt(count => count + 1);
    }, 8000);
    return () => window.clearTimeout(deadline);
  }, [liveStreamUrl, streamReady, currentStreamKey, state.bindingId, panelVisible, state.connected, blankPage]);

  useEffect(() => {
    if (panelVisible) return;
    stateRead.current?.controller.abort();
    stateRead.current = null;
  }, [panelVisible]);

  // Transient refusals (busy, a promote in flight) retry with backoff instead
  // of leaving a static frame up for the rest of the session.
  const retryDelay = streamFailed
    ? streamRetryDelayMs({
        backend: state.backend,
        connected: state.connected,
        novnc: state.novncUrl,
        failures: streamFailures,
      })
    : null;
  useEffect(() => {
    if (retryDelay === null) return;
    const timer = window.setTimeout(() => setFailedStreamKey(null), retryDelay);
    return () => window.clearTimeout(timer);
  }, [retryDelay, streamFailures]);

  const hasLiveFrame = Boolean(liveStreamUrl || state.frame?.url || paintedUrl);

  useEffect(() => {
    if (!sessionId || (!panelVisible && (!embeddedInWorkspace || machineActive ||
      !["running", "waiting_approval"].includes(runStatus)))) return;
    void refresh();
    // Browser frames arrive through the agent stream. The state poll is a
    // reconnect/address-bar fallback, but it also bootstraps the first JPEG
    // when the model drives via evaluate. Poll faster until a frame exists
    // and while the agent holds the page; back off once the panel is painted.
    const ms = browserStatePollMs({
      hasFrame: hasLiveFrame,
      novnc: Boolean(state.novncUrl),
      streaming,
      running: runStatus === "running",
      machineActive,
      agentDriving,
    });
    const timer = window.setInterval(() => void refresh(), panelVisible ? ms : Math.max(3000, ms));
    return () => window.clearInterval(timer);
  }, [sessionId, panelVisible, runStatus, machineActive, refresh, state.novncUrl, hasLiveFrame, agentDriving, streaming, embeddedInWorkspace]);

  useEffect(() => {
    if (panelMode !== "fullscreen") return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setPanelMode("expanded");
      }
    };
    window.addEventListener("keydown", onKeyDown);
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      document.body.style.overflow = "";
    };
  }, [panelMode, setPanelMode]);

  const call = useCallback(async (path: string, body: Record<string, unknown>) => {
    if (!sessionId || stateRef.current.stateUnavailable) return;
    const requestedSession = sessionId;
    const generation = sessionGeneration.current;
    const sequence = ++requestSequence.current;
    const { bindingId, controlGeneration, backend } = stateRef.current;
    try {
      const next = await api<BrowserState>(`/api/sessions/${encodeURIComponent(sessionId)}/browser/${path}`, {
        method: "POST",
        body: JSON.stringify({ ...body,
          ...((["action", "promote", "connect", "layout", "revoke-login"].includes(path) || backend === "rust") && bindingId ? { bindingId } : {}),
          ...(backend === "rust" ? { controlGeneration } : {}),
        }),
      });
      if (currentSessionRef.current !== requestedSession) return;
      if (!applyState(next, sequence, generation)) return;
      const result = String(next.result || "");
      operationError.current = "";
      setError(next.error || (result.startsWith("ERROR") ? result : "") || "");
      return next;
    } catch (err) {
      if (currentSessionRef.current === requestedSession && generation === sessionGeneration.current && sequence >= appliedSequence.current) {
        operationError.current = err instanceof Error ? err.message : String(err);
        setError(operationError.current);
      }
    }
  }, [sessionId, applyState]);

  const revokeLogin = async () => {
    const next = await call("revoke-login", {});
    if (next?.loginAccess?.status !== "revoked") return;
    const store = useAppStore.getState();
    const pending = store.pendingApproval;
    const pendingSession = pending?.sessionId || pending?.session_id || store.sessionId;
    if (store.sessionId === sessionId && pendingSession === sessionId && pending?.action === "request_secure_login") {
      await store.resolveApproval(false);
    }
  };

  useEffect(() => {
    if (!sessionId || state.lockedBy !== "user" || !state.bindingId || !panelVisible) return;
    const binding = state.bindingId;
    const timer = window.setInterval(() => {
      if (state.backend === "rust") {
        if (stateRef.current.bindingId === binding && stateRef.current.lockedBy === "user") void call("lease", { action: "renew" });
        return;
      }
      void api(`/api/sessions/${encodeURIComponent(sessionId)}/browser/lease`, {
        method: "POST", body: JSON.stringify({ action: "renew", bindingId: binding }),
      }).catch(() => {
        if (currentSessionRef.current === sessionId && stateRef.current.bindingId === binding)
          setError("Browser control could not be renewed. Refresh the view before continuing.");
      });
    }, 10000);
    return () => window.clearInterval(timer);
  }, [sessionId, state.lockedBy, state.bindingId, state.backend, panelVisible, call]);

  useEffect(() => {
    // Wait for the first state read. Connecting before that replaces a live
    // Lightpanda session (real URL) with a blank Chromium tab.
    // Also skip while the panel is hidden (user on Artifacts/Trace tab) — the
    // browser session is already live; an unnecessary connect call would open a
    // blank tab over the page the agent is working on.
    if (!sessionId || !hydrated || !state.bindingId || state.previewOnly || state.connected || state.connectionStatus === "restoring" || state.connectionStatus === "target_unavailable" || promoting) return;
    if (state.backend === "rust" && (state.selectedProfile === "chrome" || state.selectedProfile === "comet")) return;
    if (!panelVisible) return;

    let cancelled = false;
    let tries = 0;
    const tick = async () => {
      if (cancelled || tries >= 8) return;
      tries += 1;
      await call("connect", {});
      if (!cancelled) timer = window.setTimeout(() => void tick(), 1500);
    };
    let timer = window.setTimeout(() => void tick(), 0);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [sessionId, hydrated, state.bindingId, state.connected, state.connectionStatus, state.backend, state.selectedProfile,
    state.previewOnly, panelVisible, call, promoting]);
  const action = useCallback(
    (name: string, extra: Record<string, unknown> = {}) => {
      if (!promoting) void call("action", { action: name, ...extra });
    },
    [call, promoting],
  );
  const handleReload = async () => {
    if (promoting || reloading || !state.connected || agentDriving) return;
    const generation = sessionGeneration.current;
    const binding = stateRef.current.bindingId;
    setReloading(true);
    try {
      await call("action", { action: "reload" });
    } finally {
      if (generation === sessionGeneration.current && binding === stateRef.current.bindingId) setReloading(false);
    }
  };
  const handleCopyUrl = async (e: MouseEvent) => {
    e.stopPropagation();
    if (!pageUrl) return;
    setCopied(false);
    const binding = stateRef.current.bindingId;
    const generation = sessionGeneration.current;
    try {
      if (!navigator.clipboard) throw new Error("Clipboard unavailable");
      await navigator.clipboard.writeText(pageUrl);
      if (binding !== stateRef.current.bindingId || generation !== sessionGeneration.current) return;
      operationError.current = "";
      setError(stateRef.current.error || "");
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      if (binding !== stateRef.current.bindingId || generation !== sessionGeneration.current) return;
      operationError.current = "Could not copy the URL. Copy it from the address bar.";
      setError(operationError.current);
    }
  };
  const sendInput = useCallback((name: string, extra: Record<string, unknown> = {}) => {
    const { bindingId: binding, controlGeneration } = stateRef.current;
    if (!sessionId || !binding || promoting || controlTransfer.current) return;
    const move = name === "mousemove" ? ++moveSequence.current : 0;
    const generation = sessionGeneration.current;
    inputQueue.current = inputQueue.current.catch(() => {}).then(() => {
      if (generation !== sessionGeneration.current || currentSessionRef.current !== sessionId || stateRef.current.bindingId !== binding
          || stateRef.current.lockedBy !== "user" || stateRef.current.controlGeneration !== controlGeneration
          || (stateRef.current.stateUnavailable && name !== "mouseup")
          || (move && move !== moveSequence.current)) return;
      return api(`/api/sessions/${encodeURIComponent(sessionId)}/browser/action`, {
      method: "POST",
      body: JSON.stringify({ action: name, ...extra, bindingId: binding, controlGeneration }),
      });
    }).catch((err) => {
      if (useAppStore.getState().sessionId === sessionId && stateRef.current.bindingId === binding) {
        operationError.current = err instanceof Error ? err.message : String(err);
        setError(operationError.current);
      }
    });
  }, [sessionId, promoting]);
  const promote = (target: string) => {
    if (promoting || stateRef.current.stateUnavailable) return;
    const userBackend = ["comet", "chrome", "cdp"].includes(target.replace(/^profile:/, ""));
    const generation = sessionGeneration.current;
    stateRead.current?.controller.abort();
    stateRead.current = null;
    setPromoting(target);
    const request = target.startsWith("session:")
      ? { browserSession: target.slice("session:".length) }
      : { target, url: state.url, consent: userBackend };
    void (async () => {
      try {
        if (state.previewOnly || state.backend === "rust") {
          const heldByAgent = stateRef.current.lockedBy === "agent";
          const taken = heldByAgent ? await call("lease", { action: "take" }) : undefined;
          if (heldByAgent && (taken?.lockedBy !== "user" || generation !== sessionGeneration.current)) return;
          await call("promote", request);
          // A profile choice borrows control for this switch. An explicit user
          // takeover or a newer lease stays with its owner.
          const current = stateRef.current;
          if (taken?.backend === "rust" && taken.controlGeneration != null &&
              generation === sessionGeneration.current && current.lockedBy === "user" &&
              current.controlGeneration === taken.controlGeneration) {
            await call("lease", { action: "return" });
          }
          return;
        }
        if (stateRef.current.lockedBy !== "user") {
          const taken = await call("lease", { action: "take" });
          if (!taken || taken.lockedBy !== "user") return;
        }
        if (generation !== sessionGeneration.current) return;
        await call("promote", request);
      } finally {
        if (generation === sessionGeneration.current) setPromoting("");
      }
    })();
  };
  const openRecoveryTab = async () => {
    if (promoting || state.agentLocked || !state.connected) return;
    const generation = sessionGeneration.current;
    stateRead.current?.controller.abort();
    stateRead.current = null;
    setPromoting("recovery-tab");
    try {
      if (stateRef.current.lockedBy !== "user") {
        const taken = await call("lease", { action: "take" });
        if (taken?.lockedBy !== "user") return;
      }
      if (generation === sessionGeneration.current) {
        await call("action", { action: "tab_new", recovery: true });
      }
    } finally {
      if (generation === sessionGeneration.current) setPromoting("");
    }
  };
  const poppedOut = panelMode === "popup";
  const openOutside = () => {
    const kind = openOutsideKind(state.backend, state.owned);
    if (kind === "comet" || kind === "chrome") {
      action("focus");
      return;
    }
    setPanelMode(poppedOut ? "expanded" : "popup");
  };
  const setDriver = async (who: "agent" | "you") => {
    if (promoting || controlTransfer.current) return;

    if (who === "you") pinWorkspaceSurface("live");
    const generation = sessionGeneration.current;
    controlTransfer.current = true;
    setControlChanging(true);
    try {
      // Transfer ownership without changing the external page's viewport.
      // Always send an explicit return: local state can lag a server restart.
      await call("lease", { action: who === "you" ? "take" : "return" });
    } finally {
      if (generation === sessionGeneration.current) {
        controlTransfer.current = false;
        setControlChanging(false);
      }
    }
  };
  const fitPageToPanel = () => {
    if (promoting) return;
    layoutRequest.current = "";
    setPreviewZoom("responsive");
    setDeviceMode("desktop");
    setPanelFit(value => value + 1);
  };

  useEffect(() => {
    const box = previewBoxRef.current;
    if (!box) return;
    const update = () => {
      const { clientWidth: width, clientHeight: height } = box;
      setPreviewBoxSize(previous => previous.width === width && previous.height === height
        ? previous : { width, height });
    };
    const observer = new ResizeObserver(update);
    observer.observe(box);
    update();
    return () => observer.disconnect();
  }, [sessionId, state.connected, deviceMode, panelMode]);

  const hasViewport = Boolean(state.viewport);
  useEffect(() => {
    if (!panelVisible || !state.connected || !state.bindingId || state.previewOnly || state.stateUnavailable
        || state.novncUrl || showIframe || promoting || actionNeedsReview
        || state.selectedProfile === "machine"
        || (state.backend === "rust" && (!hasViewport || agentDriving))
        || ["machine", "docker", "remote", "lightpanda", "embedded"].includes(state.backend)) return;
    const responsive = previewZoom === "responsive";
    if (responsive && (previewBoxSize.width < 120 || previewBoxSize.height < 120)) return;
    const size = panelViewportFor({ width: previewBoxSize.width, height: previewBoxSize.height });
    const dpr = hdEnabled ? Math.min(2, typeof window !== "undefined" ? window.devicePixelRatio || 2 : 2) : 1;
    const key = `${state.bindingId}:${responsive ? `${size.width}x${size.height}:${panelFit}:${dpr}` : "native"}`;
    if (layoutRequest.current === key || (!responsive && !layoutRequest.current)) return;
    const timer = window.setTimeout(() => {
      layoutRequest.current = key;
      void call("layout", { mode: responsive ? "panel" : "native", ...(responsive ? size : {}), ...(dpr > 1 ? { dpr } : {}) });
    }, 280);
    return () => window.clearTimeout(timer);
  }, [panelVisible, state.connected, state.bindingId, state.novncUrl, state.backend, state.selectedProfile,
      promoting, deviceMode, showIframe, actionNeedsReview, previewZoom, panelFit, previewBoxSize.width, previewBoxSize.height,
      state.previewOnly, state.stateUnavailable, agentDriving, hasViewport, hdEnabled, call]);

  const targetChanged = Boolean(
    state.bindingId && (
      state.targetIdentity
        ? paintedTarget !== state.targetIdentity
        : paintedBinding !== state.bindingId
    )
  );

  const canDrivePage = state.connected && !state.previewOnly && !state.stateUnavailable && !controlChanging && !promoting && state.lockedBy === "user" && !state.agentLocked
    && !streamFailed && (!liveStreamUrl || streamReady)
    && (state.backend !== "rust" || Boolean(state.viewport && state.controlGeneration && streamReady))
    && Boolean(state.bindingId && !targetChanged);
  const pointerButton = (event: MouseEvent<HTMLElement>) =>
    event.button === 2 ? "right" : event.button === 1 ? "middle" : "left";
  const withinViewport = ({ x, y }: { x: number; y: number }) =>
    x >= 0 && y >= 0 && x < (state.viewport?.width ?? 0) && y < (state.viewport?.height ?? 0);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!sessionId || (!state.connected && !state.frame && !liveStreamUrl)) return;
      const modifier = event.metaKey || event.ctrlKey;
      const previewFocused = canDrivePage && document.activeElement === previewRef.current;
      if (modifier && previewFocused && event.key.toLowerCase() === "l") {
        event.preventDefault();
        addressRef.current?.focus();
        addressRef.current?.select();
        return;
      }
      if (modifier && previewFocused && event.key.toLowerCase() === "r") {
        event.preventDefault();
        action("reload");
        return;
      }
      if (
        !canDrivePage ||
        event.defaultPrevented || event.isComposing ||
        document.activeElement !== previewRef.current ||
        !(event.target instanceof HTMLElement) ||
        event.target.isContentEditable ||
        ["INPUT", "TEXTAREA", "SELECT"].includes(event.target.tagName)
      ) {
        return;
      }
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      event.preventDefault();
      if (event.key.length === 1) {
        sendInput("type", { text: event.key });
      } else {
        sendInput("press", { key: event.key });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [action, canDrivePage, liveStreamUrl, panelMode, sendInput, sessionId, state.connected, state.frame]);

  const onPreviewMouseDown = (event: MouseEvent<HTMLElement>) => {
    if (!canDrivePage) return;
    event.preventDefault();
    previewRef.current?.focus();
    const { x, y } = previewPoint(event);
    if (state.backend === "rust") {
      if (!withinViewport({ x, y })) return;
      rustClick.current = { binding: state.bindingId!, generation: state.controlGeneration!, x, y, button: pointerButton(event) };
      return;
    }
    pointerDown.current = true;
    if (sessionId && stateRef.current.bindingId)
      heldPointer.current = { session: sessionId, binding: stateRef.current.bindingId, button: pointerButton(event), x, y };
    lastMoveAt.current = performance.now();
    lastMovePt.current = { x, y };
    sendInput("mousedown", { x, y, button: pointerButton(event) });

    const boxRect = previewBoxRef.current?.getBoundingClientRect();
    if (boxRect) {
      const clickX = event.clientX - boxRect.left;
      const clickY = event.clientY - boxRect.top;
      const id = Date.now() + Math.random();
      setUserClicks((prev) => [...prev.slice(-3), { id, x: clickX, y: clickY }]);
      window.setTimeout(() => {
        setUserClicks((prev) => prev.filter((c) => c.id !== id));
      }, 550);
    }
  };

  const onPreviewMouseMove = (event: MouseEvent<HTMLElement>) => {
    if (!canDrivePage || state.backend === "rust") return;
    const { x, y } = previewPoint(event);
    if (
      !shouldSamplePointer({
        now: performance.now(),
        lastAt: lastMoveAt.current,
        x,
        y,
        lastX: lastMovePt.current.x,
        lastY: lastMovePt.current.y,
      })
    ) {
      return;
    }
    lastMoveAt.current = performance.now();
    lastMovePt.current = { x, y };
    if (heldPointer.current) Object.assign(heldPointer.current, { x, y });
    sendInput("mousemove", { x, y });
  };

  const onPreviewMouseUp = (event: MouseEvent<HTMLElement>) => {
    if (state.backend === "rust") {
      const click = rustClick.current;
      rustClick.current = null;
      if (!canDrivePage || !click || click.binding !== state.bindingId || click.generation !== state.controlGeneration) return;
      const { x, y } = previewPoint(event);
      if (withinViewport({ x, y }) && Math.hypot(click.x - x, click.y - y) <= 8 && click.button === pointerButton(event)) {
        sendInput("click", { x, y, button: click.button });
      }
      return;
    }
    if (!canDrivePage && !pointerDown.current) return;
    const { x, y } = previewPoint(event);
    pointerDown.current = false;
    heldPointer.current = null;
    sendInput("mouseup", { x, y, button: pointerButton(event) });
  };

  useEffect(() => {
    const release = releasePointer;
    window.addEventListener("mouseup", release);
    window.addEventListener("blur", release);
    window.addEventListener("pointercancel", release);
    return () => {
      release();
      window.removeEventListener("mouseup", release);
      window.removeEventListener("blur", release);
      window.removeEventListener("pointercancel", release);
    };
  }, [releasePointer]);

  const onPreviewWheel = (event: WheelEvent<HTMLElement>) => {
    if (!state.connected || !canDrivePage) return;
    event.preventDefault();
    const { x, y } = previewPoint(event);
    if (state.backend === "rust" && !withinViewport({ x, y })) return;
    sendInput("wheel", { x, y, deltaX: event.deltaX, deltaY: event.deltaY });
  };

  useEffect(() => {
    const image = previewRef.current;
    if (!image) return;
    const observer = new ResizeObserver(() => measurePreview());
    observer.observe(image);
    measurePreview();
    return () => observer.disconnect();
  }, [measurePreview, state.frame?.url, state.frameVersion, previewZoom, previewBoxSize,
    state.viewport?.width, state.viewport?.height, state.screencast?.deviceWidth, state.screencast?.deviceHeight]);

  const embeddedHost = isEmbeddedHost();
  const embeddedGuest = showEmbeddedGuest({
    embeddedHost,
    backend: state.backend,
    selectedBackend: state.selectedBackend,
    connected: state.connected,
    novnc: state.novncUrl,
  });
  const controlling = state.lockedBy === "user" && !state.previewOnly;
  const showSessionSelect = !machineActive && !["machine", "remote", "docker"].includes(state.backend);
  // Choosing a profile is an explicit user takeover. Keep the picker usable
  // during agent control; promote() acquires the fenced user lease first.
  const sessionSelectDisabled = controlChanging || Boolean(promoting) || Boolean(state.stateUnavailable);
  useEffect(() => {
    setSurfaceControlling(controlling);
  }, [controlling, setSurfaceControlling]);
  useEffect(() => {
    const next = state.novncUrl || machineNovncUrl;
    if (next) lastNovncRef.current = next;
    if (!machineActive) lastNovncRef.current = "";
  }, [state.novncUrl, machineNovncUrl, machineActive]);
  useEffect(() => {
    const desktop = window.kageha;
    if (!desktop?.setGuestBounds) return;
    const box = previewBoxRef.current;
    if (!embeddedGuest || !box) {
      desktop.setGuestBounds({ x: 0, y: 0, width: 0, height: 0 });
      return;
    }
    const publish = () => {
      const rect = box.getBoundingClientRect();
      desktop.setGuestBounds({ x: rect.x, y: rect.y, width: rect.width, height: rect.height });
    };
    publish();
    const observer = new ResizeObserver(publish);
    observer.observe(box);
    window.addEventListener("resize", publish);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", publish);
    };
  }, [embeddedGuest, panelMode, state.connected, state.backend]);
  useEffect(() => {
    window.kageha?.setGuestInteractive?.(embeddedGuest && controlling && !state.agentLocked);
  }, [embeddedGuest, controlling, state.agentLocked]);
  const activeTabKey = state.tabs.find(tab => tab.active)?.tabHandle
    ?? state.tabs.find(tab => tab.active)?.index;
  const requestedBrowser = useRequestedBrowserTarget(sessionId);
  const transfer = useRef<() => void>(() => {});
  transfer.current = () => {
    if (stateRef.current.connected && !stateRef.current.stateUnavailable && !stateRef.current.previewOnly &&
      stateRef.current.canControl !== false && stateRef.current.sessionId === sessionId) {
      void setDriver(stateRef.current.lockedBy === "user" ? "agent" : "you");
    }
  };
  const controlsConnected = Boolean(hydrated && state.connected && state.sessionId === sessionId &&
    !state.stateUnavailable && !state.previewOnly && state.canControl !== false && state.tabs.length);
  const controlsBusy = controlChanging || Boolean(promoting);
  useEffect(() => {
    if (!embeddedInWorkspace || !sessionId) return;
    publishBrowserControls({
      sessionId, connected: controlsConnected, controlling, busy: controlsBusy, error,
      transfer: () => transfer.current(),
    });
    return () => publishBrowserControls(null);
  }, [embeddedInWorkspace, sessionId, controlsConnected, controlling, controlsBusy, error]);
  const requestedTarget = requestedBrowser?.target;
  const requestedTab = requestedTarget && state.targetIdentity?.startsWith(`rust:${requestedTarget.browserId}:`)
    ? state.tabs.find(tab => tab.tabHandle === requestedTarget.tabId) : undefined;
  useEffect(() => {
    // Follow new active targets once, without fighting a user's strip scrolling
    // on every status poll.
    if (!panelVisible) return;
    tabListRef.current?.querySelector<HTMLElement>('[aria-selected="true"]')
      ?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [activeTabKey, state.bindingId, hydrated, panelVisible, previewBoxSize.width]);

  const targetUnavailable = state.connectionStatus === "target_unavailable";
  if (sessionId && (!hydrated || state.connectionStatus === "restoring" || targetUnavailable)) return (
    <section className="relative flex min-h-0 flex-1 flex-col bg-canvas" aria-label="Browser">
      {(targetUnavailable) && (
        <BrowserProfileBar state={state} showSelect={showSessionSelect} disabled={sessionSelectDisabled}
          onChange={promote} onReconnect={() => void refresh()} />
      )}
      {state.pendingBrowserActions?.length ? <BrowserActionNotice key={sessionId} actions={state.pendingBrowserActions} hold={state.browserActionHold} /> : null}
      {targetUnavailable || error ? (
        <div className="grid min-h-0 flex-1 place-items-center overflow-auto px-4 py-6">
          <div className="flex w-full max-w-xs flex-col items-center gap-3 text-center">
            <div role="status" className="flex flex-col items-center gap-2">
              <span className="grid size-9 place-items-center rounded-full bg-warn-soft text-warn">
                <Icon.Alert size={18} />
              </span>
              <p className="text-sm font-semibold text-ink">{targetUnavailable ? "Task tab unavailable" : "Browser connection interrupted"}</p>
              <p className="text-xs text-muted">Retry to reconnect to this chat’s browser tab.</p>
            </div>
            <button type="button" className="min-h-9 rounded-md border border-line bg-surface px-3 text-sm font-medium text-ink hover:bg-surface-hover"
              onClick={() => void refresh()}>Retry browser connection</button>
            {error && <details className="max-w-full text-xs text-muted">
              <summary className="cursor-pointer">Connection details</summary>
              <p className="mt-1 max-h-24 overflow-auto break-all">{error}</p>
            </details>}
          </div>
        </div>
      ) : (
        <StageBoot title="Connecting to this chat’s browser" sub="Restoring the existing browser view…" />
      )}
    </section>
  );
  if (!sessionId) {
    const idle = browserBootCopy({ kind: "idle" });
    return (
      <section className="flex min-h-0 flex-1 bg-canvas" aria-label="Browser">
        <StageBoot title={idle.title} sub={idle.sub} />
      </section>
    );
  }
  const rawFrameUrl = state.bindingId
    ? (state.frame?.bindingId === state.bindingId ? state.frame.url || "" : "")
    : "";
  const incomingFrameUrl = rawFrameUrl
    ? `${rawFrameUrl}${rawFrameUrl.includes("?") ? "&" : "?"}ui=${state.frameVersion || 0}`
    : "";
  const frameUrl = incomingFrameUrl || paintedUrl;
  const maximized = panelMode === "fullscreen";
  const extraTabs = state.tabs.filter((tab) => !tab.active);
  const chromeLocked = controlChanging || (agentDriving && !controlling);
  const novncSrc = state.novncUrl || machineNovncUrl || (hasSessionState ? lastNovncRef.current : "");
  // Only embed a real absolute URL. `novncSrc` can be the "ready" sentinel
  // (screen is up but the URL isn't known yet); a relative iframe src would
  // resolve against the app origin and load Kageha inside itself.
  const novncEmbedSrc = isEmbeddableNovncUrl(novncSrc) ? novncSrc : "";

  const pageUrl = address || state.url || "";
  const viewFailed = state.backend === "lightpanda" && streamFailed;
  const boot = browserBootCopy({
    kind: viewFailed ? "failed" : blankPage || !(state.connected || runStatus === "running") ? "idle" : "opening",
    url: blankPage ? "" : pageUrl,
    title: state.title,
    browserStage: true,
  });
  const externalBrowser = isExternalBrowser(state.backend, state.owned);
  const outsideTitle = openOutsideTitle(state.backend, state.owned);
  const dockedBrowser = !poppedOut && !maximized;
  const consentOpen = needsConsentChoice(state);
  const pageViewport = screencastViewport(state);
  const retainedStream = panelVisible && !targetChanged && Boolean(paintedStream);
  const rasterCanvas = !showIframe && !embeddedGuest && !novncEmbedSrc && !blankPage && Boolean(liveStreamUrl || retainedStream || frameUrl);
  const canvasSize = previewZoom === "responsive" ? previewBoxSize : previewZoom === "fit-width" && previewBoxSize.width > 0
    ? {
        width: previewBoxSize.width,
        height: Math.round((pageViewport.height * previewBoxSize.width) / Math.max(1, pageViewport.width)),
      }
    : browserCanvasSize(pageViewport, previewBoxSize, previewZoom === "fit-width" ? "fit" : previewZoom);
  const highlightBox =
    state.highlight && previewMetrics
      ? highlightRectFromFit({
          highlight: state.highlight,
          boxWidth: previewMetrics.boxW,
          boxHeight: previewMetrics.boxH,
          naturalWidth: previewMetrics.natW || pageViewport.width,
          naturalHeight: previewMetrics.natH || pageViewport.height,
          viewport: pageViewport,
          fit: "contain",
        })
      : null;

  return (
    <section
      className={cn(
        "browser-stage flex min-h-0 flex-col overflow-hidden bg-canvas",
        dockedBrowser && "browser-stage-docked",
        panelMode === "fullscreen"
          ? "fixed inset-0 z-50 h-screen w-screen border-0"
          : poppedOut
            ? "browser-stage-popup fixed bottom-4 right-4 z-40 h-[min(720px,calc(100vh-2rem))] w-[min(640px,calc(100vw-2rem))] rounded-2xl border border-line"
            : "relative flex-1",
      )}
      aria-label="Browser"
    >
      <header className={cn("browser-control-bar", blankPage && !state.connected && "sr-only")} aria-label="Browser control">
          <div className="browser-control-status" role="status">
            <span className="browser-status-dot" aria-hidden="true" />
            <span>{promoting ? "Switching browser…" : state.stateUnavailable ? "Waiting for browser…" : !state.connected ? "Browser disconnected" : state.layout?.pending ? "Layout update queued" : retainedStream && !streamReady ? "Reconnecting preview…" : state.inputStatus?.ready === false ? "Browser input unavailable" : controlling ? "You have control" : rustAccountHandoff ? "Waiting for you" : actionNeedsReview ? "Action needs review" : blankPage ? "No page open" : "Browser ready"}</span>
            {panelVisible && state.backend === "rust" && state.connected && !state.stateUnavailable
              && !blankPage && !streamFailed && state.bindingId && !targetChanged
              && (!liveStreamUrl || streamReady) && <span>Live preview</span>}

          </div>
          <div className="flex shrink-0 items-center gap-2">
          <BrowserDecisionRoute route={state.decisionRoute} />
          {Boolean(state.pendingBrowserActions?.length) && !actionNeedsReview && <Popover.Root>
            <Popover.Trigger asChild><button type="button" className="stage-icon-btn text-warn" aria-label="Review action details" title="An earlier action on another tab needs review"><Icon.Alert size={14} /></button></Popover.Trigger>
            <Popover.Portal><Popover.Content className="browser-view-menu browser-review-menu" aria-label="Earlier browser action" side="bottom" align="end" sideOffset={8}>
              <BrowserActionNotice actions={state.pendingBrowserActions!} hold={state.browserActionHold} defaultExpanded />
            </Popover.Content></Popover.Portal>
          </Popover.Root>}
          {state.previewOnly ? <span className="text-xs text-muted">Preview only</span> : <button type="button" className="browser-control-button" aria-busy={controlChanging} disabled={controlChanging || Boolean(promoting) || !state.connected || state.stateUnavailable || state.canControl === false}
            onClick={() => setDriver(controlling ? "agent" : "you")}>
            {controlChanging ? "Transferring…" : controlling ? "Return control to agent" : "Take control"}
          </button>}
          </div>
        </header>
      {state.connected && state.inputStatus?.ready === false && (
        <p role="status" className="browser-runtime-notice">
          <Icon.Alert size={13} aria-hidden="true" />
          <span>Browser input is paused because action history cannot be saved. Page reads and live preview remain available. Input resumes after storage recovers.</span>
        </p>
      )}
      {(state.runtimeRecovery || state.runtimeCanary?.unverifiedInput) && (
        <p role="status" className="browser-runtime-notice">
          <Icon.Alert size={13} aria-hidden="true" />
          <span>{state.runtimeCanary?.unverifiedInput ? "An earlier action needs checking" : state.runtimeRecovery}</span>
        </p>
      )}
      {state.pendingBrowserActions?.length && actionNeedsReview ? (
        <BrowserActionNotice key={sessionId} actions={state.pendingBrowserActions}
          hold={state.browserActionHold}
          onNewTab={state.connected && !state.agentLocked && !state.previewOnly && state.selectedProfile !== "embedded"
            ? () => void openRecoveryTab() : undefined}
          switching={Boolean(promoting)} />
      ) : null}
      {error && state.bindingId ? (
        <details className="shrink-0 border-b border-line px-3 py-1 text-xs text-muted">
          <summary className="cursor-pointer">{operationError.current ? "Action details" : "Browser connection details"}</summary>
          <dl className="my-2 space-y-1 break-all">
            <div><dt className="inline font-medium">Browser: </dt><dd className="inline">{browserProfileLabel(state.backend)}</dd></div>
            <div><dt className="inline font-medium">Page: </dt><dd className="inline">{state.title || state.url || "Unavailable"}</dd></div>
            <div><dt className="inline font-medium">Target: </dt><dd className="inline font-mono">{state.targetIdentity || "Unavailable"}</dd></div>
            {state.expectedTargetIdentity && <div><dt className="inline font-medium">Task target: </dt><dd className="inline font-mono">{state.expectedTargetIdentity}</dd></div>}
            <div><dt className="inline font-medium">View binding: </dt><dd className="inline font-mono">{state.bindingId}</dd></div>
          </dl>
        </details>
      ) : null}
      {state.selection && !promoting && controlling && (
        <p role="status" className="flex shrink-0 items-center gap-2 border-b border-line bg-surface px-3 py-1.5 text-xs text-muted">
          <Icon.Check size={13} className="shrink-0 text-accent" />
          <span>Agent will follow {browserProfileLabel(state.selection.backend)} when you return control</span>

        </p>
      )}
      {promoting && <p role="status" className="shrink-0 border-b border-line bg-surface px-3 py-2 text-xs text-muted">{promoting === "recovery-tab" ? "Opening a new tab…" : "Switching browser session…"}</p>}
      {rustAccountHandoff ? (
        <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-line bg-surface px-3 py-1.5 text-xs text-muted">
          <span role="status" className="flex items-center gap-2"><Icon.Shield size={13} className="shrink-0" />{onAccountTab ? "Complete the account step here" : "Return to the account tab"}</span>
          <button type="button" className="min-h-8 rounded px-2 font-medium text-ink hover:bg-surface-hover"
            onClick={() => useAppStore.getState().setPrefs({ rightPanelCollapsed: true })}>Back to chat to resume</button>
        </div>
      ) : state.loginAccess?.status && state.loginAccess.status !== "active" && (
        <div className="flex flex-wrap items-center justify-between gap-x-2 border-b border-line bg-surface px-3 py-1 text-xs text-muted">
          <span className="flex items-center gap-2"><Icon.Shield size={13} className="shrink-0" />{state.loginAccess.status === "waiting" ? "Sign in here, then return to chat to approve access" : "Agent browser access stopped"}</span>
          {state.loginAccess.status === "waiting" && <button type="button" className="min-h-8 rounded px-2 font-medium text-ink hover:bg-surface-hover" onClick={() => useAppStore.getState().setPrefs({ rightPanelCollapsed: true })}>Back to chat to approve</button>}
          {state.loginAccess.status === "waiting" && <button type="button" className="min-h-8 rounded px-2 font-medium text-ink hover:bg-surface-hover" onClick={() => void revokeLogin()}>Revoke access</button>}
        </div>
      )}
      <div className="browser-tab-row">
        <div className="flex items-center gap-1.5 pl-2.5 pr-2 py-1 shrink-0 select-none" aria-hidden="true">
          <span className="traffic-light traffic-light-red" />
          <span className="traffic-light traffic-light-yellow" />
          <span className="traffic-light traffic-light-green" />
        </div>
        {state.tabs.length > 0 ? (
          <div ref={tabListRef} className="browser-stage-tabstrip" role="tablist" aria-label="Browser tabs">
            {state.tabs.map((tab) => (
              <button
                key={tab.tabHandle || tab.index}
                type="button"
                role="tab"
                aria-selected={tab.active}
                disabled={agentDriving || controlChanging || Boolean(promoting)}
                title={tab.url || tab.title}
                className={cn("browser-stage-tab group", tab.active && "is-active")}
                onClick={async () => {
                  if (!controlling && !state.previewOnly) await setDriver("you");
                  action("tab_select", tabTarget(tab));
                }}
              >
                <Icon.Globe size={11} className="mr-1.5 shrink-0 opacity-80" />
                <span className="truncate">{tab.active ? formatTabTitle(state.title || tab.title, pageUrl || tab.url) : formatTabTitle(tab.title, tab.url || `Tab ${tab.index + 1}`)}</span>
                {state.tabs.length > 1 && (
                  <span
                    role="button"
                    tabIndex={-1}
                    aria-label="Close tab"
                    className="ml-1 -mr-1 size-3.5 rounded-full inline-flex items-center justify-center opacity-40 hover:opacity-100 hover:bg-line/60 transition-opacity"
                    onClick={(e) => {
                      e.stopPropagation();
                      action("tab_close", tabTarget(tab));
                    }}
                  >
                    <Icon.Close size={9} />
                  </span>
                )}
              </button>
            ))}
          {controlling && extraTabs.length > 0 && <button type="button" className="stage-icon-btn"
            disabled={controlChanging || Boolean(promoting)} aria-label="Close tab" title="Close current tab"
            onClick={() => { const tab = state.tabs.find(tab => tab.active); if (tab) action("tab_close", tabTarget(tab)); }}>
            <Icon.Close size={13} />
          </button>}
          {state.selectedProfile !== "embedded" && <button type="button" disabled={controlChanging || Boolean(promoting)} className="stage-icon-btn hover:bg-surface-hover hover:text-ink transition-colors ml-0.5" onClick={async () => {
            if (!controlling && !state.previewOnly) await setDriver("you");
            action("tab_new");
          }} aria-label="New tab" title="New tab">
            <Icon.Plus size={14} />
          </button>}
        </div>
      ) : (
        <div ref={tabListRef} className="browser-stage-tabstrip" role="tablist" aria-label="Browser tabs">
          <button
            type="button"
            role="tab"
            aria-selected={true}
            className="browser-stage-tab is-active"
          >
            <Icon.Globe size={11} className="mr-1.5 shrink-0 opacity-80" />
            <span className="truncate">New Tab</span>
          </button>
          {state.selectedProfile !== "embedded" && <button type="button" disabled={controlChanging || Boolean(promoting)} className="stage-icon-btn hover:bg-surface-hover hover:text-ink transition-colors ml-0.5" onClick={async () => {
            if (!controlling && !state.previewOnly) await setDriver("you");
            action("tab_new");
          }} aria-label="New tab" title="New tab">
            <Icon.Plus size={14} />
          </button>}
        </div>
      )}
        <BrowserProfileBar state={state} showSelect={showSessionSelect} disabled={sessionSelectDisabled}
          hidden={blankPage}
          onChange={promote} onReconnect={() => {
            if (state.backend === "rust" && (state.selectedProfile === "chrome" || state.selectedProfile === "comet")) {
              promote(`profile:${state.selectedProfile}`);
            } else {
              void call("connect", {});
            }
          }} />
        {state.canClaimUserTabs && <Popover.Root onOpenChange={(open) => {
          setTabCandidates(undefined);
          if (open) void call("action", { action: "list_user_tabs" }).then(setTabCandidates);
        }}>
          <Popover.Trigger asChild>
            <button type="button" className="browser-control-button" disabled={state.agentLocked || state.stateUnavailable || Boolean(promoting)}>
              Use open tab
            </button>
          </Popover.Trigger>
          <Popover.Portal><Popover.Content className="browser-view-menu" aria-label="Open tabs in this profile" sideOffset={8} align="end">
            {!tabCandidates ? <p>Loading open tabs…</p>
              : tabCandidates.bindingId !== state.bindingId ? <p>Reopen this menu to refresh the list.</p>
              : !tabCandidates.userTabCandidates?.length ? <p>No other tabs are available in this profile.</p>
              : tabCandidates.userTabCandidates.map(tab => <Popover.Close asChild key={tab.tabHandle}>
                <button type="button" className="browser-control-button block max-w-72 truncate" title={tab.url}
                  onClick={() => void call("action", { action: "claim_user_tab", ...tab, candidateBindingId: tabCandidates.bindingId })}>
                  {tab.title || tab.url}
                </button>
              </Popover.Close>)}
          </Popover.Content></Popover.Portal>
        </Popover.Root>}
        {state.loginAccess?.status === "active" && <Popover.Root>
          <Popover.Trigger asChild><button type="button" className="stage-icon-btn" aria-label="Browser access" title="Browser access"><Icon.Shield size={14} /></button></Popover.Trigger>
          <Popover.Portal><Popover.Content className="browser-view-menu" aria-label="Browser access" sideOffset={8} align="end">
            <p className="font-medium text-ink">Agent has browser access</p>
            <p className="text-muted">Website sign-in and cookies come from the selected profile.</p>
            <button type="button" className="browser-control-button" onClick={() => void revokeLogin()}>Revoke access</button>
          </Popover.Content></Popover.Portal>
        </Popover.Root>}
      </div>
      <div className="stage-hud-wrap">
          <div className="stage-hud browser-stage-hud">
            {state.previewOnly ? null : controlling ? (
              <div className="browser-stage-nav" aria-label="Browser navigation">
                <button
                  type="button"
                  className="stage-icon-btn hover:bg-surface-hover hover:text-ink transition-colors active:scale-95"
                  onClick={() => action("back")}
                  disabled={!state.connected || chromeLocked}
                  aria-label="Back"
                  title="Back"
                >
                  <Icon.Back size={14} />
                </button>
                <button
                  type="button"
                  className="stage-icon-btn hover:bg-surface-hover hover:text-ink transition-colors active:scale-95"
                  onClick={() => action("forward")}
                  disabled={!state.connected || chromeLocked}
                  aria-label="Forward"
                  title="Forward"
                >
                  <Icon.Forward size={14} />
                </button>
                <button
                  type="button"
                  className="stage-icon-btn hover:bg-surface-hover hover:text-ink transition-colors active:scale-95"
                  onClick={() => void handleReload()}
                  disabled={!state.connected || chromeLocked || Boolean(promoting) || reloading}
                  aria-busy={reloading}
                  aria-label="Reload"
                  title="Reload"
                >
                  <Icon.Reload size={13} className={cn(reloading && "animate-spin")} />
                </button>
              </div>
            ) : (!controlling) ? (
              <div className="browser-stage-nav" aria-label="Browser navigation">
                <span className="stage-icon-btn opacity-30 cursor-not-allowed inline-flex items-center justify-center" title="Back">
                  <Icon.Back size={14} />
                </span>
                <span className="stage-icon-btn opacity-30 cursor-not-allowed inline-flex items-center justify-center" title="Forward">
                  <Icon.Forward size={14} />
                </span>
                <button
                  type="button"
                  className="stage-icon-btn hover:bg-surface-hover hover:text-ink transition-colors active:scale-95"
                  onClick={() => void handleReload()}
                  disabled={!state.connected || agentDriving || Boolean(promoting) || reloading}
                  aria-busy={reloading}
                  aria-label="Reload"
                  title="Reload"
                >
                  <Icon.Reload size={13} className={cn(reloading && "animate-spin")} />
                </button>
              </div>
            ) : null}
            <StageIdentity live={agentRunning && !controlling}>
              <div className="flex items-center gap-1 shrink-0">
                {pageProtocolInfo(pageUrl).kind === "https" ? (
                  <span className="flex items-center gap-1 rounded bg-emerald-500/10 px-1 py-0.5 text-[0.6rem] font-medium text-emerald-500" title="Secure HTTPS connection">
                    <Icon.Lock size={10} className="text-emerald-500 shrink-0" />
                  </span>
                ) : pageProtocolInfo(pageUrl).kind === "localhost" ? (
                  <span className="flex items-center gap-1 rounded bg-amber-500/10 px-1.5 py-0.5 text-[0.6rem] font-medium text-amber-500" title="Localhost server">
                    <Icon.Sparkles size={9} className="text-amber-500 shrink-0" />
                    <span>local</span>
                  </span>
                ) : pageProtocolInfo(pageUrl).kind === "file" ? (
                  <span className="flex items-center gap-1 rounded bg-sky-500/10 px-1.5 py-0.5 text-[0.6rem] font-medium text-sky-500" title="Local artifact / file">
                    <Icon.File size={9} className="text-sky-500 shrink-0" />
                    <span>file</span>
                  </span>
                ) : (
                  <Icon.Globe size={12} className="text-muted/80 shrink-0" />
                )}
              </div>
              {state.previewOnly ? (
                <span className="stage-host truncate flex-1">{stageAddressLabel("", blankPage ? "" : pageUrl) || "Ready"}</span>
              ) : controlling ? (
                <input
                  ref={addressRef}
                  value={address !== "" ? address : (blankPage ? "" : pageUrl)}
                  disabled={controlChanging || Boolean(promoting) || !state.connected}
                  onChange={(e) => setAddress(e.target.value)}
                  onFocus={(e) => e.target.select()}
                  onKeyDown={async (e) => {
                    if (e.key === "Enter") {
                      const raw = (address || e.currentTarget.value).trim();
                      const targetUrl = normalizeBrowserUrl(raw);
                      if (targetUrl) {
                        action("navigate", { url: targetUrl });
                        e.currentTarget.blur();
                      }
                    }
                  }}
                  placeholder="Search or enter URL (e.g. bareandfair.ca)…"
                  aria-label="Address"
                  className="min-w-0 flex-1 bg-transparent text-xs text-ink outline-none placeholder:text-muted/60"
                />
              ) : (
                <button
                  type="button"
                  className="stage-host cursor-pointer truncate text-left hover:text-ink transition-colors flex-1"
                  onClick={async () => {
                    await setDriver("you");
                    window.setTimeout(() => {
                      addressRef.current?.focus();
                      addressRef.current?.select();
                    }, 50);
                  }}
                  disabled={controlChanging || Boolean(promoting) || !state.connected}
                  aria-label="Take control to edit address"
                  title="Take control to edit address"
                >
                  {stageAddressLabel("", blankPage ? "" : pageUrl) || (state.novncUrl ? "Watching browser" : blankPage ? "Ready" : "Starting…")}
                </button>
              )}
              {pageUrl && !blankPage && (
                <button
                  type="button"
                  className="ml-auto flex items-center gap-1 rounded px-1.5 py-0.5 text-[0.62rem] text-muted hover:text-ink hover:bg-surface-hover/80 transition-colors cursor-pointer shrink-0"
                  onClick={e => void handleCopyUrl(e)}
                  title={copied ? "Copied URL to clipboard!" : "Copy URL"}
                  aria-label={copied ? "Copied" : "Copy URL"}
                >
                  {copied ? (
                    <>
                      <Icon.Check size={11} className="text-emerald-500" />
                      <span className="text-emerald-500 font-medium">Copied</span>
                    </>
                  ) : (
                    <Icon.Copy size={11} className="opacity-60 hover:opacity-100" />
                  )}
                </button>
              )}
            </StageIdentity>

            <div className="stage-actions">
              {artifactUrl && (
                <div className="flex items-center rounded-md border border-line bg-surface/80 p-0.5 shadow-2xs mr-1 text-xs">
                  <button
                    type="button"
                    onClick={() => setViewMode("iframe")}
                    className={cn(
                      "flex items-center gap-1 rounded px-1.5 py-0.5 text-[0.62rem] transition-colors cursor-pointer",
                      showIframe ? "bg-surface-hover text-ink font-semibold shadow-2xs" : "text-muted hover:text-ink",
                    )}
                    title="Interactive page preview"
                  >
                    <Icon.Sparkles size={11} className="text-amber-500" />
                    <span>Native</span>
                  </button>
                  <button
                    type="button"
                    onClick={() => setViewMode("stream")}
                    className={cn(
                      "flex items-center gap-1 rounded px-1.5 py-0.5 text-[0.62rem] transition-colors cursor-pointer",
                      !showIframe ? "bg-surface-hover text-ink font-semibold shadow-2xs" : "text-muted hover:text-ink",
                    )}
                    title="Remote browser screencast"
                  >
                    <span>Stream</span>
                  </button>
                </div>
              )}
              <Popover.Root>
                <Popover.Trigger asChild><button type="button" className="stage-icon-btn" aria-label="Browser view settings" title="Browser view settings"><Icon.Tune size={15} /></button></Popover.Trigger>
                <Popover.Portal><Popover.Content className="browser-view-menu" aria-label="Browser view settings" sideOffset={8} align="end">
                  <p className="font-medium text-ink">Page view</p>
                  <div className="flex flex-wrap items-center gap-2">
              {!state.previewOnly && !embeddedGuest && !state.novncUrl && state.connected && (
                <button
                  type="button"
                  aria-label="Fit page to panel"
                  disabled={controlChanging || Boolean(promoting)}
                  title="Resize the page to match this panel"
                  className={cn("inline-flex h-7 shrink-0 items-center gap-1 rounded-md border border-line px-2 text-xs text-muted hover:bg-surface-hover", previewZoom === "responsive" && "bg-accent/15 text-accent")}
                  onClick={() => void fitPageToPanel()}
                >
                  <Icon.Maximize size={14} />
                  <span className="browser-fit-label">Fit panel</span>
                </button>
              )}
              {!embeddedGuest && !state.novncUrl && (
                <select
                  aria-label="Preview zoom"
                  title="Preview zoom"
                  value={previewZoom}
                  onChange={(event) => {
                    const val = event.target.value;
                    setPreviewZoom(val === "responsive" || val === "fit" || val === "fit-width" ? val : Number(val));
                  }}
                  className="h-7 rounded-md border border-line bg-surface px-1 text-xs text-muted outline-none focus:ring-1 focus:ring-accent"
                >
                  <option value="responsive">{state.previewOnly ? "Fit panel" : "Responsive"}</option>
                  <option value="fit">Fit page</option>
                  <option value="fit-width">Page width</option>
                  {[50, 75, 100, 125, 150].map((zoom) => <option key={zoom} value={zoom}>{zoom}%</option>)}
                </select>
              )}
              <div
                  className="flex items-center rounded-md border border-line bg-surface/80 p-0.5 shadow-2xs mr-1"
                  role="radiogroup"
                  aria-label="Device view"
                >
                  <button
                    type="button"
                    onClick={() => setDeviceMode("desktop")}
                    className={cn(
                      "flex items-center justify-center rounded px-1.5 py-0.5 text-xs transition-colors cursor-pointer",
                      deviceMode === "desktop"
                        ? "bg-surface-hover text-ink shadow-2xs font-semibold"
                        : "text-muted hover:text-ink",
                    )}
                    title="Desktop view"
                    aria-label="Desktop view"
                    aria-checked={deviceMode === "desktop"}
                    role="radio"
                  >
                    <Icon.Desktop size={13} />
                  </button>
                  <button
                    type="button"
                    onClick={() => setDeviceMode("mobile")}
                    className={cn(
                      "flex items-center justify-center rounded px-1.5 py-0.5 text-xs transition-colors cursor-pointer",
                      deviceMode === "mobile"
                        ? "bg-surface-hover text-ink shadow-2xs font-semibold"
                        : "text-muted hover:text-ink",
                    )}
                    title="Mobile view"
                    aria-label="Mobile view"
                    aria-checked={deviceMode === "mobile"}
                    role="radio"
                  >
                    <Icon.Mobile size={13} />
                  </button>
                </div>
                {!embeddedGuest && !state.novncUrl && (
                  <button
                    type="button"
                    aria-pressed={hdEnabled}
                    onClick={() => setHdEnabled(prev => !prev)}
                    className={cn(
                      "inline-flex h-7 shrink-0 items-center gap-1 rounded-md border border-line px-2 text-xs transition-colors cursor-pointer",
                      hdEnabled
                        ? "bg-accent/15 text-accent border-accent/40 font-semibold shadow-2xs"
                        : "text-muted hover:text-ink hover:bg-surface-hover",
                    )}
                    title={hdEnabled ? "High Definition (Retina 2×) is on" : "Standard Definition (1×) is on"}
                    aria-label="Toggle High Definition"
                  >
                    <Icon.Sparkles size={12} className={hdEnabled ? "text-accent" : "text-muted"} />
                    <span>HD</span>
                  </button>
                )}

                  </div>
                  <p className="text-muted">Responsive resizes the page to this panel. Other choices keep the page size and change its preview.</p>
                  {hasLiveFrame && !blankPage && <p className="text-muted">Page size: {pageViewport.width} × {pageViewport.height}</p>}
                  {state.runtimeCanary && <details className="browser-runtime-options">
                    <summary>Advanced browser options</summary>
                    <p className="text-muted">Browser runtime: {state.runtimeCanary.runtime === "rust" ? "Rust" : "Python"}</p>
                    {(state.runtimeCanary.runtime === "rust" || state.runtimeCanary.canTryRust) &&
                      <button type="button" className="browser-control-button"
                        disabled={Boolean(promoting) || Boolean(state.lockedBy) || state.runtimeCanary.unverifiedInput
                          || runStatus === "running" || runStatus === "waiting_approval"}
                        onClick={() => {
                          const runtime = state.runtimeCanary?.runtime === "rust" ? "python" : "rust";
                          stateRead.current?.controller.abort();
                          stateRead.current = null;
                          setPromoting("runtime");
                          void call("runtime", { runtime }).finally(() => setPromoting(""));
                        }}>
                        {state.runtimeCanary.runtime === "rust" ? "Use Python browser" : "Try Rust browser"}
                      </button>}
                  </details>}
                </Popover.Content></Popover.Portal>
              </Popover.Root>
              {externalBrowser ? (
                <button
                  type="button"
                  className="stage-icon-btn"
                  onClick={openOutside}
                  aria-label="Open outside"
                  title={outsideTitle}
                >
                  <Icon.External size={14} />
                </button>
              ) : null}
              {controlling && !maximized && !dockedBrowser ? (
                <button
                  type="button"
                  className={cn("stage-icon-btn", poppedOut && "is-on")}
                  onClick={() => setPanelMode(poppedOut ? "expanded" : "popup")}
                  aria-label={poppedOut ? "Dock panel" : "Pop out panel"}
                  title={poppedOut ? "Dock panel" : "Pop out panel"}
                >
                  <Icon.Canvas size={14} />
                </button>
              ) : null}
              {(!embeddedInWorkspace || !dockedBrowser) && <button
                  type="button"
                  className="stage-icon-btn"
                  onClick={() => setPanelMode(maximized ? "expanded" : "fullscreen")}
                  aria-label={maximized ? "Exit full screen" : "Maximize"}
                  title={maximized ? "Exit full screen (Esc)" : "Maximize"}
                >
                  {maximized ? <Icon.Minimize size={14} /> : <Icon.Maximize size={14} />}
                </button>}
              {onCollapse && !embeddedInWorkspace && !maximized && !poppedOut ? (
                <button type="button" className="stage-icon-btn" onClick={onCollapse} aria-label="Collapse panel" title="Collapse panel">
                  <Icon.ChevronRight size={15} />
                </button>
              ) : null}
            </div>
          </div>
        </div>

      {consentOpen ? (
        <BrowserConsentBar
          reason={!state.connected && externalBrowser
            ? "Browser profile disconnected. Reconnect above, or choose another browser."
            : state.loginReason}
          promoting={promoting}
          error={error || state.error}
          onPromote={promote}
        />
      ) : null}

      {requestedBrowser && (
        <div className="flex items-center gap-2 border-b border-line px-3 py-1.5 text-xs text-muted" role="status">
          <span className="truncate">{!requestedTarget ? "Showing the current browser"
            : requestedTab?.active ? "Showing the recorded tab"
            : requestedTab ? `Recorded tab: ${requestedTab.title || "Untitled"}${controlling ? "" : " · Take control to switch"}`
            : "Recorded tab unavailable · Showing the current browser"}</span>
          {requestedTab && !requestedTab.active && controlling && <button type="button" className="shrink-0 text-accent"
            disabled={controlsBusy} onClick={() => action("tab_select", tabTarget(requestedTab))}>Switch to tab</button>}
        </div>
      )}
      <StageWindow
        className="relative min-h-0 flex-1"
      >
      <div
        className={cn(
          "browser-stage-page relative h-full overflow-hidden",
          deviceMode === "mobile" && "is-mobile-canvas flex items-center justify-center p-3",
        )}
      >
        <div
          ref={previewBoxRef}
          className={cn(
            "relative h-full w-full",
            previewZoom === "responsive" ? "overflow-hidden" : "overflow-auto",
            deviceMode === "mobile" &&
              "browser-mobile-frame w-[390px] max-w-full h-[96%] max-h-[844px] rounded-[36px] border-[7px] border-[#181a1e] bg-canvas",
          )}
        >
          {deviceMode === "mobile" && (
            <div
              className="pointer-events-none absolute top-2 left-1/2 -translate-x-1/2 w-16 h-1 rounded-full bg-[#333740] z-30"
              aria-hidden="true"
            />
          )}
        <div
          data-testid="browser-page-canvas"
          style={rasterCanvas && (previewZoom !== "fit" || previewBoxSize.width > 0)
            ? { position: "relative", marginInline: "auto", ...canvasSize }
            : { position: "relative", width: "100%", height: "100%" }}
        >
        {showIframe ? (
          <iframe
            key={artifactUrl}
            src={artifactUrl!}
            title={state.title || "Live Browser Preview"}
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals"
            className="absolute inset-0 h-full w-full border-0 bg-transparent"
            allow="clipboard-read; clipboard-write; fullscreen"
          />
        ) : embeddedGuest ? (
          <>
            <div
              data-testid="embedded-guest"
              className="absolute inset-0 bg-canvas"
              aria-label="Embedded browser"
            />
            {chromeLocked || !controlling ? <div className="absolute inset-0 z-[1]" aria-hidden="true" /> : null}

          </>
        ) : novncEmbedSrc ? (
          <>
            <iframe
              title="This computer's screen"
              src={novncEmbedSrc}
              className="absolute inset-0 h-full w-full border-0 bg-canvas"
              allow="clipboard-read; clipboard-write; fullscreen"
            />
            {chromeLocked ? <div className="absolute inset-0 z-[1]" aria-hidden="true" /> : null}

          </>
        ) : blankPage && state.connected ? (
          <div className="stage-boot browser-empty flex flex-col items-center justify-center p-6 max-w-lg mx-auto w-full text-center">
            <div className="size-14 rounded-2xl bg-surface-hover/80 border border-line flex items-center justify-center shadow-md mb-4" aria-hidden="true">
              <Icon.Globe size={26} className="text-accent" />
            </div>
            <p className="stage-boot-title text-base font-semibold text-ink mb-1">
              {state.previewOnly ? "No page open" : "New Tab"}
            </p>
            <span className="sr-only">No page open</span>
            <p className="stage-boot-sub text-xs text-muted mb-5 max-w-sm">
              {state.previewOnly
                ? "Ask the agent to open a page in this profile."
                : `${browserProfileLabel(state.backend === "rust" ? state.selectedProfile || state.backend : state.backend)} is connected. Type a URL or choose a shortcut to begin.`}
            </p>
            {!state.previewOnly && state.canControl !== false && (
              <div className="w-full space-y-4">
                <form className="browser-empty-form flex items-center gap-2 rounded-xl border border-line bg-surface/90 p-1.5 shadow-sm focus-within:border-accent focus-within:ring-2 focus-within:ring-accent/20 transition-all" onSubmit={async event => {
                  event.preventDefault();
                  const form = event.currentTarget;
                  const input = form.elements.namedItem("address") as HTMLInputElement | null;
                  const raw = String(input?.value || "").trim();
                  if (!raw || controlChanging || promoting || agentDriving) return;
                  const targetUrl = normalizeBrowserUrl(raw);
                  if (!targetUrl) return;
                  const generation = sessionGeneration.current;
                  if (!controlling) await setDriver("you");
                  if (generation === sessionGeneration.current) {
                    await call("action", { action: "navigate", url: targetUrl });
                    if (input) input.value = "";
                  }
                }}>
                  <input
                    name="address"
                    aria-label="Website URL"
                    placeholder="Or open a site yourself — example.com"
                    required
                    disabled={controlChanging || Boolean(promoting) || agentDriving}
                    className="flex-1 bg-transparent text-xs text-ink outline-none px-2 py-1 placeholder:text-muted/60"
                  />
                  <button
                    type="submit"
                    disabled={controlChanging || Boolean(promoting) || agentDriving}
                    className="px-3.5 py-1 rounded-lg bg-accent text-accent-ink font-medium text-xs hover:brightness-110 active:scale-95 transition-all shadow-sm shrink-0"
                  >
                    {controlChanging ? "Connecting…" : "Open"}
                  </button>
                </form>
                <div className="grid grid-cols-4 gap-2 pt-1">
                  {[
                    { title: "Bare & Fair", host: "bareandfair.ca", url: "https://bareandfair.ca" },
                    { title: "Wikipedia", host: "wikipedia.org", url: "https://wikipedia.org" },
                    { title: "Google", host: "google.com", url: "https://google.com" },
                    { title: "Hacker News", host: "news.ycombinator.com", url: "https://news.ycombinator.com" },
                  ].map((site) => (
                    <button
                      key={site.host}
                      type="button"
                      disabled={controlChanging || Boolean(promoting) || agentDriving}
                      onClick={async () => {
                        if (!controlling) await setDriver("you");
                        await call("action", { action: "navigate", url: site.url });
                      }}
                      className="flex flex-col items-center gap-1.5 p-2 rounded-xl border border-line/50 bg-surface/40 hover:bg-surface-hover hover:border-line transition-all text-center group cursor-pointer"
                    >
                      <div className="size-7 rounded-lg bg-canvas border border-line/40 flex items-center justify-center text-[0.68rem] font-bold text-accent group-hover:scale-105 transition-transform">
                        {site.title[0]}
                      </div>
                      <span className="text-[0.65rem] font-medium text-ink truncate max-w-full">{site.title}</span>
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
        ) : viewFailed ? (
          <StageBoot title={boot.title} sub={boot.sub} failed />
        ) : liveStreamUrl || retainedStream ? (
          <>
            {!retainedStream && !streamReady && !frameUrl ? <StageBoot title={boot.title} sub={boot.sub} /> : null}
            <BrowserLiveSurface
              surfaceRef={setPreviewElement}
              source={liveStreamUrl}
              binding={state.targetIdentity || state.bindingId!}
              onPaint={() => {
                if (!state.previewAcknowledgment || !panelVisible || document.visibilityState === "hidden"
                  || stateRef.current.bindingId !== state.bindingId || activeStreamUrl.current !== liveStreamUrl) return;
                const now = Date.now();
                const key = `${sessionId}/${state.bindingId}`;
                if (lastPaintReport.current.binding === key && now - lastPaintReport.current.at < 2000) return;
                lastPaintReport.current = { binding: key, at: now };
                void api(`/api/sessions/${encodeURIComponent(sessionId!)}/browser/paint`, {
                  method: "POST", body: JSON.stringify({ bindingId: state.bindingId, displayed: true }),
                }).catch(() => {});
              }}
              tabIndex={0}
              className={cn(
                "browser-stage-shot",
                targetChanged && "opacity-0",
                blankPage && "is-pending",
                "is-contain object-contain",
              )}
              style={{ zIndex: 1 }}
              onMouseDown={onPreviewMouseDown}
              onMouseMove={onPreviewMouseMove}
              onMouseUp={onPreviewMouseUp}
              onWheel={onPreviewWheel}
              onContextMenu={(event) => event.preventDefault()}
              onFailure={() => {
                if (activeStreamUrl.current !== liveStreamUrl) return;
                setFailedStreamKey(currentStreamKey);
                setStreamFailures((count) => count + 1);
                setStreamAttempt((count) => count + 1);
              }}
              onFrame={() => {
                if (stateRef.current.bindingId?.split(":")[0] !== state.bindingId?.split(":")[0] || activeStreamUrl.current !== liveStreamUrl) return;
                setPaintedStream(liveStreamUrl);
                setPaintedBinding(state.bindingId);
                setPaintedTarget(state.targetIdentity || undefined);
                if (streamFailures) setStreamFailures(0);
                measurePreview();
              }}
            />
            {targetChanged && frameUrl && <img
              src={frameUrl}
              alt=""
              data-testid="browser-stream-fallback"
              className="browser-stage-shot pointer-events-none"
            />}
            {highlightBox ? (
              <AgentHighlight box={highlightBox} target={state.highlight?.target} />
            ) : null}
          </>
        ) : frameUrl ? (
          <>
            {incomingFrameUrl && paintedUrl && incomingFrameUrl !== paintedUrl ? (
              <img
                key={incomingFrameUrl}
                src={incomingFrameUrl}
                alt=""
                className={cn(
                  "browser-stage-shot is-incoming",
                  "is-contain",
                )}
                onLoad={() => {
                  if (stateRef.current.bindingId !== state.bindingId) return;
                setPaintedBinding(state.bindingId);
                  setPaintedUrl(incomingFrameUrl);
                  measurePreview();
                }}
              />
            ) : null}
            <img
              ref={setPreviewElement}
              src={paintedUrl || incomingFrameUrl}
              alt=""
              tabIndex={0}
              className={cn(
                "browser-stage-shot",
                "is-contain object-contain",
              )}
              onMouseDown={onPreviewMouseDown}
              onMouseMove={onPreviewMouseMove}
              onMouseUp={onPreviewMouseUp}
              onWheel={onPreviewWheel}
              onContextMenu={(event) => event.preventDefault()}
              onLoad={() => {
                if (stateRef.current.bindingId !== state.bindingId) return;
                setPaintedBinding(state.bindingId);
                if (incomingFrameUrl) setPaintedUrl(incomingFrameUrl);
                measurePreview();
              }}
            />
            {highlightBox ? (
              <AgentHighlight box={highlightBox} target={state.highlight?.target} />
            ) : null}
          </>
        ) : state.backend === "lightpanda" && state.connected ? (
          <div className="stage-boot" role="status">
            <Icon.Globe size={28} className="text-muted" aria-hidden="true" />
            <p className="stage-boot-title">DOM browser connected</p>
            <p className="stage-boot-sub">This headless browser reads page structure. Choose a private browser or a logged-in profile for a live preview.</p>
          </div>
        ) : state.backend === "rust" && !state.connected && !state.stateUnavailable ? (
          <div className="stage-boot" role="status">
            <Icon.Globe size={28} className="text-muted" aria-hidden="true" />
            <p className="stage-boot-title">Browser disconnected</p>
            <p className="stage-boot-sub">Reconnect to open a live tab for this chat.</p>
            {webpagePath && (
              <button type="button" className="browser-control-button" onClick={() => {
                useAppStore.getState().selectCanvasItem(webpagePath);
                pinWorkspaceSurface("artifacts");
              }}>
                Preview generated webpage in Files
              </button>
            )}
          </div>
        ) : (
          <StageBoot title={boot.title} sub={boot.sub} />
        )}
        {/* User Direct Control Click Wave */}
        {userClicks.map((click) => (
          <span
            key={click.id}
            className="pointer-events-none absolute z-20 rounded-full border-2 border-accent/80 bg-accent/20 animate-[user-click-wave_0.5s_cubic-bezier(0,0,0.2,1)_forwards]"
            style={{ left: `${click.x}px`, top: `${click.y}px` }}
            aria-hidden="true"
          />
        ))}
        </div>
        </div>
        {error && !consentOpen && <div role="alert" className="absolute bottom-2 left-2 right-2 z-30 flex items-center justify-between gap-2 rounded-lg border border-red-500/30 bg-red-950/90 px-3 py-1.5 text-xs text-red-200 shadow-md backdrop-blur-xs">
          <span className="truncate">{error.replace(/^[{"\s]+/, "").replace(/["}\s]+$/, "")}</span>
          <button type="button" className="shrink-0 rounded px-2 py-0.5 bg-red-900/60 hover:bg-red-800 text-red-100 transition-colors text-[0.68rem]" aria-label="Dismiss action error"
            onClick={() => { operationError.current = ""; setError(""); }}>Dismiss</button>
        </div>}
      </div>
      </StageWindow>

    </section>
  );
}

function BrowserProfileBar({ state, showSelect, disabled, onChange, onReconnect, hidden }: {
  state: BrowserState;
  showSelect: boolean;
  disabled: boolean;
  onChange: (target: string) => void;
  onReconnect: () => void;
  hidden?: boolean;
}) {
  return (
    <div className={cn("browser-window-header flex shrink-0 items-center gap-2 border-b border-line bg-surface px-3 py-1.5", hidden && "sr-only")}>
      <div className="browser-session-controls">
        {showSelect ? <BrowserSessionSelect state={state} disabled={disabled} onChange={onChange} />
          : <span>{browserProfileLabel(state.selectedBackend || state.backend)}</span>}
        <span className="browser-connection" data-connected={state.connected} role="status" aria-label={state.connected ? "Connected" : "Disconnected"}>
          <span aria-hidden="true" /><span className="browser-connection-label">{state.connected ? "Connected" : "Disconnected"}</span>
        </span>
        {!state.connected && <button type="button" className="browser-reconnect" disabled={disabled} onClick={onReconnect}>Reconnect</button>}
      </div>
    </div>
  );
}

function consentPrompt(promoting: string, error?: string, reason?: string): string {
  if (promoting === "chromium") return "Staying on a private browser…";
  if (promoting) return `Opening ${promoting === "chrome" ? "Chrome" : "Comet"}…`;
  const first = (error || "").trim().split("\n")[0];
  if (first) return first;
  return `${reason || "This page looks like a sign-in."} Use a logged-in browser?`;
}

function BrowserConsentBar({
  reason,
  promoting,
  error,
  onPromote,
}: {
  reason?: string;
  promoting: string;
  error?: string;
  onPromote: (target: string) => void;
}) {
  const busy = Boolean(promoting);
  return (
    <div
      className="browser-consent-bar"
      role="region"
      aria-label="Choose a browser profile"
      data-testid="browser-consent-bar"
      onMouseDown={(event) => event.stopPropagation()}
      onPointerDown={(event) => event.stopPropagation()}
    >
      <p>{consentPrompt(promoting, error, reason)}</p>
      <div className="browser-consent-actions">
        <button type="button" disabled={busy} onClick={() => onPromote("comet")}>Use Comet</button>
        <button type="button" disabled={busy} onClick={() => onPromote("chrome")}>Use Chrome</button>
        <button type="button" disabled={busy} onClick={() => onPromote("chromium")}>Stay private</button>
      </div>
    </div>
  );
}

function AgentHighlight({
  box,
  target,
}: {
  box: { left: number; top: number; width: number; height: number };
  target?: string;
}) {
  const isNearTop = box.top < 28;
  return (
    <div
      data-testid="browser-highlight"
      className="browser-highlight"
      style={{
        left: `${box.left}px`,
        top: `${box.top}px`,
        width: `${box.width}px`,
        height: `${box.height}px`,
      }}
    >
      {/* Outer sonar ripple ring */}
      <div
        className="pointer-events-none absolute inset-0 -m-1 rounded-lg border border-emerald-400/60 animate-[operator-ripple_2s_cubic-bezier(0,0,0.2,1)_infinite]"
        aria-hidden="true"
      />
      {/* Corner radar ping */}
      <div className="pointer-events-none absolute -top-1 -right-1 flex h-2.5 w-2.5" aria-hidden="true">
        <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75" />
        <span className="relative inline-flex rounded-full h-2.5 w-2.5 bg-emerald-500 shadow-sm" />
      </div>
      {/* AI Cursor Pin & Target Tag (OpenAI Operator Style) */}
      <div
        className={cn(
          "pointer-events-none absolute -left-1 flex items-center gap-1 rounded-full bg-emerald-600/95 px-2 py-0.5 text-[0.6rem] font-medium text-white shadow-md backdrop-blur-sm select-none whitespace-nowrap z-30",
          isNearTop ? "top-full mt-1.5" : "-top-6",
        )}
        aria-hidden="true"
      >
        <svg className="w-2.5 h-2.5 text-white fill-current shrink-0 -rotate-45" viewBox="0 0 24 24">
          <path d="M3 3l7.07 16.97 2.51-7.39 7.39-2.51L3 3z" />
        </svg>
        <span className="truncate max-w-[130px]">{target || "Target"}</span>
      </div>
    </div>
  );
}
