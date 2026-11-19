import { useCallback, useEffect, useRef, useState } from "react";
import type { MouseEvent, WheelEvent, KeyboardEvent as ReactKeyboardEvent } from "react";
import { api } from "../api/client";
import type { ComputerState } from "../api/types";
import { isComputerToolName } from "../lib/activityUi";
import { createClientId } from "../lib/clientId";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { useAppStore } from "../store";
import { refreshSessionTruth } from "../lib/sessionTruth";
import { useShallow } from "zustand/react/shallow";
import { StageDriver, StageIdentity, StageWindow } from "./shared/Stage";
import { useStageChrome } from "./shared/useStageChrome";

const EMPTY: ComputerState = { sessionId: "", available: false, connected: false };

function containPoint(
  event: MouseEvent<HTMLImageElement>,
  image: HTMLImageElement,
  viewport: { width: number; height: number },
) {
  const rect = image.getBoundingClientRect();
  const natW = image.naturalWidth || viewport.width;
  const natH = image.naturalHeight || viewport.height;
  const scale = Math.min(rect.width / Math.max(1, natW), rect.height / Math.max(1, natH));
  const displayW = natW * scale;
  const displayH = natH * scale;
  const offsetX = (rect.width - displayW) / 2;
  const offsetY = (rect.height - displayH) / 2;
  const localX = event.clientX - rect.left - offsetX;
  const localY = event.clientY - rect.top - offsetY;
  if (scale <= 0 || localX < 0 || localY < 0 || localX >= displayW || localY >= displayH) return null;
  return {
    x: Math.max(0, Math.min(viewport.width, ((event.clientX - rect.left - offsetX) / scale) * (viewport.width / natW))),
    y: Math.max(0, Math.min(viewport.height, ((event.clientY - rect.top - offsetY) / scale) * (viewport.height / natH))),
  };
}

interface ComputerPanelProps {
  onCollapse?: () => void;
  onOpenBrowser?: () => void;
  machine?: boolean;
}

export function ComputerPanel({ onCollapse, onOpenBrowser, machine = false }: ComputerPanelProps) {
  const sessionId = useAppStore((s) => s.sessionId);
  const currentSessionRef = useRef(sessionId);
  currentSessionRef.current = sessionId;
  const runStatus = useAppStore((s) => s.runStatus);
  const panelMode = useAppStore((s) => s.computerPanelMode);
  const setPanelMode = useAppStore((s) => s.setComputerPanelMode);
  const setSurfaceControlling = useAppStore((s) => s.setSurfaceControlling);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const { agentFrame, activeComputerTool } = useAppStore(
    useShallow((s) => {
      let frame;
      let tool;
      for (let i = s.messages.length - 1; i >= 0; i--) {
        const m = s.messages[i];
        if (!frame && m.computerFrames?.length) {
          frame = m.computerFrames.findLast((f) => f.app !== "Kageha Browser");
        }
        if (!tool && m.toolCards?.length) {
          tool = m.toolCards.findLast((c) => isComputerToolName(c.name) && c.status === "running");
        }
        if (frame && tool) break;
      }
      return { agentFrame: frame, activeComputerTool: tool };
    }),
  );
  const [state, setState] = useState<ComputerState>(EMPTY);
  const hostedLinux = state.provider === "docker_desktop" || state.provider === "linux_host" || state.provider === "ssh_desktop";
  const [text, setText] = useState("");
  const [appName, setAppName] = useState("");
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  const [inputError,setInputError] = useState("");
  const [liveFrame, setLiveFrame] = useState<{url:string; id?:string; width:number; height:number} | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [failedPreviews, setFailedPreviews] = useState<string[]>([]);
  const previewRef = useRef<HTMLImageElement | null>(null);
  const previewTargetRef = useRef("");
  const dragStartRef = useRef<{x:number;y:number} | null>(null);
  const suppressClickRef = useRef(false);
  const captureBusyRef = useRef(false);
  const inputBusyRef = useRef(false);
  const controlRevision = useRef(0);

  const waitForDesktopPaint = useCallback(() => new Promise<void>((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
  }), []);

  const refresh = useCallback(async () => {
    if (!sessionId || currentSessionRef.current !== sessionId || captureBusyRef.current) return;
    const requestedSession = sessionId;
    const revision = controlRevision.current;
    captureBusyRef.current = true;
    try {
      const next = await api<ComputerState>(`/api/sessions/${encodeURIComponent(sessionId)}/computer`);
      if (currentSessionRef.current !== requestedSession || revision !== controlRevision.current) return;
      const target = `${requestedSession}:${next.serviceEpoch || ""}:${next.connected}:${next.app || ""}`;
      if (previewTargetRef.current !== target) { setLiveFrame(null); previewTargetRef.current = target; }
      setState(next);
      setError(next.error || "");
      if (next.connected && next.previewAvailable === true) {
        const reply = await api<{ status: string; frame_id?:string; result?: { width?:number; height?:number; image?: { mime_type: string; data: string } }; error?: { message?: string } }>(`/api/sessions/${encodeURIComponent(requestedSession)}/computer/preview`);
        if (currentSessionRef.current !== requestedSession || revision !== controlRevision.current) return;
        const image = reply.result?.image;
        if (reply.status === "ok" && image && ["image/jpeg", "image/png"].includes(image.mime_type)) {
          setLiveFrame({url:`data:${image.mime_type};base64,${image.data}`,id:reply.frame_id,width:reply.result?.width || 0,height:reply.result?.height || 0});
        } else {
          setError(reply.error?.message || "Desktop preview unavailable");
        }
      }
    } catch (err) {
      if (currentSessionRef.current === requestedSession && revision === controlRevision.current) setError(err instanceof Error ? err.message : String(err));
    } finally {
      captureBusyRef.current = false;
    }
  }, [sessionId]);

  useEffect(() => {
    setState(EMPTY);
    setLiveFrame(null);
    setError("");
    setFailedPreviews([]);
    setInputError("");
  }, [sessionId]);

  useEffect(() => {
    if (!sessionId) return;
    void refresh();
    const timer = window.setInterval(() => void refresh(), machine ? 700 : runStatus === "running" ? 1500 : 3000);
    return () => window.clearInterval(timer);
  }, [sessionId, runStatus, refresh, machine]);

  useEffect(() => {
    if (panelMode !== "fullscreen") return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPanelMode("expanded");
    };
    window.addEventListener("keydown", onKeyDown);
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      document.body.style.overflow = "";
    };
  }, [panelMode, setPanelMode]);

  const call = useCallback(async (path: string, body: Record<string, unknown>) => {
    const changesControl = path === "lease" || path === "access" || path === "refresh" || path === "provider";
    const manualInput = path === "action";
    if (manualInput && hostedLinux && !liveFrame?.id) return false;
    if (!sessionId || (!changesControl && !manualInput && captureBusyRef.current)) return;
    if (manualInput && inputBusyRef.current) return;
    if (manualInput) {inputBusyRef.current = true;setInputError("");}
    const requestedSession = sessionId;
    if (changesControl || manualInput) controlRevision.current += 1;
    if (hostedLinux && (changesControl || manualInput)) {
      setLiveFrame(frame => frame ? {...frame, id: undefined} : null);
    }
    else captureBusyRef.current = true;
    const revision = controlRevision.current;
    setCapturing(true);
    try {
      await waitForDesktopPaint();
      const next = await api<ComputerState>(`/api/sessions/${encodeURIComponent(sessionId)}/computer/${path}`, {
        method: "POST", headers: { "X-Kageha-Computer": state.provider || "local_mac" }, body: JSON.stringify(manualInput && state.runtime === "rust" ? {...body,frame_id:liveFrame?.id,request_id:createClientId()} : body),
      });
      if (currentSessionRef.current !== requestedSession) return;
      if (revision !== controlRevision.current) return;
      if (manualInput && state.runtime === "rust") {
        const result=next as unknown as {status:string;summary?:string;outcome?:{summary?:string;error?:{message?:string}}};
        if(result.status!=="ok") throw new Error(result.outcome?.error?.message || result.summary || result.outcome?.summary || "Input outcome is uncertain. Inspect the app before trying again.");
        setError("");
      } else {setState(next);setError(next.error || "");}
      if (path === "provider") refreshSessionTruth(requestedSession);
      return true;
    } catch (err) {
      if (currentSessionRef.current !== requestedSession || revision !== controlRevision.current) return false;
      if (manualInput || path === "provider") setInputError(err instanceof Error ? err.message : String(err));
      else setError(err instanceof Error ? err.message : String(err));
      return false;
    } finally {
      if (manualInput) inputBusyRef.current = false;
      if (!changesControl) captureBusyRef.current = false;
      setCapturing(false);
      if (manualInput || changesControl) void refresh();
    }
  }, [sessionId, waitForDesktopPaint, state.runtime, state.provider, hostedLinux, liveFrame, refresh]);

  const requestScreenPermission = async (kind = "screen_recording") => {
    if (!sessionId) return;
    setStarting(true);
    try {
      await api(`/api/sessions/${encodeURIComponent(sessionId)}/computer/permission`, {method:"POST",body:JSON.stringify({kind})});
      await refresh();
    } catch (err) { setError(err instanceof Error ? err.message : String(err)); }
    finally { setStarting(false); }
  };

  const startSession = async () => {
    setStarting(true);
    try {
      await call("start", { app: appName.trim() });
    } finally {
      setStarting(false);
    }
  };

  const setDriver = (who: "agent" | "you") => {
    const userHas = state.lockedBy === "user";
    if (who === "you") pinWorkspaceSurface("live");
    if (who === "you" && (!userHas || !!state.interruptionReason)) void call("lease", { action: "take" });
    if (who === "agent" && userHas) void call("lease", { action: "return" });
  };
  const pointForEvent = (event: MouseEvent<HTMLImageElement>) => previewRef.current ? containPoint(event,previewRef.current,{width:liveFrame?.width || previewRef.current.naturalWidth,height:liveFrame?.height || previewRef.current.naturalHeight}) : null;
  const onPreviewClick = (event: MouseEvent<HTMLImageElement>) => {
    if (suppressClickRef.current) {suppressClickRef.current=false;return;}
    if (state.lockedBy !== "user" || state.canControl === false) return;
    const image = previewRef.current;
    if (!image) return;
    const point = containPoint(event, image, {
      width: Number(liveFrame?.width || state.frame?.width || image.naturalWidth || 1),
      height: Number(liveFrame?.height || state.frame?.height || image.naturalHeight || 1),
    });
    if (!point) return;
    void call("action", { action: "click", x: point.x, y: point.y, ...(event.type === "contextmenu" ? {button:"right"} : {}) });
  };
  const onPreviewKey = (event: ReactKeyboardEvent<HTMLImageElement>) => {
    if (state.lockedBy !== "user" || state.canControl === false) return;
    const names:Record<string,string>={Enter:"return",Escape:"escape",Backspace:"backspace",Delete:"delete",ArrowLeft:"left",ArrowRight:"right",ArrowUp:"up",ArrowDown:"down",Tab:"tab"," ":"space",PageUp:"pageup",PageDown:"pagedown",Home:"home",End:"end"};
    const key=names[event.key] || ((event.metaKey || event.ctrlKey) && /^[a-z0-9]$/i.test(event.key) ? event.key.toLowerCase() : "");
    if (!key) return;
    event.preventDefault();
    void call("action",{action:"key",key:[event.metaKey&&"cmd",event.ctrlKey&&"ctrl",event.altKey&&"alt",event.shiftKey&&"shift",key].filter(Boolean).join("+")});
  };
  const onPreviewWheel = (event: WheelEvent<HTMLImageElement>) => {
    if (state.lockedBy !== "user" || state.canControl === false) return;
    event.preventDefault();
    void call("action", {
      action: "scroll",
      direction: Math.abs(event.deltaX) > Math.abs(event.deltaY)
        ? (event.deltaX > 0 ? "right" : "left")
        : (event.deltaY > 0 ? "down" : "up"),
      amount: 3,
    });
  };

  const waitingOnAgent = Boolean(state.agentBound || activeComputerTool);
  const controlling = state.lockedBy === "user";
  const { visible: chromeVisible, bind: chromeBind, noteAction } = useStageChrome(controlling);
  useEffect(() => {
    setSurfaceControlling(state.lockedBy === "user");
  }, [state.lockedBy, setSurfaceControlling]);
  const chromeArmed = useRef(false);
  useEffect(() => {
    if (!state.frameVersion && !state.lastAction) return;
    if (!chromeArmed.current) {
      chromeArmed.current = true;
      return;
    }
    noteAction();
  }, [state.frameVersion, state.lastAction, noteAction]);
  const maximized = panelMode === "fullscreen";
  const path = state.frame?.path || state.frame?.thumb_path || "";
  const fileFrameUrl = path && sessionId
    ? `/api/sessions/${encodeURIComponent(sessionId)}/files/${path.split("/").map(encodeURIComponent).join("/")}?v=${state.frameVersion || 0}`
    : "";
  const appLabel = state.app || agentFrame?.app || "Desktop";
  const browserApp = /comet|chrome|safari|firefox|arc|brave|edge/i.test(appLabel);
  const toolAction = activeComputerTool?.name
    .replace(/^.*computer_/, "")
    .replaceAll("_", " ");
  const actionLabel = toolAction || agentFrame?.action || state.lastAction || state.frame?.action || "";
  const frameW = Number(state.frame?.width || 0);
  const frameH = Number(state.frame?.height || 0);
  const stubFrame = frameW > 0 && frameH > 0 && (frameH < 80 || frameW < 160 || frameW / frameH >= 12);
  // Native desktop work may target a browser window too.
  const hasCapturedFrame = Boolean(agentFrame?.url || fileFrameUrl);
  // The polled frame is the current desktop; transcript thumbnails may be stale.
  // A failed image must fall through to another source or the reconnect UI.
  const previewUrl = (state.previewAvailable === true ? [liveFrame?.url] : [!stubFrame && fileFrameUrl, agentFrame?.url])
    .find((url): url is string => Boolean(url) && !failedPreviews.includes(String(url))) || "";
  const hasPreview = Boolean(previewUrl);
  useEffect(() => {
    if (!machine) return;
    useAppStore.setState({ machineScreenConnected: hasPreview && !error });
    return () => useAppStore.setState({ machineScreenConnected: false });
  }, [machine, hasPreview, error]);

  const previewUnsupported = state.previewAvailable === false;
  const previewError = error && !/^waiting for /i.test(error);
  const statusLine = controlling
    ? "You control it"
    : actionLabel
      ? `${appLabel} · ${actionLabel}`
      : waitingOnAgent
        ? `Agent on ${appLabel}`
        : state.connected
          ? "Watching"
          : "Desktop disconnected";

  if (!sessionId) return null;

  return (
    <section
      className={cn(
        "relative flex min-h-0 flex-col overflow-hidden bg-canvas",
        maximized && "fixed inset-0 z-50 h-screen w-screen border-0 bg-canvas",
        !maximized && "min-h-0 flex-1",
      )}
      aria-label="Live desktop control"
      aria-modal={maximized || undefined}
      {...chromeBind}
    >
      <div className={cn("stage-hud-wrap", !chromeVisible && "is-hidden")}>
        <div className="stage-hud">
          <StageDriver
            controlling={controlling}
            onAgent={() => setDriver("agent")}
            onYou={() => setDriver("you")}
            label="Who controls this desktop"
          />
          <StageIdentity live={!controlling && waitingOnAgent}>
            <span className="shrink-0 rounded-full bg-surface-hover px-1.5 py-[0.12rem] text-[0.58rem] font-semibold uppercase tracking-[0.14em] text-muted">
              {state.provider === "ssh_desktop" || state.provider === "ssh" ? "SSH" : state.provider === "linux_host" ? "Linux host" : machine ? "Linux" : <select aria-label="Computer desktop" className="bg-transparent text-inherit" disabled={capturing && !captureBusyRef.current}
                value={state.provider === "docker_desktop" ? "docker_desktop" : "local_mac"}
                onChange={(event) => { setInputError(""); setLiveFrame(null); void call("provider", {provider:event.target.value}); }}>
                <option value="local_mac">Mac</option>
                <option value="docker_desktop">Docker Linux</option>
              </select>}
            </span>
            <span className="stage-host">{statusLine}</span>
          </StageIdentity>
          <div className="stage-actions">
            {state.inputAuthorized !== undefined && <button className="stage-chip" onClick={() => void call("access", {allow:!state.inputAuthorized})}>{state.inputAuthorized ? "Revoke access" : "Enable agent control"}</button>}
            {onOpenBrowser ? (
              <button type="button" className="stage-icon-btn" onClick={onOpenBrowser} aria-label="Browser" title="Browser">
                <Icon.Globe size={14} />
              </button>
            ) : null}
            {controlling ? (
              <button
                type="button"
                className="stage-icon-btn"
                onClick={() => setPanelMode(maximized ? "expanded" : "fullscreen")}
                aria-label={maximized ? "Exit full screen" : "Maximize"}
                title={maximized ? "Exit full screen (Esc)" : "Maximize"}
              >
                {maximized ? <Icon.Minimize size={14} /> : <Icon.Maximize size={14} />}
              </button>
            ) : null}
            {controlling && onCollapse && !maximized ? (
              <button type="button" className="stage-icon-btn" onClick={onCollapse} aria-label="Collapse panel" title="Collapse panel">
                <Icon.ChevronRight size={15} />
              </button>
            ) : null}
          </div>
        </div>
      </div>
      <StageWindow>
        <div className="relative h-full min-h-0 flex-1 overflow-hidden bg-canvas">
          {hasPreview ? (
            <img
              ref={previewRef}
              data-input-ready={hostedLinux && controlling && !!liveFrame?.id}
              src={previewUrl}
              alt="Live desktop"
              onError={() => setFailedPreviews((urls) => [...urls.slice(-7), previewUrl])}
              className={cn(
                "absolute left-1/2 top-1/2 h-auto w-auto max-h-full max-w-full -translate-x-1/2 -translate-y-1/2 rounded-[1.25rem] object-contain shadow-[0_24px_80px_rgba(0,0,0,0.55)]",
                controlling ? "cursor-crosshair" : "cursor-default",
              )}
              draggable={false}
              tabIndex={controlling && state.canControl !== false ? 0 : -1}
              onKeyDown={onPreviewKey}
              onContextMenu={(event)=>{event.preventDefault();onPreviewClick(event);}}
              onMouseDown={(event)=>{if(event.button===0 && controlling && state.canControl !== false)dragStartRef.current=pointForEvent(event);}}
              onMouseUp={(event)=>{const from=dragStartRef.current;dragStartRef.current=null;const to=pointForEvent(event);if(from && to && Math.hypot(to.x-from.x,to.y-from.y)>5){suppressClickRef.current=true;void call("action",{action:"drag",from_x:from.x,from_y:from.y,to_x:to.x,to_y:to.y});}}}
              onMouseLeave={()=>{dragStartRef.current=null;}}
              onClick={onPreviewClick}
              onWheel={onPreviewWheel}
            />
          ) : (
            <div className="stage-boot">
              {!previewError && !previewUnsupported && waitingOnAgent && <div className="stage-boot-ring" />}
              <div className="stage-boot-title">
                {previewError ? "Desktop preview unavailable" : previewUnsupported ? "Live desktop preview is not available" : browserApp && !hasCapturedFrame ? "This is a browser window" : waitingOnAgent ? `Waiting for ${appLabel} preview` : state.connected ? "Waiting for a live frame" : "Connect a desktop app"}
              </div>
              <p className="stage-boot-sub">
                {previewError
                  ? error
                  : previewUnsupported
                  ? "Watch the app on your Mac. Agent actions are listed in Activity."
                  : browserApp && !hasCapturedFrame
                  ? "Use Browser for web tasks, or focus a native Mac app."
                  : waitingOnAgent
                    ? "The app is selected. Waiting for the agent to publish a window image."
                    : machine ? "Waiting for the Docker desktop to start." : "Send /computer with a task, or connect an app."}
              </p>
              {(previewError || !state.connected) && hostedLinux && <button className="stage-chip mt-4" onClick={() => void call("refresh", {})}>{state.connected ? "Reconnect Linux desktop" : "Start Linux desktop"}</button>}
              {previewError && /Screen Recording/i.test(error) && <button className="stage-chip mt-4" disabled={starting} onClick={() => void requestScreenPermission()}>Enable Screen Recording</button>}
              {!state.connected && !previewUnsupported && !hostedLinux && state.available ? (
                <div className="mt-5 flex w-full max-w-xs gap-2">
                  <input
                    className="min-w-0 flex-1 rounded-full border border-line bg-surface-hover px-3 py-1.5 text-xs text-ink outline-none placeholder:text-muted focus:border-emerald-300/40"
                    value={appName}
                    onChange={(event) => setAppName(event.target.value)}
                    onKeyDown={(event) => { if (event.key === "Enter") void startSession(); }}
                    placeholder="App name · optional"
                    aria-label="Desktop app to connect"
                    disabled={starting || !state.available}
                  />
                  <button
                    type="button"
                    className="rounded-full bg-emerald-400 px-3 py-1.5 text-xs font-semibold text-slate-950 hover:bg-emerald-300 disabled:opacity-60"
                    onClick={() => void startSession()}
                    disabled={starting || !state.available}
                  >
                    {starting ? "…" : "Connect"}
                  </button>
                </div>
              ) : null}
            </div>
          )}
          <div className="stage-glow" data-who={controlling ? "you" : "agent"} />
          {capturing && (
            <div className="pointer-events-none absolute right-3 top-[4.2rem] z-[3] rounded-full bg-surface/95 px-2.5 py-1 text-[0.62rem] text-muted backdrop-blur-sm">
              Updating…
            </div>
          )}
          {!controlling && (waitingOnAgent || state.agentBound) ? (
            <button type="button" className={cn("stage-chip absolute bottom-4 left-4 z-[3]", runStatus === "running" && "is-live")} onClick={() => setDriver("you")}>
              <span className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-emerald-400" />
              <span className="truncate text-[0.65rem]">{statusLine}</span>
            </button>
          ) : null}
          {state.interruptionReason && <div role="status" className="absolute left-3 right-3 top-3 z-[4] rounded-xl border border-amber-400/30 bg-surface/95 p-3 text-sm text-ink">
            <p>{state.interruptionReason === "human_input" ? "Paused because you used the mouse or keyboard." : state.interruptionReason === "monitor_unavailable" ? "Paused because input monitoring is unavailable. Check macOS permissions or Secure Input." : "Computer connection interrupted. Previous input will not be replayed."}</p>
            <div className="mt-2 flex gap-2"><button className="stage-chip" onClick={() => setDriver("agent")}>Return control to agent</button><button className="stage-chip" onClick={() => setDriver("you")}>Resume manual control</button>
            {state.interruptionReason === "monitor_unavailable" && <button className="stage-chip" onClick={() => void requestScreenPermission("input_monitoring")}>Enable Input Monitoring</button>}</div>
          </div>}
          {controlling && state.canControl === false && <div className="absolute bottom-4 left-4 right-4 rounded-xl bg-surface/95 p-3 text-sm text-ink">{state.inputAuthorized === false ? "Agent access is disabled. Enable agent control, then choose Agent." : "You have control on your Mac. Choose Agent to return control."}</div>}
          {controlling && state.canControl !== false && (!hostedLinux || !!liveFrame?.id) ? (
            <div className="absolute inset-x-0 bottom-12 z-[3] flex justify-center bg-gradient-to-t from-black/75 via-black/25 to-transparent p-4 pt-10">
              <div className="flex w-full max-w-xl items-center gap-0.5 rounded-full border border-line bg-surface/95 p-1 shadow-[0_18px_50px_rgba(0,0,0,0.38)] backdrop-blur-[22px]">
                {state.runtime === "rust" && <button type="button" className="stage-icon-btn text-[0.68rem]" onClick={()=>void call("refresh",{})} title="Refresh selected window">Refresh view</button>}
                <input
                  className="min-w-0 flex-1 bg-transparent px-3 py-1.5 text-[0.72rem] text-ink outline-none placeholder:text-muted"
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter" && text) { const submitted=text;void call("action", { action: "type", text:submitted }).then(ok=>{if(ok)setText(current=>current===submitted ? "" : current)}); } }}
                  placeholder="Type into the app…"
                />
                {!hostedLinux && <button type="button" className="stage-icon-btn text-[0.68rem] font-semibold" onClick={() => void call("action", { action: "focus" })} title="Focus selected window">Focus</button>}
                {hostedLinux && <button type="button" className="stage-icon-btn text-[0.68rem] font-semibold" onClick={() => void call("action", { action: "terminal" })}>Terminal</button>}
                <button type="button" className="stage-icon-btn text-[0.68rem] font-semibold" onClick={() => void call("action", { action: "key", key: "return" })} title="Return">↵</button>
                <button type="button" className="stage-icon-btn text-[0.62rem] font-semibold" onClick={() => void call("action", { action: "key", key: "escape" })} title="Escape">Esc</button>
              </div>
            </div>
          ) : null}
          {inputError && <div role="alert" className="absolute bottom-20 left-3 right-3 z-[4] rounded-lg bg-red-950/95 p-3 text-sm text-red-100">{inputError}</div>}
          {hasPreview && previewError && !(state.agentBound && /browser/i.test(error)) && (
            <div className="absolute bottom-16 left-3 right-3 z-[3] rounded-lg bg-red-950/85 px-2 py-1.5 text-[0.65rem] text-red-200">{error}</div>
          )}
        </div>
      </StageWindow>
    </section>
  );
}
