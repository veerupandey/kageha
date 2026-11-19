import { useCallback, useEffect, useRef, useState } from "react";
import {
  startLiveSpeechRecognition,
  startMicRecording,
  supportsLiveSpeechRecognition,
  stopSpokenReply,
  transcribeBlob,
} from "../lib/voiceClient";
import { useAppStore } from "../store";

interface ComposerControllerOptions {
  draft: string;
  setDraft: (value: string) => void;
  onSubmitted?: () => void;
}

export function useComposerController({
  draft,
  setDraft,
  onSubmitted,
}: ComposerControllerOptions) {
  const sendMessage = useAppStore((s) => s.sendMessage);
  const sendNowMessage = useAppStore((s) => s.sendNowMessage);
  const steerMessage = useAppStore((s) => s.steerMessage);
  const sending = useAppStore((s) => s.sending);
  const stopGeneration = useAppStore((s) => s.stopGeneration);
  const sessionId = useAppStore((s) => s.sessionId);
  const pendingFiles = useAppStore((s) => s.pendingFiles);
  const addPendingFiles = useAppStore((s) => s.addPendingFiles);
  const removePendingFile = useAppStore((s) => s.removePendingFile);
  const showToast = useAppStore((s) => s.showToast);
  const micStopRef = useRef<null | (() => Promise<Blob>)>(null);
  const liveMicStopRef = useRef<null | (() => Promise<string>)>(null);
  const liveMicBaseRef = useRef("");
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const micBusyRef = useRef(false);
  const silenceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const relistenTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const clearSilence = () => { if (silenceRef.current) clearTimeout(silenceRef.current); silenceRef.current = null; };
  const voiceConversation = useAppStore((s) => s.prefs.voiceConversation);
  const setVoiceRelisten = useAppStore((s) => s.setVoiceRelisten);
  const handleMicRef = useRef<() => void>(() => {});

  useEffect(() => {
    return () => {
      clearSilence();
      if (relistenTimerRef.current) clearTimeout(relistenTimerRef.current);
      if (micStopRef.current) void micStopRef.current().catch(() => {});
      if (liveMicStopRef.current) void liveMicStopRef.current().catch(() => {});
      micStopRef.current = null;
      liveMicStopRef.current = null;
      stopSpokenReply();
    };
  }, []);

  const submit = useCallback(
    (textOverride?: string, options?: { allowWhileSending?: boolean; delivery?: "queue" | "steer" | "now" }) => {
      const text = (textOverride ?? draft).trim();
      if (recording || transcribing || (!text && !pendingFiles.length)) {
        return false;
      }
      if (textOverride !== undefined) setDraft(text);
      const send = sending && options?.delivery === "now" ? sendNowMessage : sending && options?.delivery === "steer" ? steerMessage : sendMessage;
      void send(textOverride !== undefined ? text : undefined);
      onSubmitted?.();
      return true;
    },
    [draft, onSubmitted, pendingFiles.length, sendMessage, steerMessage, sendNowMessage, sending, setDraft, recording, transcribing],
  );

  const handleMic = useCallback(() => {
    void (async () => {
      if (micBusyRef.current || transcribing) return;
      if (!sessionId && !supportsLiveSpeechRecognition()) {
        showToast("Start a thread to use voice input in this browser.");
        return;
      }
      micBusyRef.current = true;
      if (recording && (micStopRef.current || liveMicStopRef.current)) {
        clearSilence();
        setRecording(false);
        setTranscribing(true);
        try {
          let text = "";
          let baseText = draft.trim();
          if (liveMicStopRef.current) {
            baseText = liveMicBaseRef.current;
            text = await liveMicStopRef.current();
            liveMicStopRef.current = null;
          } else if (micStopRef.current) {
            const blob = await micStopRef.current();
            micStopRef.current = null;
            text = await transcribeBlob(sessionId!, blob);
          }
          if (text) {
            const next = [baseText, text].filter(Boolean).join(" ");
            setDraft(next);
            // Hands-free conversation mode: auto-send the transcript instead
            // of waiting for manual review (Codex/Perplexity-style). Call
            // sendMessage directly so the just-cleared recording/transcribing
            // state in this closure doesn't block submit's guards.
            if (useAppStore.getState().prefs.voiceConversation) {
              setTranscribing(false);
              micBusyRef.current = false;
              if (next.trim() && !useAppStore.getState().sending) {
                setDraft(next);
                void sendMessage(next);
                onSubmitted?.();
              }
              return;
            }
            showToast("Transcript ready — review it, then send.");
          } else {
            showToast("No speech detected");
          }
        } catch (err) {
          showToast(`Mic: ${err instanceof Error ? err.message : err}`);
        } finally {
          setTranscribing(false);
          micBusyRef.current = false;
        }
        return;
      }
      try {
        stopSpokenReply();
        liveMicBaseRef.current = draft;
        if (supportsLiveSpeechRecognition()) {
          liveMicBaseRef.current = draft.trim();
          const live = startLiveSpeechRecognition((finalText, interimText) => {
            const spoken = [finalText, interimText].filter(Boolean).join(" ");
            setDraft([liveMicBaseRef.current, spoken].filter(Boolean).join(" "));
            clearSilence();
            if (spoken.trim() && useAppStore.getState().prefs.voiceConversation) {
              silenceRef.current = setTimeout(() => {
                if (useAppStore.getState().prefs.voiceConversation && liveMicStopRef.current) handleMicRef.current();
              }, 1400);
            }
          }, () => {
            if (liveMicStopRef.current) handleMicRef.current();
          });
          liveMicStopRef.current = live.stop;
        } else {
          const rec = await startMicRecording();
          micStopRef.current = rec.stop;
        }
        setRecording(true);
        showToast(
          supportsLiveSpeechRecognition()
            ? "Listening… words will appear as you speak"
            : "Listening… finish to review your transcript",
        );
      } catch (err) {
        showToast(`Mic: ${err instanceof Error ? err.message : err}`);
      } finally {
        micBusyRef.current = false;
      }
    })();
  }, [draft, recording, sessionId, setDraft, showToast, transcribing, sendMessage, onSubmitted]);

  // Keep a stable ref to handleMic so the hands-free loop can reopen the mic
  // after a spoken reply without re-registering on every render.
  useEffect(() => {
    handleMicRef.current = handleMic;
  }, [handleMic]);

  useEffect(() => {
    if (!voiceConversation) {
      clearSilence();
      if (relistenTimerRef.current) clearTimeout(relistenTimerRef.current);
      setVoiceRelisten(null);
      return;
    }
    setVoiceRelisten(() => {
      // Small delay so the audio device is released before re-opening.
      if (relistenTimerRef.current) clearTimeout(relistenTimerRef.current);
      const expectedSession = useAppStore.getState().sessionId;
      relistenTimerRef.current = setTimeout(() => {
        const state = useAppStore.getState();
        if (state.prefs.voiceConversation && state.sessionId === expectedSession && !state.sending && !micStopRef.current && !liveMicStopRef.current) handleMicRef.current();
      }, 400);
    });
    return () => { if (relistenTimerRef.current) clearTimeout(relistenTimerRef.current); setVoiceRelisten(null); };
  }, [voiceConversation, setVoiceRelisten]);

  const cancelRecording = useCallback(() => {
    clearSilence();
    if (micBusyRef.current) return;
    const stopLive = liveMicStopRef.current;
    const stopMic = micStopRef.current;
    liveMicStopRef.current = null;
    micStopRef.current = null;
    setRecording(false);
    setDraft(liveMicBaseRef.current);
    void (async () => {
      try { if (stopLive) await stopLive(); if (stopMic) await stopMic(); }
      catch { /* Discarded recording needs no transcription. */ }
      finally { setDraft(liveMicBaseRef.current); }
    })();
  }, [setDraft]);

  return {
    addPendingFiles,
    handleMic,
    cancelRecording,
    pendingFiles,
    recording,
    removePendingFile,
    sending,
    sessionId,
    stopGeneration,
    submit,
    transcribing,
  };
}
