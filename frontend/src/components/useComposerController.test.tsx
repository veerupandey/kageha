import { act, renderHook, waitFor, cleanup } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useComposerController } from "./useComposerController";
import { useAppStore } from "../store";
import { startLiveSpeechRecognition, supportsLiveSpeechRecognition } from "../lib/voiceClient";

vi.mock("../lib/voiceClient", () => ({
  startLiveSpeechRecognition: vi.fn(), supportsLiveSpeechRecognition: vi.fn(() => true),
  stopSpokenReply: vi.fn(), startMicRecording: vi.fn(), transcribeBlob: vi.fn(),
}));

describe("reviewable dictation", () => {
  const send = vi.fn();
  const setDraft = vi.fn();
  const stop = vi.fn(async () => "spoken words");
  beforeEach(() => {
    vi.clearAllMocks();
    useAppStore.setState(s => ({ prefs: { ...s.prefs, voiceConversation: false, voiceReply: false } }));
    vi.mocked(supportsLiveSpeechRecognition).mockReturnValue(true);
    vi.mocked(startLiveSpeechRecognition).mockReturnValue({ stop });
    useAppStore.setState({ sessionId: "voice-test", sending: false, pendingFiles: [], sendMessage: send, showToast: vi.fn() });
  });
  afterEach(() => { cleanup(); vi.useRealTimers(); });
  it("finishes into a draft without sending", async () => {
    const { result } = renderHook(() => useComposerController({ draft: "My idea", setDraft }));
    act(() => result.current.handleMic());
    expect(result.current.recording).toBe(true);
    act(() => { expect(result.current.submit()).toBe(false); });
    await act(async () => result.current.handleMic());
    await waitFor(() => expect(result.current.transcribing).toBe(false));
    expect(setDraft).toHaveBeenLastCalledWith("My idea spoken words");
    expect(send).not.toHaveBeenCalled();
  });
  it("queues Enter submissions while running and routes explicit steering separately", () => {
    const steer = vi.fn();
    useAppStore.setState({ sending: true, steerMessage: steer });
    const { result } = renderHook(() => useComposerController({ draft: "Change direction", setDraft }));
    act(() => { expect(result.current.submit()).toBe(true); });
    expect(send).toHaveBeenCalledTimes(1);
    expect(steer).not.toHaveBeenCalled();
    act(() => { expect(result.current.submit(undefined, { delivery: "steer" })).toBe(true); });
    expect(steer).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledTimes(1);
  });
  it("cancels and restores the original draft", async () => {
    const { result } = renderHook(() => useComposerController({ draft: "Keep this", setDraft }));
    act(() => result.current.handleMic());
    await act(async () => result.current.cancelRecording());
    expect(result.current.recording).toBe(false);
    expect(setDraft).toHaveBeenLastCalledWith("Keep this");
    expect(send).not.toHaveBeenCalled();
  });

  it("auto-sends the transcript in hands-free voice conversation mode", async () => {
    useAppStore.setState((s) => ({ prefs: { ...s.prefs, voiceConversation: true } }));
    const { result } = renderHook(() => useComposerController({ draft: "", setDraft }));
    act(() => result.current.handleMic());
    expect(result.current.recording).toBe(true);
    await act(async () => result.current.handleMic());
    await waitFor(() => expect(send).toHaveBeenCalled());
    // Auto-sent the spoken words without waiting for manual review.
    expect(send).toHaveBeenCalledWith("spoken words");
    act(() => useAppStore.setState((s) => ({ prefs: { ...s.prefs, voiceConversation: false } })));
  });
  it("auto-sends browser speech after a pause", async () => {
    vi.useFakeTimers();
    useAppStore.setState(s => ({ prefs: { ...s.prefs, voiceConversation: true } }));
    const { result } = renderHook(() => useComposerController({ draft: "", setDraft }));
    act(() => result.current.handleMic());
    const update = vi.mocked(startLiveSpeechRecognition).mock.calls.at(-1)![0];
    act(() => update("spoken words", ""));
    await act(async () => { await vi.advanceTimersByTimeAsync(1400); });
    expect(send).toHaveBeenCalledWith("spoken words");
    expect(stop).toHaveBeenCalled();
  });

  it("does not reopen the mic if conversation mode is disabled during the relisten delay", async () => {
    vi.useFakeTimers();
    useAppStore.setState(s => ({ prefs: { ...s.prefs, voiceConversation: true } }));
    renderHook(() => useComposerController({ draft: "", setDraft }));
    act(() => useAppStore.getState().voiceRelisten?.());
    act(() => useAppStore.setState(s => ({ prefs: { ...s.prefs, voiceConversation: false } })));
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    expect(startLiveSpeechRecognition).not.toHaveBeenCalled();
  });

});
