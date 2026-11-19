import { afterEach, describe, expect, it, vi } from "vitest";
import {
  plainSpeakable,
  speakText,
  stopSpokenReply,
  startLiveSpeechRecognition,
  supportsLiveSpeechRecognition,
} from "./voiceClient";

class FakeRecognition {
  static current: FakeRecognition | null = null;
  continuous = false;
  interimResults = false;
  lang = "";
  onerror: ((event: { error?: string }) => void) | null = null;
  onend: (() => void) | null = null;
  onresult:
    | ((event: {
        resultIndex: number;
        results: ArrayLike<{ isFinal: boolean; 0: { transcript: string } }>;
      }) => void)
    | null = null;

  constructor() {
    FakeRecognition.current = this;
  }

  start() {}

  stop() {
    this.onend?.();
  }

  emit(
    resultIndex: number,
    results: ArrayLike<{ isFinal: boolean; 0: { transcript: string } }>,
  ) {
    this.onresult?.({ resultIndex, results });
  }
}

afterEach(() => {
  delete (window as Window & { SpeechRecognition?: unknown }).SpeechRecognition;
  FakeRecognition.current = null;
});

describe("plainSpeakable", () => {
  it("strips markdown for TTS", () => {
    expect(
      plainSpeakable("## Hello\n\nUse `gemini_tts` and [docs](https://x.test)"),
    ).toBe("Hello Use gemini_tts and docs");
  });

  it("drops fenced code", () => {
    expect(plainSpeakable("Hi\n```js\nconsole.log(1)\n```\nBye")).toBe(
      "Hi Bye",
    );
  });
});

describe("live speech recognition", () => {
  it("reports interim text and returns only finalized speech", async () => {
    Object.defineProperty(window, "SpeechRecognition", {
      configurable: true,
      value: FakeRecognition,
    });
    const updates: Array<[string, string]> = [];
    const session = startLiveSpeechRecognition((finalText, interimText) => {
      updates.push([finalText, interimText]);
    });
    const recognition = FakeRecognition.current!;
    expect(supportsLiveSpeechRecognition()).toBe(true);
    expect(recognition.continuous).toBe(true);
    expect(recognition.interimResults).toBe(true);

    recognition.emit(0, [{ isFinal: false, 0: { transcript: "hello" } }]);
    expect(updates.at(-1)).toEqual(["", "hello"]);
    recognition.emit(0, [{ isFinal: true, 0: { transcript: "hello world" } }]);
    await expect(session.stop()).resolves.toBe("hello world");
  });

  it("does not duplicate finalized results when the browser replays them", async () => {
    Object.defineProperty(window, "SpeechRecognition", {
      configurable: true,
      value: FakeRecognition,
    });
    const session = startLiveSpeechRecognition(() => {});
    const recognition = FakeRecognition.current!;
    recognition.emit(0, [{ isFinal: true, 0: { transcript: "first" } }]);
    recognition.emit(1, [
      { isFinal: true, 0: { transcript: "first" } },
      { isFinal: true, 0: { transcript: "second" } },
    ]);
    await expect(session.stop()).resolves.toBe("first second");
  });
  it("reports natural microphone completion and retains errors until stop", async () => {
    Object.defineProperty(window, "SpeechRecognition", { configurable: true, value: FakeRecognition });
    const ended = vi.fn();
    const session = startLiveSpeechRecognition(() => {}, ended);
    FakeRecognition.current!.onerror?.({ error: "not-allowed" });
    expect(ended).toHaveBeenCalledOnce();
    await expect(session.stop()).rejects.toThrow("not-allowed");
  });

});

// Regression: Audio.play() resolves when playback starts, not when it finishes.
describe("spoken reply lifecycle", () => {
  afterEach(() => vi.unstubAllGlobals());
  function playback() {
    class Player {
      static current: Player;
      src: string;
      onended: (() => void) | null = null;
      onerror: (() => void) | null = null;
      pause = vi.fn();
      play = vi.fn(async () => {});
      constructor(url: string) { this.src = url; Player.current = this; }
    }
    vi.stubGlobal("Audio", Player);
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, blob: async () => new Blob(["audio"]) })));
    vi.stubGlobal("URL", { createObjectURL: vi.fn(() => "blob:reply"), revokeObjectURL: vi.fn() });
    return Player;
  }
  it("waits until the audio ends before allowing hands-free relistening", async () => {
    const Player = playback();
    let complete = false;
    const speech = speakText("session", "Hello").then(() => { complete = true; });
    await vi.waitFor(() => expect(Player.current?.play).toHaveBeenCalled());
    expect(complete).toBe(false);
    Player.current.onended?.();
    await speech;
    expect(complete).toBe(true);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:reply");
  });
  it("cancels playback instead of treating cancellation as a completed reply", async () => {
    const Player = playback();
    const speech = speakText("session", "Hello");
    const cancelled = expect(speech).rejects.toMatchObject({ name: "AbortError" });
    await vi.waitFor(() => expect(Player.current?.play).toHaveBeenCalled());
    stopSpokenReply();
    await cancelled;
    expect(Player.current.pause).toHaveBeenCalled();
    expect(URL.revokeObjectURL).toHaveBeenCalled();
  });
  it("propagates audio errors", async () => {
    const Player = playback();
    const speech = speakText("session", "Hello");
    const failed = expect(speech).rejects.toThrow("could not be played");
    await vi.waitFor(() => expect(Player.current?.play).toHaveBeenCalled());
    Player.current.onerror?.();
    await failed;
  });
});
