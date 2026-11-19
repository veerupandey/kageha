import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { VoiceOrb } from "./VoiceOrb";
import { orbStateFor } from "./voiceCore";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voiceTypes";

/**
 * Unit tests for the presentational VoiceOrb. The orb derives a single
 * discrete `data-orb-state` from `orbStateFor(status, phase)` and reads the
 * mic level ONLY via CSS (never in JS). jsdom does not compute CSS `clamp()`
 * or resolve theme tokens, so these tests assert the JS-visible contract:
 * the resolved orb state, the reduced-motion branch, the class/structure that
 * the accent-token CSS targets, and that a garbage `--ka-live-mic-level` does
 * not affect rendering (the component never reads it in JS).
 */

function orbStage(container: HTMLElement): HTMLElement {
  const stage = container.querySelector<HTMLElement>(".ka-voice-orb-stage");
  if (!stage) throw new Error("orb stage not found");
  return stage;
}

describe("VoiceOrb", () => {
  afterEach(() => {
    cleanup();
    // Reset any garbage we may have written to :root during a test.
    document.documentElement.style.removeProperty("--ka-live-mic-level");
  });

  describe("data-orb-state reflects orbStateFor", () => {
    // error / connecting take precedence over any phase (Req 4.7, 4.8).
    it("gives error precedence over phase (status=error, phase=speaking)", () => {
      const { container } = render(<VoiceOrb status="error" phase="speaking" />);
      expect(orbStage(container)).toHaveAttribute("data-orb-state", "error");
    });

    it("gives connecting precedence over phase (status=connecting, phase=listening)", () => {
      const { container } = render(
        <VoiceOrb status="connecting" phase="listening" />,
      );
      expect(orbStage(container)).toHaveAttribute("data-orb-state", "connecting");
    });

    // Distinct treatment per phase while active (Req 4.3, 4.4, 4.5).
    it("reflects thinking phase while active (status=active, phase=thinking)", () => {
      const { container } = render(<VoiceOrb status="active" phase="thinking" />);
      expect(orbStage(container)).toHaveAttribute("data-orb-state", "thinking");
    });

    it("reflects listening phase while active (Req 4.1/4.3 distinct state)", () => {
      const { container } = render(<VoiceOrb status="active" phase="listening" />);
      expect(orbStage(container)).toHaveAttribute("data-orb-state", "listening");
    });

    it("reflects speaking phase while active (Req 4.3)", () => {
      const { container } = render(<VoiceOrb status="active" phase="speaking" />);
      expect(orbStage(container)).toHaveAttribute("data-orb-state", "speaking");
    });

    it("reflects idle phase while active (Req 4.5)", () => {
      const { container } = render(<VoiceOrb status="active" phase="idle" />);
      expect(orbStage(container)).toHaveAttribute("data-orb-state", "idle");
    });

    // Exhaustive agreement with the pure helper over the full grid — proves the
    // rendered attribute is exactly orbStateFor(status, phase), so the six
    // states (error/connecting/idle/listening/thinking/speaking) are distinct
    // wherever the helper distinguishes them (Req 4.3-4.5, 4.7, 4.8).
    it("matches orbStateFor for every status x phase pair", () => {
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
          const { container, unmount } = render(
            <VoiceOrb status={status} phase={phase} />,
          );
          expect(orbStage(container)).toHaveAttribute(
            "data-orb-state",
            orbStateFor(status, phase),
          );
          unmount();
        }
      }
    });
  });

  describe("mic level is read only via CSS, never in JS (Req 4.2)", () => {
    it("renders regardless of a garbage --ka-live-mic-level on :root", () => {
      // Stale / garbage value the CSS clamp() would neutralise. The component
      // must not read this in JS, so rendering is unaffected by it.
      document.documentElement.style.setProperty(
        "--ka-live-mic-level",
        "not-a-number",
      );
      const { container } = render(<VoiceOrb status="active" phase="listening" />);
      const stage = orbStage(container);
      // The orb element itself renders (the CSS var fallback is safe).
      expect(stage.querySelector(".ka-voice-orb")).toBeInTheDocument();
      // State is still driven purely by props, not the garbage var.
      expect(stage).toHaveAttribute("data-orb-state", "listening");
    });

    it("renders identically for a huge out-of-range mic level", () => {
      document.documentElement.style.setProperty(
        "--ka-live-mic-level",
        "9999",
      );
      const { container } = render(<VoiceOrb status="active" phase="listening" />);
      const stage = orbStage(container);
      expect(stage.querySelector(".ka-voice-orb")).toBeInTheDocument();
      expect(stage).toHaveAttribute("data-orb-state", "listening");
    });
  });

  describe("reduced motion (Req 8.1, 8.2)", () => {
    it("sets data-reduced-motion=true when reducedMotion prop is true", () => {
      const { container } = render(
        <VoiceOrb status="active" phase="listening" reducedMotion={true} />,
      );
      expect(orbStage(container)).toHaveAttribute("data-reduced-motion", "true");
    });

    it("sets data-reduced-motion=false when reducedMotion prop is false", () => {
      const { container } = render(
        <VoiceOrb status="active" phase="listening" reducedMotion={false} />,
      );
      expect(orbStage(container)).toHaveAttribute("data-reduced-motion", "false");
    });
  });

  describe("accent-token styling hooks (Req 10.1)", () => {
    it("renders the ka-voice-orb element the accent-token CSS targets", () => {
      // jsdom won't resolve --color-accent, so assert the class/structure the
      // stylesheet uses to apply the accent tokens.
      const { container } = render(<VoiceOrb status="active" phase="listening" />);
      const stage = orbStage(container);
      expect(stage.querySelector(".ka-voice-orb")).toBeInTheDocument();
      expect(stage.querySelector(".ka-voice-orb-glow")).toBeInTheDocument();
    });

    it("hides the decorative orb from assistive tech via aria-hidden", () => {
      const { container } = render(<VoiceOrb status="active" phase="listening" />);
      expect(orbStage(container)).toHaveAttribute("aria-hidden", "true");
    });
  });
});
