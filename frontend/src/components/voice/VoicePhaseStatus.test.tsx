import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { VoicePhaseStatus } from "./VoicePhaseStatus";
import { phaseStatusLabel } from "./voiceCore";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voiceTypes";

/**
 * Unit tests for the presentational VoicePhaseStatus. The component renders
 * the human-readable label from `phaseStatusLabel(status, phase)` inside a
 * `role="status" aria-live="polite"` region and updates the text on each
 * status/phase transition. These tests assert the JS-visible contract: the
 * label text per state (Req 6.1-6.4), that the label updates when props change
 * (Req 6.5), and that the polite live region's text content reflects the
 * current phase/label and updates on change (Req 9.4, 9.5).
 */

/** Locate the polite live region rendered by the component. */
function liveRegion(): HTMLElement {
  // role="status" implies aria-live="polite"; the component also sets it
  // explicitly. getByRole("status") resolves the same element.
  return screen.getByRole("status");
}

describe("VoicePhaseStatus", () => {
  afterEach(() => {
    cleanup();
  });

  describe("label text per state (Req 6.1-6.4)", () => {
    it("shows 'Connecting' while connecting (Req 6.1)", () => {
      render(<VoicePhaseStatus status="connecting" phase="idle" />);
      expect(liveRegion()).toHaveTextContent("Connecting");
    });

    it("shows 'Listening' while active + listening (Req 6.2)", () => {
      render(<VoicePhaseStatus status="active" phase="listening" />);
      expect(liveRegion()).toHaveTextContent("Listening");
    });

    it("shows 'Thinking' while active + thinking (Req 6.3)", () => {
      render(<VoicePhaseStatus status="active" phase="thinking" />);
      expect(liveRegion()).toHaveTextContent("Thinking");
    });

    it("shows 'Speaking' while active + speaking (Req 6.4)", () => {
      render(<VoicePhaseStatus status="active" phase="speaking" />);
      expect(liveRegion()).toHaveTextContent("Speaking");
    });

    // Exhaustive agreement with the pure helper over the full grid — proves the
    // rendered text is exactly phaseStatusLabel(status, phase) for every pair.
    it("matches phaseStatusLabel for every status x phase pair", () => {
      const statuses: GeminiLiveStatus[] = [
        "idle",
        "connecting",
        "active",
        "error",
      ];
      const phases: GeminiLivePhase[] = [
        "idle",
        "listening",
        "thinking",
        "speaking",
      ];
      for (const status of statuses) {
        for (const phase of phases) {
          const { unmount } = render(
            <VoicePhaseStatus status={status} phase={phase} />,
          );
          expect(liveRegion()).toHaveTextContent(
            phaseStatusLabel(status, phase),
          );
          unmount();
        }
      }
    });
  });

  describe("label updates on state change (Req 6.5)", () => {
    it("updates the label when the phase prop changes", () => {
      const { rerender } = render(
        <VoicePhaseStatus status="active" phase="listening" />,
      );
      expect(liveRegion()).toHaveTextContent("Listening");

      rerender(<VoicePhaseStatus status="active" phase="thinking" />);
      expect(liveRegion()).toHaveTextContent("Thinking");

      rerender(<VoicePhaseStatus status="active" phase="speaking" />);
      expect(liveRegion()).toHaveTextContent("Speaking");
    });

    it("updates the label when the status prop changes", () => {
      const { rerender } = render(
        <VoicePhaseStatus status="connecting" phase="idle" />,
      );
      expect(liveRegion()).toHaveTextContent("Connecting");

      rerender(<VoicePhaseStatus status="active" phase="listening" />);
      expect(liveRegion()).toHaveTextContent("Listening");
    });
  });

  describe("polite ARIA live region reflects current phase (Req 9.4, 9.5)", () => {
    it("exposes a polite live region whose text is the current label", () => {
      render(<VoicePhaseStatus status="active" phase="listening" />);
      const region = liveRegion();
      expect(region).toHaveAttribute("aria-live", "polite");
      // The live region text content equals the current label (Req 9.4).
      expect(region).toHaveTextContent("Listening");
      // The region tracks the current phase via data-phase (Req 9.4).
      expect(region).toHaveAttribute("data-phase", "listening");
    });

    it("replaces the live region text content when the phase changes (Req 9.5)", () => {
      const { rerender } = render(
        <VoicePhaseStatus status="active" phase="thinking" />,
      );
      const before = liveRegion();
      expect(before).toHaveAttribute("aria-live", "polite");
      expect(before).toHaveTextContent("Thinking");
      expect(before).toHaveAttribute("data-phase", "thinking");

      rerender(<VoicePhaseStatus status="active" phase="speaking" />);
      const after = liveRegion();
      // Still a polite region, but its content now states the new phase.
      expect(after).toHaveAttribute("aria-live", "polite");
      expect(after).toHaveTextContent("Speaking");
      expect(after).not.toHaveTextContent("Thinking");
      expect(after).toHaveAttribute("data-phase", "speaking");
    });
  });
});
