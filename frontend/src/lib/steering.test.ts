import { expect, it } from "vitest";
import type { ChatMessage } from "../api/types";
import { captureSteeringBoundary, orderSteeringMessages } from "./steering";

it("keeps corrections ordered within their own turns without mutating history", () => {
  const messages: ChatMessage[] = [
    { id: "old", role: "assistant", text: "Previous", turnId: "old" },
    { id: "request", role: "user", text: "Find jobs", turnId: "current" },
    { id: "answer", role: "assistant", text: "Shortlist", turnId: "current" },
    { id: "steer1", role: "user", text: "No data engineer", steering: "applied", turnId: "current" },
    { id: "steer2", role: "user", text: "Remote", steering: "applied", turnId: "current" },
    { id: "next", role: "user", text: "Next request", turnId: "next" },
  ];
  const ordered = orderSteeringMessages(messages);
  expect(ordered.map((m) => m.id)).toEqual(["old", "request", "answer", "steer1", "steer2", "next"]);
  expect(messages[2].id).toBe("answer");
  expect(orderSteeringMessages(ordered)).toEqual(ordered);
});

it("places a steer between earlier and later tool activity and keeps actions bound to the original", () => {
  const before: ChatMessage = { id: "a", role: "assistant", text: "First findings.", turnId: "t", streaming: true,
    blocks: [{ type: "narrative", id: "n", text: "First findings." },
      { type: "tool", id: "tool1", card: { id: "one", name: "read_file", status: "ok" } }] };
  const boundary = captureSteeringBoundary(before);
  const final: ChatMessage = { ...before, text: "First findings.Corrected result.", streaming: false, statusLabel: "Error",
    failureCause: "Provider timeout", blocks: [...before.blocks!,
      { type: "tool", id: "tool2", card: { id: "two", name: "image_generate", status: "ok" } },
      { type: "narrative", id: "final", text: "Corrected result." }] };
  const rows = orderSteeringMessages([final,
    { id: "u", role: "user", text: "Use this direction", turnId: "t", steering: "applied", steeringBoundary: boundary }]);
  expect(rows.map((m) => m.id)).toEqual(["a:before:u", "u", "a"]);
  expect(rows[0].blocks?.map((b) => b.id)).toEqual(["n", "tool1"]);
  expect(rows[2].blocks?.map((b) => b.id)).toEqual(["tool2", "final"]);
  expect(rows[0].failureCause).toBeUndefined();
  expect(rows[2].failureCause).toBe("Provider timeout");
  expect(rows[0].sourceMessageId).toBe("a");
  expect(orderSteeringMessages(rows)).toEqual(rows);
});

it("splits text at multiple send positions even when later deltas extend the same block", () => {
  const message: ChatMessage = { id: "a", role: "assistant", text: "BeforeMiddleAfter", turnId: "t", streaming: true,
    blocks: [{ type: "narrative", id: "n", text: "BeforeMiddleAfter" }] };
  const rows = orderSteeringMessages([message,
    { id: "u1", role: "user", text: "one", turnId: "t", steering: "pending", steeringBoundary: { blockCount: 1, tailTextLength: 6 } },
    { id: "u2", role: "user", text: "two", turnId: "t", steering: "pending", steeringBoundary: { blockCount: 1, tailTextLength: 12 } }]);
  expect(rows.map((m) => m.text)).toEqual(["Before", "one", "Middle", "two", "After"]);
  expect(rows.filter((m) => m.streaming)).toHaveLength(1);
  expect(message.text).toBe("BeforeMiddleAfter");
});
