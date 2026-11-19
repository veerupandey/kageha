import { describe, expect, it } from "vitest";
import type { PendingApproval } from "../api/types";
import { parseAskHumanApproval, resolveAskHumanMessageId } from "./askHumanApproval";

describe("parseAskHumanApproval", () => {
  it("uses structured options from the approval payload", () => {
    const pending: PendingApproval = {
      approval_id: "question:1",
      question: "Which loss term matters most?",
      options: ["SupCon", "GenDice", "BCE"],
    };
    expect(parseAskHumanApproval(pending)).toEqual({
      question: "Which loss term matters most?",
      options: ["SupCon", "GenDice", "BCE"],
      allowOther: true,
    });
  });

  it("falls back to yes/no labels and detail parsing", () => {
    const pending: PendingApproval = {
      approval_id: "question:2",
      detail: "Use forest look?\n\n[Y] Regenerate\n[N] Keep as is",
    };
    expect(parseAskHumanApproval(pending)).toEqual({
      question: "Use forest look?",
      options: ["Regenerate", "Keep as is"],
      allowOther: true,
    });
  });

  it("parses numbered choices from detail text", () => {
    const pending: PendingApproval = {
      approval_id: "question:3",
      detail: "Pick a repo\n\n[1] kageha\n[2] loomable",
    };
    expect(parseAskHumanApproval(pending)).toEqual({
      question: "Pick a repo",
      options: ["kageha", "loomable"],
      allowOther: true,
    });
  });
});

describe("resolveAskHumanMessageId", () => {
  it("prefers the assistant message that owns the pending approval", () => {
    const pending: PendingApproval = {
      approval_id: "question:turn-1",
      sessionId: "s1",
      action: "ask_human",
    };
    const messages = [
      { id: "u1", role: "user" as const, text: "hi" },
      { id: "a1", role: "assistant" as const, text: "old", approval: pending },
      { id: "a2", role: "assistant" as const, text: "latest" },
    ];
    expect(resolveAskHumanMessageId(messages, pending, "s1")).toBe("a1");
  });

  it("falls back to the latest assistant message", () => {
    const pending: PendingApproval = {
      approval_id: "question:turn-2",
      sessionId: "s1",
      action: "ask_human",
    };
    const messages = [
      { id: "a1", role: "assistant" as const, text: "latest" },
    ];
    expect(resolveAskHumanMessageId(messages, pending, "s1")).toBe("a1");
  });
});
