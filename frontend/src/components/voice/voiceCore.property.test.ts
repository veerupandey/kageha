import { describe, expect, it } from "vitest";
import fc from "fast-check";

import {
  appendCaption,
  clamp01,
  orbStateFor,
  phaseStatusLabel,
  reduceDelegation,
} from "./voiceCore";
import {
  CAPTION_RETENTION_LIMIT,
  type CaptionEntry,
  type GeminiLivePhase,
  type GeminiLiveStatus,
  type OrbVisualState,
  type VoiceSseEvent,
} from "./voiceTypes";

const STATUSES: GeminiLiveStatus[] = ["idle", "connecting", "active", "error"];
const PHASES: GeminiLivePhase[] = ["idle", "listening", "thinking", "speaking"];

/**
 * Local mirror of the orb amplitude mapping defined in voiceOrb.css:
 *   transform: scale(calc(1 + 0.35 * var(--lvl)))
 * Used only to check the monotonicity/endpoint guarantees of Req 4.1.
 */
function scale(level: number): number {
  return 1 + 0.35 * level;
}

const NUM_RUNS = 100;

describe("voiceCore property tests", () => {
  // Feature: live-voice-immersive-ui, Property 1: Mic-level clamp and monotonic amplitude mapping
  describe("Property 1: Mic-level clamp and monotonic amplitude mapping (Req 4.1, 4.2)", () => {
    it("clamp01 returns a value in [0,1] for arbitrary inputs", () => {
      const arbitraryInput = fc.oneof(
        fc.double(), // includes out-of-range, NaN, +/-Infinity
        fc.integer(),
        fc.constant(Number.NaN),
        fc.constant(Number.POSITIVE_INFINITY),
        fc.constant(Number.NEGATIVE_INFINITY),
        fc.constant(undefined),
        fc.constant(null),
        fc.string(),
        fc.boolean(),
        fc.object(),
      );
      fc.assert(
        fc.property(arbitraryInput, (value) => {
          const result = clamp01(value);
          expect(typeof result).toBe("number");
          expect(Number.isFinite(result)).toBe(true);
          expect(result).toBeGreaterThanOrEqual(0);
          expect(result).toBeLessThanOrEqual(1);
        }),
        { numRuns: NUM_RUNS },
      );
    });

    it("clamp01 maps missing / non-numeric / NaN / non-finite to 0", () => {
      const nonNumericOrNonFinite = fc.oneof(
        fc.constant(undefined),
        fc.constant(null),
        fc.string(),
        fc.boolean(),
        fc.object(),
        fc.constant(Number.NaN),
        fc.constant(Number.POSITIVE_INFINITY),
        fc.constant(Number.NEGATIVE_INFINITY),
      );
      fc.assert(
        fc.property(nonNumericOrNonFinite, (value) => {
          expect(clamp01(value)).toBe(0);
        }),
        { numRuns: NUM_RUNS },
      );
    });

    it("clamp01 preserves in-range values", () => {
      fc.assert(
        fc.property(
          fc.double({ min: 0, max: 1, noNaN: true }),
          (value) => {
            expect(clamp01(value)).toBe(value);
          },
        ),
        { numRuns: NUM_RUNS },
      );
    });

    it("amplitude mapping is monotonic non-decreasing with min at 0 and max at 1", () => {
      fc.assert(
        fc.property(
          fc.double({ min: 0, max: 1, noNaN: true }),
          fc.double({ min: 0, max: 1, noNaN: true }),
          (x, y) => {
            const a = Math.min(x, y);
            const b = Math.max(x, y);
            // a <= b => scale(a) <= scale(b)
            expect(scale(a)).toBeLessThanOrEqual(scale(b));
            // scale(0) is the minimum and scale(1) is the maximum over [0,1].
            expect(scale(a)).toBeGreaterThanOrEqual(scale(0));
            expect(scale(b)).toBeLessThanOrEqual(scale(1));
          },
        ),
        { numRuns: NUM_RUNS },
      );
    });
  });

  // Feature: live-voice-immersive-ui, Property 2: Orb state is a total function of (status, phase) with precedence error > connecting > phase
  describe("Property 2: Orb state totality + precedence error > connecting > phase (Req 4.3, 4.4, 4.5, 4.7, 4.8)", () => {
    const validOrbStates: OrbVisualState[] = [
      "error",
      "connecting",
      "idle",
      "listening",
      "thinking",
      "speaking",
    ];

    it("returns exactly one defined state for every status x phase pair, honoring precedence", () => {
      fc.assert(
        fc.property(
          fc.constantFrom(...STATUSES),
          fc.constantFrom(...PHASES),
          (status, phase) => {
            const state = orbStateFor(status, phase);
            // Total: always a defined, valid state.
            expect(validOrbStates).toContain(state);

            // Precedence: error dominates.
            if (status === "error") {
              expect(state).toBe("error");
            } else if (status === "connecting") {
              // then connecting.
              expect(state).toBe("connecting");
            } else {
              // otherwise the phase-derived state.
              expect(state).toBe(phase);
            }
          },
        ),
        { numRuns: NUM_RUNS },
      );
    });

    it("the six orb states are distinct across the input space", () => {
      const observed = new Set<OrbVisualState>();
      for (const status of STATUSES) {
        for (const phase of PHASES) {
          observed.add(orbStateFor(status, phase));
        }
      }
      // All six distinct states are reachable and none collide.
      expect(observed).toEqual(new Set(validOrbStates));
      expect(observed.size).toBe(6);
    });
  });

  // Feature: live-voice-immersive-ui, Property 3: Caption append preserves chronological order, drops blanks, and bounds retention
  describe("Property 3: Caption append ordering / blank-drop / bound (Req 5.1, 5.2, 5.3, 5.5, 5.7)", () => {
    // Generator for Unicode-whitespace-only strings (blank events).
    const whitespaceChar = fc.constantFrom(
      " ",
      "\t",
      "\n",
      "\r",
      "\f",
      "\v",
      "\u00a0", // no-break space
      "\u2028", // line separator
      "\u2029", // paragraph separator
      "\u3000", // ideographic space
      "\u200a", // hair space
      "",
    );
    const blankText = fc.array(whitespaceChar, { maxLength: 6 }).map((cs) => cs.join(""));

    // Generator for text that is guaranteed non-empty after trimming.
    const nonEmptyText = fc
      .string({ minLength: 1 })
      .map((s) => `x${s}y`) // ensure non-whitespace content survives trimming
      .filter((s) => s.trim().length > 0);

    const role = fc.constantFrom<"user" | "assistant">("user", "assistant");

    // An event is either blank (should be dropped) or non-empty (should append).
    const eventArb = fc.oneof(
      fc.record({ role, text: blankText, blank: fc.constant(true) }),
      fc.record({ role, text: nonEmptyText, blank: fc.constant(false) }),
    );

    it("folding appendCaption yields correctly filtered, ordered, trimmed, bounded captions", () => {
      fc.assert(
        fc.property(
          fc.array(eventArb, { maxLength: 120 }),
          fc.integer({ min: 1, max: 60 }),
          (events, limit) => {
            let list: CaptionEntry[] = [];
            let seq = 0;
            // Track the "expected" qualifying entries independently.
            const expectedAll: CaptionEntry[] = [];

            for (const ev of events) {
              const before = list.length;
              list = appendCaption(list, { role: ev.role, text: ev.text }, seq, limit);
              const isBlank = ev.text.trim().length === 0;

              if (isBlank) {
                // Blank / whitespace-only events contribute nothing.
                expect(list.length).toBe(Math.min(before, limit));
              } else {
                expectedAll.push({
                  id: String(seq),
                  role: ev.role,
                  text: ev.text.trim(),
                  seq,
                });
              }
              seq += 1;
            }

            // Length never exceeds the limit.
            expect(list.length).toBeLessThanOrEqual(limit);

            // Retains the most-recent qualifying entries.
            const expectedTail = expectedAll.slice(
              Math.max(0, expectedAll.length - limit),
            );
            expect(list).toEqual(expectedTail);

            // Every retained entry has non-empty trimmed text and correct role,
            // and seq is strictly increasing (arrival order preserved).
            for (let i = 0; i < list.length; i += 1) {
              expect(list[i].text.length).toBeGreaterThan(0);
              expect(list[i].text).toBe(list[i].text.trim());
              expect(["user", "assistant"]).toContain(list[i].role);
              if (i > 0) {
                expect(list[i].seq).toBeGreaterThan(list[i - 1].seq);
              }
            }
          },
        ),
        { numRuns: NUM_RUNS },
      );
    });

    it("defaults retention to CAPTION_RETENTION_LIMIT", () => {
      fc.assert(
        fc.property(
          fc.array(nonEmptyText, { minLength: 0, maxLength: CAPTION_RETENTION_LIMIT + 40 }),
          (texts) => {
            let list: CaptionEntry[] = [];
            texts.forEach((text, seq) => {
              list = appendCaption(list, { role: "user", text }, seq);
            });
            expect(list.length).toBeLessThanOrEqual(CAPTION_RETENTION_LIMIT);
          },
        ),
        { numRuns: NUM_RUNS },
      );
    });
  });

  // Feature: live-voice-immersive-ui, Property 4: Phase status label is a total function matching the documented table
  describe("Property 4: Phase status label totality + table match (Req 6.1, 6.2, 6.3, 6.4)", () => {
    it("returns a non-empty string for every status/phase and matches the documented table", () => {
      fc.assert(
        fc.property(
          fc.constantFrom(...STATUSES),
          fc.constantFrom(...PHASES),
          (status, phase) => {
            const label = phaseStatusLabel(status, phase);
            // Total: always a non-empty string.
            expect(typeof label).toBe("string");
            expect(label.length).toBeGreaterThan(0);

            if (status === "connecting") {
              expect(label).toBe("Connecting");
            } else if (status === "active") {
              if (phase === "listening") expect(label).toBe("Listening");
              else if (phase === "thinking") expect(label).toBe("Thinking");
              else if (phase === "speaking") expect(label).toBe("Speaking");
            }
          },
        ),
        { numRuns: NUM_RUNS },
      );
    });
  });

  // Feature: live-voice-immersive-ui, Property 5: Delegation set reflects exactly started-but-not-terminated delegations
  describe("Property 5: Delegation set reflects started-but-not-terminated delegations (Req 12.1, 12.2, 12.3)", () => {
    const terminalKinds = [
      "voice_delegation_completed",
      "voice_delegation_failed",
      "voice_delegation_cancelled",
    ] as const;

    function startedEvent(callId: string): VoiceSseEvent {
      return {
        kind: "voice_delegation_started",
        data: { call_id: callId, task: "t", session_id: "s" },
      };
    }

    function terminalEvent(
      callId: string,
      kind: (typeof terminalKinds)[number],
    ): VoiceSseEvent {
      switch (kind) {
        case "voice_delegation_completed":
          return {
            kind,
            data: { call_id: callId, task: "t", result: "r", session_id: "s" },
          };
        case "voice_delegation_failed":
          return {
            kind,
            data: { call_id: callId, task: "t", error: "e", session_id: "s" },
          };
        case "voice_delegation_cancelled":
          return { kind, data: { call_id: callId, task: "t", session_id: "s" } };
      }
    }

    const callId = fc.constantFrom("a", "b", "c", "d", "e");
    const eventArb = fc.oneof(
      callId.map((id) => ({ type: "start" as const, id })),
      fc
        .tuple(callId, fc.constantFrom(...terminalKinds))
        .map(([id, kind]) => ({ type: "terminal" as const, id, kind })),
    );

    it("fold yields exactly ids started without a subsequent terminal event", () => {
      fc.assert(
        fc.property(fc.array(eventArb, { maxLength: 120 }), (events) => {
          let active = new Set<string>();
          // Reference model: last event per id determines membership.
          const model = new Set<string>();

          for (const ev of events) {
            if (ev.type === "start") {
              active = reduceDelegation(active, startedEvent(ev.id));
              model.add(ev.id);
            } else {
              const hadIt = active.has(ev.id);
              const next = reduceDelegation(active, terminalEvent(ev.id, ev.kind));
              // Terminal event for an absent id is a no-op (same reference).
              if (!hadIt) {
                expect(next).toBe(active);
              }
              active = next;
              model.delete(ev.id);
            }
          }

          expect(active).toEqual(model);
        }),
        { numRuns: NUM_RUNS },
      );
    });

    it("terminal event for an absent id leaves the set unchanged", () => {
      fc.assert(
        fc.property(
          fc.uniqueArray(fc.constantFrom("a", "b", "c", "d", "e")),
          callId,
          fc.constantFrom(...terminalKinds),
          (present, target, kind) => {
            fc.pre(!present.includes(target));
            const active = new Set(present);
            const next = reduceDelegation(active, terminalEvent(target, kind));
            expect(next).toBe(active);
            expect(next).toEqual(new Set(present));
          },
        ),
        { numRuns: NUM_RUNS },
      );
    });
  });
});
