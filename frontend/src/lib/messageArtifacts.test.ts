import { describe, expect, it } from "vitest";
import type { ChatMessage } from "../api/types";
import {
  collectMessageToolCards,
  collectWrittenArtifactRefs,
  messageArtifactCardPaths,
  messageBubbleArtifactPaths,
} from "./messageArtifacts";

function assistant(partial: Partial<ChatMessage>): ChatMessage {
  return {
    id: "m1",
    role: "assistant",
    text: "",
    ...partial,
  };
}

describe("message artifact cards", () => {
  it("does not create a card from a narrative backtick mention", () => {
    const message = assistant({
      text: "e.g. `artifacts/kageha_sales_growth_research.md` if we had written one.",
    });
    expect(collectWrittenArtifactRefs(message)).toEqual([]);
    expect(
      messageArtifactCardPaths(message, ["artifacts/kageha_sales_growth_research.md"]),
    ).toEqual([]);
  });

  it("cards only this-turn writes that exist in the session listing", () => {
    const message = assistant({
      text: "See `artifacts/missing.md` and leftovers `analysis_and_plan.md`.",
      toolCards: [
        {
          id: "w1",
          name: "write_file",
          artifactRefs: ["artifacts/report.md", "artifacts/ghost.md"],
        },
      ],
    });
    expect(collectWrittenArtifactRefs(message)).toEqual([
      "artifacts/report.md",
      "artifacts/ghost.md",
    ]);
    expect(
      messageArtifactCardPaths(message, ["artifacts/report.md", "analysis_and_plan.md"]),
    ).toEqual(["artifacts/report.md"]);
  });

  it("picks up journal-replay tool blocks when toolCards are empty", () => {
    const message = assistant({
      blocks: [
        {
          type: "tool",
          id: "t1",
          card: { id: "w1", name: "write_file", artifactRefs: ["artifacts/deck.pptx"] },
        },
      ],
    });
    expect(
      messageArtifactCardPaths(message, ["artifacts/deck.pptx"]),
    ).toEqual(["artifacts/deck.pptx"]);
  });

  it("collects tool cards from journal blocks when toolCards is empty", () => {
    const message = assistant({
      blocks: [
        {
          type: "tool",
          id: "b1",
          card: { id: "grep1", name: "grep", argsPreview: "mode_policy" },
        },
      ],
    });
    expect(collectMessageToolCards(message).map((card) => card.name)).toEqual(["grep"]);
  });

  it("shows a user-paste image from inputs/ as a bubble card", () => {
    expect(
      messageBubbleArtifactPaths(
        {
          role: "user",
          attachments: ["inputs/clipboard-1.png"],
        },
        [],
      ),
    ).toEqual(["inputs/clipboard-1.png"]);
  });

  it("pins finished assistant deliverables from attachments, not the session gallery", () => {
    const message = assistant({
      streaming: false,
      attachments: ["artifacts/ch2_s5.jpg"],
      toolCards: [
        {
          id: "w1",
          name: "write_file",
          artifactRefs: ["artifacts/ch1_s1.jpg", "artifacts/ch2_s5.jpg"],
        },
      ],
    });
    expect(
      messageBubbleArtifactPaths(message, [
        "artifacts/ch1_s1.jpg",
        "artifacts/ch2_s5.jpg",
      ]),
    ).toEqual(["artifacts/ch2_s5.jpg"]);
  });

  it("uses tool writes only while the assistant is still streaming", () => {
    const message = assistant({
      streaming: true,
      toolCards: [
        {
          id: "w1",
          name: "nano_banana_edit",
          artifactRefs: ["artifacts/out.jpg"],
        },
      ],
    });
    expect(messageBubbleArtifactPaths(message, ["artifacts/out.jpg"])).toEqual([
      "artifacts/out.jpg",
    ]);
  });
});
