import { describe, expect, it } from "vitest";
import { downsampleBuffer, floatTo16BitPCM, rmsLevel } from "./geminiLiveClient";

describe("floatTo16BitPCM", () => {
  it("converts full-scale positive and negative samples correctly", () => {
    const input = new Float32Array([1, -1, 0]);
    const view = new DataView(floatTo16BitPCM(input));
    expect(view.getInt16(0, true)).toBe(0x7fff);
    expect(view.getInt16(2, true)).toBe(-0x8000);
    expect(view.getInt16(4, true)).toBe(0);
  });

  it("clamps values outside [-1, 1] instead of overflowing", () => {
    const input = new Float32Array([2.5, -3.1]);
    const view = new DataView(floatTo16BitPCM(input));
    expect(view.getInt16(0, true)).toBe(0x7fff);
    expect(view.getInt16(2, true)).toBe(-0x8000);
  });

  it("produces exactly 2 bytes per sample, little-endian", () => {
    const input = new Float32Array(10).fill(0.5);
    const buffer = floatTo16BitPCM(input);
    expect(buffer.byteLength).toBe(20);
  });
});

describe("downsampleBuffer", () => {
  it("returns the input unchanged when rates already match", () => {
    const input = new Float32Array([0.1, 0.2, 0.3]);
    const out = downsampleBuffer(input, 16_000, 16_000);
    expect(out).toBe(input);
  });

  it("halves sample count when downsampling 2:1", () => {
    // 48kHz capture (common browser default) -> 16kHz Gemini input rate is 3:1.
    const input = new Float32Array([0, 1, 0, 1, 0, 1, 0, 1]);
    const out = downsampleBuffer(input, 32_000, 16_000);
    expect(out.length).toBe(4);
  });

  it("averages samples within each output bucket rather than dropping them", () => {
    const input = new Float32Array([0, 1, 0, 1]); // avg = 0.5 for a 4:1 -> 1 bucket
    const out = downsampleBuffer(input, 4, 1);
    expect(out.length).toBe(1);
    expect(out[0]).toBeCloseTo(0.5, 5);
  });

  it("never produces NaN when the ratio leaves a partial trailing bucket", () => {
    const input = new Float32Array(17).fill(0.25);
    const out = downsampleBuffer(input, 48_000, 16_000);
    expect(out.every((v) => Number.isFinite(v))).toBe(true);
  });
});

describe("rmsLevel", () => {
  it("is 0 for silence", () => {
    expect(rmsLevel(new Float32Array(100))).toBe(0);
  });

  it("is 0 for an empty chunk (never divides by zero)", () => {
    expect(rmsLevel(new Float32Array(0))).toBe(0);
  });

  it("is 1 for a constant full-scale signal", () => {
    const chunk = new Float32Array(50).fill(1);
    expect(rmsLevel(chunk)).toBeCloseTo(1, 5);
  });

  it("is higher for louder input, driving real amplitude-based animation", () => {
    const quiet = new Float32Array(50).fill(0.05);
    const loud = new Float32Array(50).fill(0.5);
    expect(rmsLevel(loud)).toBeGreaterThan(rmsLevel(quiet));
  });

  it("treats positive and negative samples of equal magnitude the same", () => {
    const positive = new Float32Array(10).fill(0.3);
    const negative = new Float32Array(10).fill(-0.3);
    expect(rmsLevel(positive)).toBeCloseTo(rmsLevel(negative), 6);
  });
});
