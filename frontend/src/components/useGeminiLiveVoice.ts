import { useCallback, useEffect, useRef, useState } from "react";
import {
  startGeminiLiveSession,
  type GeminiLivePhase,
  type GeminiLiveSession,
} from "../lib/geminiLiveClient";
import { useAppStore } from "../store";
import { clamp01 } from "./voice/voiceCore";

export type GeminiLiveStatus = "idle" | "connecting" | "active" | "error";

/**
 * Live voice mode: a persistent duplex conversation with Gemini Live, distinct
 * from the existing push-to-talk mic (record clip -> STT -> send -> TTS ->
 * play). Delegated work happens server-side (see gemini_live.py); this hook
 * only owns starting/stopping the browser audio session.
 *
 * Mic level updates arrive on every audio chunk (tens of times per second) —
 * routing that through React state would cause a re-render storm on every
 * chunk. Following the same pattern AppShell.tsx already uses for continuous
 * viewport measurements: write the level straight to a CSS custom property
 * via a ref, and only use React state for the coarse `phase` (listening /
 * thinking / speaking), which changes far less often.
 */
export function useGeminiLiveVoice() {
  const showToast = useAppStore((s) => s.showToast);
  const [status, setStatus] = useState<GeminiLiveStatus>("idle");
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [muted, setMuted] = useState(false);
  const [visualSource, setVisualSource] = useState<"camera" | "screen" | null>(null);
  const [avatarUrl, setAvatarUrl] = useState<string | null>(null);
  const pendingRef = useRef<AbortController | null>(null);
  const [phase, setPhase] = useState<GeminiLivePhase>("idle");
  const sessionRef = useRef<GeminiLiveSession | null>(null);
  const levelTargetRef = useRef<HTMLButtonElement | null>(null);
  const lastPhaseRef = useRef<GeminiLivePhase>("idle");

  useEffect(() => {
    return () => {
      pendingRef.current?.abort();
      pendingRef.current = null;
      sessionRef.current?.stop();
      sessionRef.current = null;
    };
  }, []);

  const stop = useCallback(() => {
    pendingRef.current?.abort();
    pendingRef.current = null;
    lastPhaseRef.current = "idle";
    setMuted(false);
    setVisualSource(null);
    setAvatarUrl(null);
    setError(null);
    sessionRef.current?.stop();
    sessionRef.current = null;
    setStatus("idle");
    setActiveSessionId(null);
    setPhase("idle");
    // Reset the shared root mic-level so a stale value cannot deform a
    // re-opened orb before the next session publishes a fresh level.
    document.documentElement.style.setProperty("--ka-live-mic-level", "0");
  }, []);

  const start = useCallback((sessionIdOverride?: string) => {
    void (async () => {
      // Prefer an explicit override (a caller that just minted a session via
      // ensureSession() has the fresh id immediately, before this hook's own
      // subscribed `sessionId` has necessarily re-rendered this closure), then
      // fall back to the live store value rather than the possibly-stale
      // value this callback closed over — Zustand's set() does not retroactively
      // update an already-created callback reference.
      const activeSessionId = sessionIdOverride || useAppStore.getState().sessionId;
      if (!activeSessionId) {
        showToast("Start a thread to use live voice.");
        return;
      }
      if (sessionRef.current?.active || pendingRef.current) return;
      const attempt = new AbortController();
      pendingRef.current = attempt;
      const current = () => pendingRef.current === attempt && !attempt.signal.aborted;
      setError(null);
      setMuted(false);
      lastPhaseRef.current = "idle";
      setPhase("idle");
      setStatus("connecting");
      setActiveSessionId(activeSessionId);
      try {
        const session = await startGeminiLiveSession(activeSessionId, {
          signal: attempt.signal,
          onAvatarUrl: (url) => { if (current()) setAvatarUrl(url); },
          onVisualStop: () => { if (current()) setVisualSource(null); },
          onOpen: () => { if (current()) setStatus("active"); },
          onClose: () => {
            if (!current()) return;
            pendingRef.current = null;
            sessionRef.current = null;
            setStatus("idle");
            setActiveSessionId(null);
            setPhase("idle");
          },
          onError: (message) => {
            if (!current()) return;
            pendingRef.current = null;
            sessionRef.current = null;
            setError(message);
            showToast(message);
            setStatus("error");
            setActiveSessionId(null);
          },
          onPhase: (nextPhase, micLevel) => {
            if (!current()) return;
            const level = clamp01(micLevel).toFixed(3);
            // Retain the existing composer-button target for backward-compatible styling.
            levelTargetRef.current?.style.setProperty("--ka-live-mic-level", level);
            // Also publish on :root so any overlay/orb can read it by inheritance
            // without a second ref registration and without a per-chunk React render.
            document.documentElement.style.setProperty("--ka-live-mic-level", level);
            if (nextPhase !== lastPhaseRef.current) {
              lastPhaseRef.current = nextPhase;
              setPhase(nextPhase);
            }
          },
        });
        if (!current()) { session.stop(); return; }
        sessionRef.current = session;
      } catch (err) {
        if (!current()) return;
        pendingRef.current = null;
        setError(err instanceof Error ? err.message : "Could not start live voice");
        showToast(err instanceof Error ? err.message : "Could not start live voice");
        setStatus("error");
        setActiveSessionId(null);
      }
    })();
  }, [showToast]);

  const toggle = useCallback(() => {
    if (status === "active" || status === "connecting") {
      stop();
    } else {
      start();
    }
  }, [status, start, stop]);

  const toggleMute = useCallback(() => {
    const next = !muted;
    sessionRef.current?.setMuted?.(next);
    setMuted(next);
  }, [muted]);

  const toggleVisual = useCallback(async (source: "camera" | "screen") => {
    const session = sessionRef.current;
    if (!session?.active) return;
    if (visualSource === source) {
      session.stopVisual?.();
      setVisualSource(null);
      return;
    }
    try {
      await session.startVisual?.(source);
      setVisualSource(source);
    } catch (err) {
      showToast(err instanceof Error ? err.message : "Could not share video");
    }
  }, [visualSource, showToast]);

  return { status, phase, levelTargetRef, start, stop, toggle, error, muted, toggleMute, visualSource, toggleVisual, avatarUrl, activeSessionId };
}
