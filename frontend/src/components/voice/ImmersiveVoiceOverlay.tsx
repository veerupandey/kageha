import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

import { cn } from "../../lib/cn";
import { getLiveVoiceProvider, setLiveVoiceProvider, type LiveVoiceProvider } from "../../lib/geminiLiveClient";
import { useFocusTrap } from "../../lib/focusTrap";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";
import { ApprovalBanner } from "../ApprovalBanner";
import type { useGeminiLiveVoice } from "../useGeminiLiveVoice";
import { VoiceCaptions } from "./VoiceCaptions";
import { VoiceControls } from "./VoiceControls";
import { VoiceOrb } from "./VoiceOrb";
import { VoicePhaseStatus } from "./VoicePhaseStatus";
import { useVoiceTranscript } from "./useVoiceTranscript";

/** The shape returned by the shared live-voice hook. */
export type UseGeminiLiveVoice = ReturnType<typeof useGeminiLiveVoice>;

export interface ImmersiveVoiceOverlayProps {
  open: boolean;
  /** The element that opened the overlay, for focus restore (Req 2.4, 9.7). */
  openerRef: RefObject<HTMLElement | null>;
  /** Live voice hook instance shared with the composer (single session). */
  voice: UseGeminiLiveVoice;
  /** Current thread/session id from the store; null => no thread (Req 11.3/11.4). */
  sessionId: string | null;
  /** Close the overlay WITHOUT stopping the session (Req 2). */
  onClose: () => void;
}

/** The kind of error surfaced while the overlay is open. */
type OverlayErrorKind = "mic-permission" | "connection";

/**
 * Resolve the default reduced-motion preference from the browser.
 * Guarded for non-DOM environments (SSR / tests without a window).
 */
function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return false;
  }
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * Best-effort classification of the current error as denied microphone
 * permission vs. a generic connection error (Req 11.1 vs 11.2).
 *
 * The shared voice hook only surfaces a coarse `status === "error"`, so we
 * probe the Permissions API for a definitive `denied` microphone state. When
 * the API is unavailable or the state is anything other than `denied`, we fall
 * back to treating the failure as a connection error, which keeps a sensible,
 * actionable message in every case.
 */
async function classifyError(): Promise<OverlayErrorKind> {
  try {
    const perms = navigator.permissions;
    if (perms && typeof perms.query === "function") {
      const status = await perms.query({
        name: "microphone" as PermissionName,
      });
      if (status.state === "denied") return "mic-permission";
    }
  } catch {
    // Permissions API missing or the "microphone" name unsupported — fall
    // through to the connection-error default.
  }
  return "connection";
}

/**
 * Immersive full-screen live-voice overlay.
 *
 * Owns overlay open/close, the session reuse-vs-start decision, the focus
 * trap, Escape handling, and the error / edge-state rendering. It never starts
 * or stops the underlying session on its own except where the requirements
 * mandate it (start on entry when idle/error, stop on the end control) so the
 * single shared session stays continuous across enter/exit (Req 1.2, 2.2).
 */
export function ImmersiveVoiceOverlay({
  open,
  openerRef,
  voice,
  sessionId,
  onClose,
}: ImmersiveVoiceOverlayProps) {
  const { status, phase, start, stop } = voice;
  const [provider, setProvider] = useState<LiveVoiceProvider>(getLiveVoiceProvider);
  const { captions, activeDelegations, latestWork, progressLabel } = useVoiceTranscript(open ? sessionId : null);
  const pendingApproval = useAppStore((s) => s.pendingApproval);

  const containerRef = useRef<HTMLDivElement | null>(null);
  const initialFocusRef = useRef<HTMLButtonElement | null>(null);

  // Whether an active session existed at open time. When the overlay opens
  // onto an existing session, an eventual return to "idle" is unexpected and
  // should surface the "voice session ended" indication (Req 2.6). We also use
  // it to decide whether to auto-close (Req 3.3).
  const hadSessionAtOpenRef = useRef(false);
  // Tracks whether we intentionally ended the session from this overlay, so an
  // end-driven idle transition closes silently rather than showing "ended".
  const endedByUserRef = useRef(false);

  const [errorKind, setErrorKind] = useState<OverlayErrorKind | null>(null);
  const [sessionEnded, setSessionEnded] = useState(false);

  const reducedMotion = prefersReducedMotion();
  const noThread = sessionId === null;

  // --- Entry effect: reuse an existing session, or start a new one ---------
  // Runs when the overlay transitions to open. Reuses a connecting/active
  // session (Req 1.2); starts one when idle/error and a thread exists
  // (Req 1.3); never starts when there is no thread (Req 11.3, 11.4).
  useEffect(() => {
    if (!open) {
      // Reset transient state so a re-open starts clean.
      setErrorKind(null);
      setSessionEnded(false);
      endedByUserRef.current = false;
      hadSessionAtOpenRef.current = false;
      return;
    }

    if (noThread) {
      // No thread -> render the edge state; do NOT start a session or show the
      // active view (Req 11.3, 11.4).
      hadSessionAtOpenRef.current = false;
      return;
    }

    hadSessionAtOpenRef.current =
      status === "active" || status === "connecting";

    if (status === "idle" || status === "error") {
      endedByUserRef.current = false;
      setSessionEnded(false);
      start();
    }
    // We deliberately only depend on `open`/`sessionId`: the decision is made
    // at entry. Subsequent status changes are handled by the effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, sessionId]);

  // --- Error classification -------------------------------------------------
  useEffect(() => {
    if (!open || status !== "error") {
      setErrorKind(null);
      return;
    }
    let cancelled = false;
    void classifyError().then((kind) => {
      if (!cancelled) setErrorKind(kind);
    });
    return () => {
      cancelled = true;
    };
  }, [open, status]);

  // --- Auto-close when the session ends while open (Req 3.3, 2.6) ----------
  useEffect(() => {
    if (!open || noThread) return;
    if (status !== "idle") return;
    // Ignore the brief idle window during entry before start() connects: only
    // treat idle as an ended session once we've observed a live session.
    if (!hadSessionAtOpenRef.current) return;

    if (endedByUserRef.current) {
      // Ended intentionally via the end control — close silently (Req 3.2).
      onClose();
      return;
    }
    // Unexpected end — surface the indication, then close (Req 2.6, 3.3).
    setSessionEnded(true);
    onClose();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, status]);

  // --- Escape closes the overlay, session kept alive (Req 2.3, 9.6) --------
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  // --- Focus trap while open (Req 9.1, 9.2). focusTrap restores focus to the
  // previously focused element on teardown; we additionally handle the
  // opener/chat-view fallback explicitly on close (Req 2.4, 2.5, 9.7). -------
  useFocusTrap(open, containerRef, { initialFocusRef });

  const restoreFocus = useCallback(() => {
    const opener = openerRef.current;
    if (opener && document.contains(opener) && typeof opener.focus === "function") {
      opener.focus();
      return;
    }
    // Opener gone -> fall back to the underlying chat view (Req 2.5, 9.7).
    const chatView =
      document.querySelector<HTMLElement>("[data-chat-view]") ??
      document.querySelector<HTMLElement>("main");
    if (chatView && typeof chatView.focus === "function") {
      if (!chatView.hasAttribute("tabindex")) {
        chatView.setAttribute("tabindex", "-1");
      }
      chatView.focus();
    }
  }, [openerRef]);

  const handleExit = useCallback(() => {
    // Close WITHOUT stopping the session (Req 2.1, 2.2).
    onClose();
  }, [onClose]);

  const handleEnd = useCallback(() => {
    // Stop the session, then close (Req 3.1, 3.2). The idle transition that
    // follows is treated as a user-initiated end (silent close).
    endedByUserRef.current = true;
    stop();
    onClose();
  }, [stop, onClose]);

  // Restore focus to the opener (or chat view) whenever the overlay closes.
  const wasOpenRef = useRef(false);
  useEffect(() => {
    if (wasOpenRef.current && !open) {
      restoreFocus();
    }
    wasOpenRef.current = open;
  }, [open, restoreFocus]);

  const showActiveView = open && !noThread && status === "active";
  const showConnecting = open && !noThread && status === "connecting";
  const showError = open && !noThread && status === "error";

  const errorContent = useMemo(() => {
    if (!showError) return null;
    if (errorKind === "mic-permission") {
      return {
        title: "Microphone access denied",
        detail:
          "Live voice needs your microphone. Allow microphone access for this site in your browser settings, then start the conversation again.",
      };
    }
    return {
      title: "Connection error",
      detail:
        voice.error || "The live voice session could not connect. Check your network connection, then try again.",
    };
  }, [showError, errorKind, voice.error]);

  if (!open) return null;

  const hasWorkingDelegation = activeDelegations.size > 0;
  const approvalForSession = Boolean(
    pendingApproval?.approval_id &&
    (pendingApproval.sessionId || pendingApproval.session_id || sessionId) === sessionId,
  );

  const viewWork = () => {
    onClose();
    if (sessionId) {
      void (async () => {
        await useAppStore.getState().openSession(sessionId);
        useAppStore.getState().setWorkspaceSurface("artifacts");
        useAppStore.getState().setCanvasOpen(true);
      })();
    }
  };

  const overlay = (
    <div
      ref={containerRef}
      className={cn(
        "ka-voice-overlay fixed left-1/2 top-1/2 z-[80] flex w-[calc(100%-2rem)] max-w-2xl -translate-x-1/2 -translate-y-1/2 flex-col overflow-y-auto rounded-3xl border border-line shadow-2xl",
        "bg-surface text-ink",
      )}
      style={{
        // Theme-derived surface so a live theme change is reflected without a
        // remount (Req 10.2, 10.3). Falls back to the token if unset.
        backgroundColor: "var(--color-bg, var(--color-surface))",
        maxHeight: "calc(100dvh - 2rem)",
      }}
      role="dialog"
      aria-modal="true"
      aria-label="Immersive live voice"
      data-reduced-motion={reducedMotion ? "true" : "false"}
      data-status={status}
    >
      <label className="flex items-center justify-center gap-2 p-3 text-sm text-muted">
        Voice provider
        <select aria-label="Voice provider" value={provider}
          className="rounded border border-line bg-surface p-1 text-ink"
          onChange={(event) => {
            const value = event.target.value as LiveVoiceProvider;
            setLiveVoiceProvider(value);
            setProvider(value);
            stop();
            if (sessionId) void start(sessionId);
          }}>
          <option value="gemini">Gemini</option>
          <option value="openai">OpenAI</option>
        </select>
      </label>
      {noThread ? (
        // --- No-thread edge state (Req 11.3, 11.4) ---------------------------
        <div className="flex flex-1 flex-col items-center justify-center gap-4 p-8 text-center">
          <Icon.LiveVoice size={40} className="text-muted" aria-hidden="true" />
          <div className="max-w-md space-y-2">
            <h2 className="text-lg font-semibold text-ink">A thread is required</h2>
            <p className="text-sm text-muted">
              Start or open a thread first, then activate live voice to begin an
              immersive conversation.
            </p>
          </div>
          <VoiceControls
            onExit={handleExit}
            onEnd={handleEnd}
            showEndControl={false}
          />
        </div>
      ) : showError ? (
        // --- Error state — always keeps an exit control (Req 1.5, 11.1, 11.2, 11.5) ---
        <div className="flex flex-1 flex-col items-center justify-center gap-4 p-8 text-center">
          <div className="ka-voice-overlay-orb flex items-center justify-center">
            <VoiceOrb status={status} phase={phase} reducedMotion={reducedMotion} />
          </div>
          <Icon.Alert
            size={28}
            className="text-danger"
            aria-hidden="true"
          />
          <div
            className="max-w-md space-y-2"
            role="alert"
            data-error-kind={errorKind ?? "connection"}
          >
            <h2 className="text-lg font-semibold text-ink">
              {errorContent?.title}
            </h2>
            <p className="text-sm text-muted">{errorContent?.detail}</p>
          </div>
          <button type="button" onClick={() => start()} className="rounded-full bg-accent px-5 py-2 text-sm text-white focus-visible:ring-2 focus-visible:ring-accent">Try again</button>
          <VoiceControls onExit={handleExit} onEnd={handleEnd} showEndControl={false} />
        </div>
      ) : (
        // --- Connecting / active view ---------------------------------------
        <div className="flex flex-col items-center gap-5 p-6">
          <div className="flex w-full items-start justify-center pt-2">
            <VoicePhaseStatus status={status} phase={phase} />
          </div>

          <div className="flex flex-col items-center justify-center gap-3 py-4">
            {voice.avatarUrl ? (
              <video src={voice.avatarUrl} autoPlay playsInline aria-label="Live avatar"
                className="max-h-72 w-full rounded-2xl bg-black object-contain" />
            ) : (
              <div className="ka-voice-overlay-orb flex items-center justify-center">
                <VoiceOrb status={status} phase={phase} reducedMotion={reducedMotion} />
              </div>
            )}

            {showActiveView ? <p className="text-sm text-muted">{voice.muted ? "Microphone muted" : "Talk naturally. You can interrupt anytime."}</p> : null}
            {showConnecting ? (
              <p
                className="flex items-center gap-2 text-sm text-muted"
                role="status"
                aria-live="polite"
              >
                <Icon.Loader
                  size={16}
                  className={cn(!reducedMotion && "animate-spin")}
                  aria-hidden="true"
                />
                <span>Connecting to live voice…</span>
              </p>
            ) : null}

            {showActiveView && hasWorkingDelegation ? (
              // Delegation working indicator — conveyed without looping motion
              // under reduced motion (Req 12.1, 12.2, 12.4). App-wide reduced
              // motion CSS also neutralises the pulse.
              <p
                className="ka-voice-delegation flex items-center gap-2 rounded-full bg-surface/70 px-3 py-1 text-xs text-muted ring-1 ring-line"
                role="status"
                aria-live="polite"
                data-reduced-motion={reducedMotion ? "true" : "false"}
              >
                <span
                  className={cn(
                    "inline-block h-2 w-2 rounded-full bg-accent",
                    !reducedMotion && "animate-pulse",
                  )}
                  aria-hidden="true"
                />
                <span>{progressLabel || "Working on a background task…"}</span>
              </p>
            ) : null}
            {showActiveView && (hasWorkingDelegation || approvalForSession) ? (
              <button type="button" onClick={viewWork} className="text-sm text-accent hover:underline">
                Open thread and artifacts
              </button>
            ) : null}
            {showActiveView && latestWork ? (
              <div className="w-full rounded-xl border border-line bg-surface/70 p-3 text-sm" role="status">
                <p className="font-medium text-ink">
                  {latestWork.state === "completed" ? "Work finished" : latestWork.state === "failed" ? "Work needs attention" : "Work continues"}
                </p>
                <p className="mt-1 max-h-28 overflow-auto whitespace-pre-wrap text-muted">{latestWork.text}</p>
                <button type="button" onClick={viewWork} className="mt-2 text-accent hover:underline">
                  View in thread
                </button>
              </div>
            ) : null}
          </div>

          {showActiveView && approvalForSession ? (
            <div className="w-full overflow-hidden rounded-xl border border-warn/40" aria-label="Approval needed in live conversation">
              <ApprovalBanner />
            </div>
          ) : null}

          {showActiveView ? (
            <div className="flex w-full max-w-2xl flex-col gap-4">
              <div className="flex justify-center gap-2">
                <button type="button" aria-pressed={voice.visualSource === "camera"}
                  onClick={() => void voice.toggleVisual("camera")}
                  className="rounded-full border border-line px-4 py-2 text-sm hover:bg-line/50 focus-visible:ring-2 focus-visible:ring-accent">
                  {voice.visualSource === "camera" ? "Stop camera" : "Share camera"}
                </button>
                <button type="button" aria-pressed={voice.visualSource === "screen"}
                  onClick={() => void voice.toggleVisual("screen")}
                  className="rounded-full border border-line px-4 py-2 text-sm hover:bg-line/50 focus-visible:ring-2 focus-visible:ring-accent">
                  {voice.visualSource === "screen" ? "Stop sharing" : "Share screen"}
                </button>
              </div>
              <div className="max-h-24 w-full overflow-y-auto">
                <VoiceCaptions captions={captions} />
              </div>
              <VoiceControls
                onExit={handleExit}
                onEnd={handleEnd}
                showEndControl={true}
                muted={voice.muted}
                onToggleMute={voice.toggleMute}
              />
            </div>
          ) : (
            // While connecting, still expose an exit control so the user is
            // never trapped in a blank immersive view.
            <VoiceControls
              onExit={handleExit}
              onEnd={handleEnd}
              showEndControl={showConnecting}
            />
          )}
        </div>
      )}

      {/* A focusable anchor the trap can land on first, kept off-screen but
          reachable so initial focus is deterministic even before controls
          mount (e.g. during the connecting phase). (Req 9.1.) */}
      <button
        ref={initialFocusRef}
        type="button"
        className="sr-only"
        aria-label="Immersive live voice controls"
        onClick={handleExit}
      >
        Exit immersive view
      </button>

      {/* Live-region indication that the session ended unexpectedly (Req 2.6).
          Rendered briefly before the overlay unmounts on close. */}
      {sessionEnded ? (
        <p className="sr-only" role="status" aria-live="assertive">
          The voice session has ended.
        </p>
      ) : null}
    </div>
  );

  if (typeof document === "undefined") return overlay;
  return createPortal(<><div className="fixed inset-0 z-[79] bg-black/40 backdrop-blur-sm" aria-hidden="true" />{overlay}</>, document.body);
}

export default ImmersiveVoiceOverlay;
