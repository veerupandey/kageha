import { describe, expect, it } from "vitest";
import { mergeTerminalNarrative } from "./narrativeText";
import { applyTranscriptToMessage, replayStreamFrames, messageNarrativeText, applyStreamFrameToMessage } from "./transcript";

it("replays an inspection revision once while keeping earlier progress", () => {
  const frames = [
    { kind: "assistant_narrative", payload: { text: "Reading.\n\nThe rate is 3%.", offset: 0 } },
    { kind: "answer_revision", payload: { original: "The rate is 3%.", replacement: "The rate is 4%." } },
  ];
  const restored = replayStreamFrames({ id: "a", role: "assistant", text: "The rate is 4%." }, frames, "The rate is 4%.");
  expect(messageNarrativeText(restored)).toBe("Reading.\n\nThe rate is 4%.");
  expect(messageNarrativeText(applyStreamFrameToMessage(restored, frames[1])))
    .toBe("Reading.\n\nThe rate is 4%.");
});

const narration = (message: ReturnType<typeof replayStreamFrames>) =>
  (message.blocks || []).map((b) => b.type === "narrative" ? b.text : "").join("");

it("reconciles a masked answer after progress when the terminal snapshot adds a footer", () => {
  const progress = "Checking the saved application screenshots.";
  const answer = "## Application result\n\nThe application did not submit; verification failed.\n\n"
    + "## Recommended next role\n\n[Posting](https://www.[REDACTED]/) · Hybrid\n\n"
    + "This is the strongest remaining match based on the saved posting requirements.";
  const footer = "\n\n---\n_Criterion verification is incomplete._";
  const final = answer.replace("[REDACTED]", "example.com/jobs/123") + footer;
  const expected = progress + answer + footer;
  expect(mergeTerminalNarrative(progress + answer, final)).toBe(expected);
  const frames = [
    { kind: "stream_item", payload: { item_id: "progress", channel: "text", state: "completed", text: progress, base_offset: 0 } },
    { kind: "tool_completed", card: { id: "inspect", name: "image_analyze", status: "done" } },
    { kind: "stream_item", payload: { item_id: "answer", channel: "text", state: "completed", text: answer, base_offset: progress.length } },
  ];
  const restored = replayStreamFrames({ id: "a", role: "assistant", text: final }, frames, final);
  expect(messageNarrativeText(restored)).toBe(expected);
  expect(restored.blocks?.some(b => b.type === "tool")).toBe(true);
  expect(messageNarrativeText(replayStreamFrames(restored, [], final))).toBe(expected);
});

describe("terminal narration reconciliation", () => {
  it("does not duplicate a journal answer when only its path was redacted", () => {
    const saved = "Deliverable: `artifacts/jobfeeds/futurefitai.json`\n[REDACTED]\nAlso: `artifacts/jobfeeds/glia.json`";
    const final = saved.replace("[REDACTED]", "/Users/example/.kageha/sessions/123/artifacts/jobfeeds/futurefitai.json");
    expect(mergeTerminalNarrative(saved, final)).toBe(saved);
    expect(mergeTerminalNarrative(final, saved)).toBe(saved);
    expect(mergeTerminalNarrative(saved, final + "\nThe model timed out.")).toContain("The model timed out.");
  });

  it("keeps masks and treats punctuation literally when comparing snapshots", () => {
    const saved = "Result (v1): key=[REDACTED]";
    expect(mergeTerminalNarrative(saved, "Result (v1): key=private")).toBe(saved);
    expect(mergeTerminalNarrative(saved, "Different result")).toContain("Different result");
  });
  it.each(["completed", "failed", "cancelled"])("restores the terminal answer from a %s journal frame", (kind) => {
    const live = applyTranscriptToMessage({ id: "a", role: "assistant", text: "" },
      { type: "narrative", assembled: "Full reading" });
    const restored = applyStreamFrameToMessage(live, {
      kind, payload: { status: "no_progress", message: "Verification remains unresolved." },
    });
    expect(messageNarrativeText(restored)).toBe("Full reading\n\nVerification remains unresolved.");
    expect(restored.text).toBe("Verification remains unresolved.");
  });
  it("copies the preserved answer rather than only the canonical recap", () => {
    const restored = replayStreamFrames({ id: "a", role: "assistant", text: "Recap" }, [
      { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Full reading", offset: 0 } },
    ], "Recap");
    expect(messageNarrativeText(restored)).toBe("Full reading\n\nRecap");
    expect(messageNarrativeText({ id: "old", role: "assistant", text: "Legacy answer" })).toBe("Legacy answer");
  });
  it.each([
    ["", "Answer", "Answer"],
    ["Answer", "", "Answer"],
    ["Answer", "  ", "Answer"],
    ["Answer", "Answer", "Answer"],
    ["Answer", "Answer continued", "Answer continued"],
    ["Answer continued", "Answer", "Answer continued"],
    ["Detailed answer.\n\nRecap", "Recap", "Detailed answer.\n\nRecap"],
    ["Detailed answer.", "Recap", "Detailed answer.\n\nRecap"],
    ["Draft", "Final", "Draft\n\nFinal"],
    ["Short", "Different longer answer", "Short\n\nDifferent longer answer"],
    ["Reading 😀", "Reading 😀", "Reading 😀"],
  ])("merges %j and %j", (saved, final, expected) => {
    expect(mergeTerminalNarrative(saved, final)).toBe(expected);
  });

  it.each(["Final", "Different longer answer", "Draft extended", "", "Draft"])(
    "finish and history replay preserve narration consistently for %j", (final) => {
      const live = applyTranscriptToMessage({ id: "a", role: "assistant", text: "" },
        { type: "narrative", assembled: "Draft" });
      const finished = applyTranscriptToMessage(live, { type: "finish", assembled: final });
      const restored = replayStreamFrames({ id: "a", role: "assistant", text: final }, [
        { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Draft", offset: 0 } },
      ], final);
      expect(narration(finished)).toBe(mergeTerminalNarrative("Draft", final));
      expect(narration(restored)).toBe(narration(finished));
      expect(narration(replayStreamFrames(restored, [], final))).toBe(narration(restored));
    },
  );
});

it("does not duplicate narration when the final snapshot adds an inspection label", () => {
  const first = "The initial answer with substantive detail.";
  const second = "The additional observation with verified detail.";
  const final = first + "\n\nInspection: " + second;
  expect(mergeTerminalNarrative(first + second, final)).toBe(final);
  const restored = replayStreamFrames({ id: "a", role: "assistant", text: final }, [
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: first, offset: 0 } },
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: second, offset: first.length } },
  ], final);
  expect(messageNarrativeText(restored)).toBe(final);
});

it("keeps one answer when inspection formatting and path redaction change together", () => {
  const answer = "# Architecture lesson\n\n" + "Detailed explanation of the architecture. ".repeat(80);
  const inspection = "Inspection confirmed the figures at `[REDACTED]`.\n\n" + "All seven figures are readable and have the expected dimensions. ".repeat(5);
  const saved = answer + inspection;
  const final = answer + "\n\nInspection: " + inspection.replace("[REDACTED]", "/Users/example/session/artifacts/");
  expect(mergeTerminalNarrative(saved, final)).toBe(saved);
  const restored = replayStreamFrames({ id: "a", role: "assistant", text: final }, [
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: answer, offset: 0 } },
    { kind: "tool_completed", card: { id: "verify", name: "inspect_result", status: "done" } },
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: inspection, offset: answer.length } },
  ], final);
  expect(messageNarrativeText(restored)).toBe(saved);
  expect(messageNarrativeText(replayStreamFrames(restored, [], final))).toBe(saved);
  expect(restored.blocks?.some((b) => b.type === "tool")).toBe(true);
});

it("keeps one lesson when a preamble precedes it and the final adds inspection formatting and sources", () => {
  const preamble = "I'll check the sources before teaching this.\n\n";
  const answer = "# Architecture lesson\n\n" + "A detailed explanation of transformers and vehicle damage. ".repeat(8);
  const inspection = "Inspection confirmed the architecture claims against the original sources.";
  const final = answer + "\n\nInspection: " + inspection + "\n\n## Sources\n[1] Original paper";
  const saved = preamble + answer + inspection;
  expect(mergeTerminalNarrative(saved, final)).toBe(preamble + final);
  const restored = replayStreamFrames({ id: "a", role: "assistant", text: final }, [
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: preamble, offset: 0 } },
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: answer, offset: preamble.length } },
    { kind: "tool_completed", card: { id: "verify", name: "inspect_result", status: "done" } },
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: inspection, offset: preamble.length + answer.length } },
  ], final);
  expect(messageNarrativeText(restored)).toBe(preamble + final);
  expect(messageNarrativeText(replayStreamFrames(restored, [], final))).toBe(preamble + final);
  expect(restored.blocks?.some((b) => b.type === "tool")).toBe(true);
});

it("shows a streamed answer once when the terminal version wraps it in artifacts and sources", () => {
  const progress = "Navigation succeeded. Capturing the screen.";
  const answer = "Here is the browser showing the discussion community:\n\n![Discussion](artifacts/page.jpg)\n\n### What people are discussing\n\nA detailed observed discussion summary with several concrete examples.";
  const final = "Deliverable: `artifacts/page.jpg`\n\n" + answer + "\n\n## Sources\n[1] Example";
  const restored = replayStreamFrames({ id: "a", role: "assistant", text: final }, [
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: progress, offset: 0 } },
    { kind: "tool_completed", card: { id: "capture", name: "computer_observe", status: "done" } },
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: answer, offset: progress.length } },
  ], final);
  const text = messageNarrativeText(restored);
  expect(text.split("Here is the browser")).toHaveLength(2);
  expect(text.split("![Discussion]")).toHaveLength(2);
  expect(text).toContain(progress);
  expect(text).toContain("Deliverable:");
  expect(text).toContain("## Sources");
  expect(restored.blocks?.some((b) => b.type === "tool")).toBe(true);
  expect(messageNarrativeText(replayStreamFrames(restored, [], final))).toBe(text);
});

it("replaces an exact answer suffix even when prior narration has no separator", () => {
  const answer = "Here is the visible page and its discussion. " + "Observed detail. ".repeat(12);
  const saved = "Taking a screenshot now." + answer;
  const final = "Deliverable: page.png\n\n" + answer + "\n\nSources: example";
  const result = mergeTerminalNarrative(saved, final);
  expect(result).toBe("Taking a screenshot now." + final);
});
