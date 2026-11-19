import assert from "node:assert/strict";
import { experienceTimings } from "./agent_speed_benchmark_lib.mjs";

// Subscribe before submitting, so event observations cannot be delayed by the
// acceptance response. The headless runner cannot measure browser paint time.
export async function observedBenchmarkTurn(body, {base, session, token, api, timeoutMs = 600000}) {
  const abort = new AbortController();
  const response = await fetch(base + `/api/sessions/${encodeURIComponent(session)}/events/stream`, {
    headers: {Authorization: "Bearer " + token}, signal: abort.signal,
  });
  if (!response.ok || !response.body) throw Error(`Event stream HTTP ${response.status}`);
  const events = [], doneByTurn = new Map();
  let target, started, resolveDone, rejectDone;
  const done = new Promise((resolve, reject) => { resolveDone = resolve; rejectDone = reject; });
  // Attach immediately: the feed may fail before the admission request returns.
  void done.catch(() => {});
  const pump = (async () => {
    const decoder = new TextDecoder();
    let pending = "";
    for await (const chunk of response.body) {
      pending += decoder.decode(chunk, {stream: true});
      let end;
      while ((end = pending.indexOf("\n\n")) >= 0) {
        const lines = pending.slice(0, end).split("\n");
        pending = pending.slice(end + 2);
        if (!lines.includes("event: update")) continue;
        const packet = JSON.parse(lines.filter(line => line.startsWith("data: ")).map(line => line.slice(6)).join("\n"));
        for (const frame of packet.events || []) {
          if (started === undefined) continue;
          const at = performance.now() - started;
          // Retain only timing metadata, not model commentary or tool contents.
          if (frame.kind === "assistant_progress") events.push({elapsed_ms: at, frame: {
            kind: frame.kind, turn_id: frame.turn_id,
            payload: {origin: frame.payload?.origin, text: typeof frame.payload?.text === "string" && frame.payload.text.trim() ? "present" : ""},
          }});
          if (frame.kind === "tool_card") events.push({elapsed_ms: at, frame: {
            kind: frame.kind, turn_id: frame.turn_id,
            tool_card: {status: frame.tool_card?.status, cached: frame.tool_card?.cached},
          }});
          if (frame.kind === "done") {
            const result = {payload: frame.payload, elapsed_ms: at};
            doneByTurn.set(frame.turn_id, result);
            if (frame.turn_id === target) resolveDone(result);
          }
        }
      }
    }
    if (!abort.signal.aborted) rejectDone(Error("Event stream ended before completion"));
  })().catch(error => { if (!abort.signal.aborted) rejectDone(error); });
  const timeout = setTimeout(() => rejectDone(Error("Timed out waiting for benchmark completion")), timeoutMs);
  try {
    started = performance.now();
    const accepted = await api("/api/chat/stream", {...body, event_stream: "session"});
    const acceptance = performance.now() - started;
    assert.equal(accepted.status, "accepted", "Benchmark turn must be accepted");
    assert(typeof accepted.turn_id === "string" && accepted.turn_id.length > 0, "Accepted turn identity is required");
    target = accepted.turn_id;
    if (doneByTurn.has(target)) resolveDone(doneByTurn.get(target));
    const completed = await done;
    return {answer: completed.payload, elapsed_ms: Math.round(completed.elapsed_ms),
      experience: experienceTimings(events, target, acceptance, completed.elapsed_ms)};
  } finally {
    clearTimeout(timeout);
    abort.abort();
    await pump;
  }
}
