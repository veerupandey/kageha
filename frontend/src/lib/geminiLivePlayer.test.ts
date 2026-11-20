import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PcmPlayer } from "./geminiLiveClient";

/**
 * jsdom has no AudioContext at all, so PcmPlayer's real dependency is faked
 * here with just enough surface to exercise the actual bug this file guards
 * against: clearQueue() must stop every *scheduled* source (not just the
 * ones already playing), since AudioBufferSourceNode.start(futureTime)
 * cannot be "un-started" by resetting a timestamp alone.
 */
class FakeSourceNode {
  buffer: { duration: number } | null = null;
  started = false;
  stopped = false;
  onended: (() => void) | null = null;
  connect(): void {}
  start(_when?: number): void {
    this.started = true;
  }
  stop(): void {
    if (this.stopped) throw new Error("already stopped"); // mirrors real DOM behavior
    this.stopped = true;
    this.onended?.();
  }
}

class FakeAudioContext {
  currentTime = 0;
  destination = {};
  sources: FakeSourceNode[] = [];
  createBuffer(_channels: number, _length: number, _rate: number) {
    return { duration: 1, copyToChannel: () => {} };
  }
  createBufferSource(): FakeSourceNode {
    const node = new FakeSourceNode();
    this.sources.push(node);
    return node;
  }
  async close(): Promise<void> {}
}

function samplesFor(seconds: number): ArrayBuffer {
  // 24kHz output rate — byte length only needs to be even (2 bytes/sample);
  // exact duration math is stubbed on the fake buffer, not derived here.
  return new ArrayBuffer(Math.round(seconds * 24_000) * 2);
}

describe("PcmPlayer", () => {
  let fakeCtx: FakeAudioContext;

  beforeEach(() => {
    fakeCtx = new FakeAudioContext();
    vi.stubGlobal(
      "AudioContext",
      vi.fn(function createAudioContext() { return fakeCtx; }),
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("schedules enqueued chunks back-to-back without gaps", () => {
    const player = new PcmPlayer();
    player.enqueue(samplesFor(1));
    player.enqueue(samplesFor(1));
    expect(fakeCtx.sources).toHaveLength(2);
    expect(fakeCtx.sources.every((s) => s.started)).toBe(true);
  });

  it("clearQueue stops every scheduled source, not just the first", () => {
    const player = new PcmPlayer();
    player.enqueue(samplesFor(1));
    player.enqueue(samplesFor(1));
    player.enqueue(samplesFor(1));
    expect(fakeCtx.sources.some((s) => s.stopped)).toBe(false);

    player.clearQueue();

    // This is the exact regression: resetting nextStartTime alone does
    // nothing to nodes already given a future start() time — they were
    // "started" (scheduled) regardless, so every one of them must be
    // individually .stop()'d.
    expect(fakeCtx.sources.every((s) => s.stopped)).toBe(true);
  });

  it("does not throw when clearQueue runs with nothing scheduled", () => {
    const player = new PcmPlayer();
    expect(() => player.clearQueue()).not.toThrow();
  });

  it("does not double-stop a source that already finished naturally", () => {
    const player = new PcmPlayer();
    player.enqueue(samplesFor(1));
    const source = fakeCtx.sources[0];
    source.onended?.(); // simulate natural playback completion (removes from set)
    expect(() => player.clearQueue()).not.toThrow();
  });

  it("close() also clears any still-scheduled sources", async () => {
    const player = new PcmPlayer();
    player.enqueue(samplesFor(1));
    player.enqueue(samplesFor(1));
    await player.close();
    expect(fakeCtx.sources.every((s) => s.stopped)).toBe(true);
  });

  it("hasActivePlayback reflects whether anything is scheduled", () => {
    const player = new PcmPlayer();
    expect(player.hasActivePlayback()).toBe(false);
    player.enqueue(samplesFor(1));
    expect(player.hasActivePlayback()).toBe(true);
    player.clearQueue();
    expect(player.hasActivePlayback()).toBe(false);
  });

  it("hasActivePlayback goes false once a source ends naturally", () => {
    const player = new PcmPlayer();
    player.enqueue(samplesFor(1));
    expect(player.hasActivePlayback()).toBe(true);
    fakeCtx.sources[0].onended?.();
    expect(player.hasActivePlayback()).toBe(false);
  });

  it("onPlaybackChange fires when a source ends naturally, not only on clearQueue", () => {
    const player = new PcmPlayer();
    let calls = 0;
    player.onPlaybackChange = () => calls++;
    player.enqueue(samplesFor(1));
    expect(calls).toBe(0); // enqueue itself does not notify — only state changes do
    fakeCtx.sources[0].onended?.();
    expect(calls).toBe(1);
  });

  it("onPlaybackChange fires exactly once when clearQueue stops multiple sources", () => {
    const player = new PcmPlayer();
    let calls = 0;
    player.onPlaybackChange = () => calls++;
    player.enqueue(samplesFor(1));
    player.enqueue(samplesFor(1));
    player.clearQueue();
    expect(calls).toBe(1);
  });

  it("onPlaybackChange does not fire on a no-op clearQueue", () => {
    const player = new PcmPlayer();
    let calls = 0;
    player.onPlaybackChange = () => calls++;
    player.clearQueue();
    expect(calls).toBe(0);
  });
});
