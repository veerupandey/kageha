import { expect, it, vi } from "vitest";
import { sessionTurnResponse } from "./sessionTransport";

vi.mock("../lib/sessionEvents", () => ({ subscribeSessionEvents: vi.fn(() => () => {}) }));

it("reports server acceptance immediately, before the first journal event", async () => {
  const controller = new AbortController();
  const response = sessionTurnResponse({ session_id: "s", turn_id: "t", thread_id: "th", request_id: "r", after_sequence: 0 }, {}, controller.signal);
  const reader = response.body!.getReader();
  const { value } = await reader.read();
  const frame = new TextDecoder().decode(value);
  expect(frame).toContain("event: status");
  expect(frame).toContain("Request received · preparing…");
  expect(frame).toContain('"turn_id":"t"');
  controller.abort();
  await reader.cancel();
});
