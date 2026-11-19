import { createContext, useContext, type ReactNode } from "react";
import { useGeminiLiveVoice } from "./useGeminiLiveVoice";

type GeminiLiveVoiceValue = ReturnType<typeof useGeminiLiveVoice>;

const GeminiLiveVoiceContext = createContext<GeminiLiveVoiceValue | null>(null);

/**
 * Provides a SINGLE shared `useGeminiLiveVoice()` instance across the whole
 * app shell, so the live-voice session survives the CommandCenter (home) <->
 * ThreadView remount.
 *
 * Without this, clicking "Live" from the home screen (no thread yet) would:
 * mint a session via `ensureSession()` -> the store's `sessionId` becomes
 * non-null -> `AppShell`'s `isHome` flips to false -> `CommandCenter` (and the
 * `useGeminiLiveVoice()` instance living inside its `MiniComposer`) unmounts
 * *before* the pending `liveVoice.start(mintedSessionId)` call could run —
 * so `start()` fired on an already-unmounted hook instance and silently did
 * nothing. Lifting the hook up to `AppShell` (mounted once, unconditionally)
 * and sharing it via context keeps the same instance alive across that
 * transition, matching the single-shared-session design (Req 1.2, 2.2).
 */
export function GeminiLiveVoiceProvider({ children }: { children: ReactNode }) {
  const voice = useGeminiLiveVoice();
  return (
    <GeminiLiveVoiceContext.Provider value={voice}>{children}</GeminiLiveVoiceContext.Provider>
  );
}

export function useSharedGeminiLiveVoice(): GeminiLiveVoiceValue {
  const ctx = useContext(GeminiLiveVoiceContext);
  if (!ctx) {
    throw new Error("useSharedGeminiLiveVoice must be used within GeminiLiveVoiceProvider");
  }
  return ctx;
}
