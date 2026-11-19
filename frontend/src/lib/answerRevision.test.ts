import { expect, it } from "vitest";
import fc from "fast-check";
import { applyLiveNarrativeSnapshot, applyStreamFrameToMessage, messageNarrativeText, replayStreamFrames } from "./transcript";

it.each(["Corrected.", "Corrected.\n\nA longer explanation with a source and a limitation. 😀"])(
  "keeps one canonical answer after stale journal and live snapshots: %s", replacement => {
    const prefix = "Checking the source.\n\n";
    const original = "The initial answer has an incorrect rate and an unsupported qualifier.";
    const source = { kind: "assistant_narrative", payload: { text: prefix + original, offset: 0 } };
    const revision = { kind: "answer_revision", payload: { original, replacement } };
    let message = replayStreamFrames({ id: "a", role: "assistant", text: replacement }, [source, revision], replacement);
    message = applyStreamFrameToMessage(message, source);
    message = applyLiveNarrativeSnapshot(message, prefix + original);
    message = applyStreamFrameToMessage(message, revision);
    const final = replacement + "\n\nSources: [1].\nVerification remains incomplete.";
    message = replayStreamFrames(message, [], final);
    expect(messageNarrativeText(message)).toBe(prefix + final);
  },
);

it("does not apply a revision to an unrelated answer", () => {
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "Other answer" }, [], "Other answer");
  expect(applyStreamFrameToMessage(message, { kind: "answer_revision", payload: {
    original: "Different turn", replacement: "Replacement",
  } })).toBe(message);
});

it.each([0, 1, 9, 20])("a correction replaces a partially received draft of length %s", length => {
  const prefix = "Progress 😀\n\n";
  const original = "The original answer contains a wrong value.";
  const replacement = "The corrected answer is ready.";
  let message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    { kind: "assistant_narrative", payload: { text: prefix + original.slice(0, length), offset: 0 } },
    { kind: "answer_revision", payload: { original, replacement, offset: prefix.length } },
  ], replacement);
  message = applyStreamFrameToMessage(message, { kind: "assistant_narrative", payload: {
    text: original, offset: prefix.length,
  } });
  expect(messageNarrativeText(message)).toBe(prefix + replacement);
});

it("replay is idempotent across chunk boundaries, Unicode and shortened corrections", () => {
  fc.assert(fc.property(fc.string({ maxLength: 300 }), fc.array(fc.integer({ min: 1, max: 20 }), { minLength: 1, maxLength: 20 }), (detail, sizes) => {
    const prefix = "Progress 😀\n\n";
    const original = "## Answer\n\n" + detail + "\nOriginal conclusion.";
    const replacement = "## Answer\n\nCorrected conclusion ✅.";
    const raw = prefix + original;
    const frames = [];
    let offset = 0;
    while (offset < raw.length) {
      const size = sizes[offset % sizes.length];
      frames.push({ kind: "assistant_narrative", payload: { text: raw.slice(offset, offset + size), offset } });
      offset += size;
    }
    const revision = { kind: "answer_revision", payload: { original, replacement } };
    const all = [...frames, revision];
    const first = replayStreamFrames({ id: "a", role: "assistant", text: replacement }, all, replacement);
    const second = replayStreamFrames(first, all, replacement);
    expect(messageNarrativeText(first)).toBe(prefix + replacement);
    expect(messageNarrativeText(second)).toBe(prefix + replacement);
  }), { numRuns: Number(process.env.ANSWER_REVISION_RUNS || 200),
    ...(process.env.ANSWER_REVISION_SEED ? { seed: Number(process.env.ANSWER_REVISION_SEED) } : {}) });
});
