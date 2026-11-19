import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { VoiceControls } from "./VoiceControls";

/**
 * Unit tests for the presentational VoiceControls. The component renders an
 * exit-immersive control (always) and an end-conversation control (only when
 * `showEndControl` is true). Each control must expose a non-empty accessible
 * name (Req 9.3) and invoke the corresponding callback when clicked
 * (Req 2.1, 3.1).
 */

describe("VoiceControls", () => {
  afterEach(() => {
    cleanup();
  });

  describe("accessible names (Req 9.3)", () => {
    it("exposes a non-empty accessible name for the exit control", () => {
      render(
        <VoiceControls onExit={() => {}} onEnd={() => {}} showEndControl />,
      );
      const exit = screen.getByRole("button", { name: "Exit immersive view" });
      expect(exit).toBeInTheDocument();
      expect(exit).toHaveAccessibleName(/\S/);
    });

    it("exposes a non-empty accessible name for the end control", () => {
      render(
        <VoiceControls onExit={() => {}} onEnd={() => {}} showEndControl />,
      );
      const end = screen.getByRole("button", { name: "End conversation" });
      expect(end).toBeInTheDocument();
      expect(end).toHaveAccessibleName(/\S/);
    });
  });

  describe("callbacks (Req 2.1, 3.1)", () => {
    it("invokes onExit when the exit control is clicked", () => {
      const onExit = vi.fn();
      const onEnd = vi.fn();
      render(<VoiceControls onExit={onExit} onEnd={onEnd} showEndControl />);

      fireEvent.click(
        screen.getByRole("button", { name: "Exit immersive view" }),
      );

      expect(onExit).toHaveBeenCalledTimes(1);
      expect(onEnd).not.toHaveBeenCalled();
    });

    it("invokes onEnd when the end control is clicked", () => {
      const onExit = vi.fn();
      const onEnd = vi.fn();
      render(<VoiceControls onExit={onExit} onEnd={onEnd} showEndControl />);

      fireEvent.click(
        screen.getByRole("button", { name: "End conversation" }),
      );

      expect(onEnd).toHaveBeenCalledTimes(1);
      expect(onExit).not.toHaveBeenCalled();
    });
  });

  describe("end control visibility", () => {
    it("hides the end control when showEndControl is false", () => {
      render(
        <VoiceControls
          onExit={() => {}}
          onEnd={() => {}}
          showEndControl={false}
        />,
      );
      expect(
        screen.queryByRole("button", { name: "End conversation" }),
      ).toBeNull();
      // The exit control remains available regardless.
      expect(
        screen.getByRole("button", { name: "Exit immersive view" }),
      ).toBeInTheDocument();
    });

    it("shows the end control when showEndControl is true", () => {
      render(
        <VoiceControls onExit={() => {}} onEnd={() => {}} showEndControl />,
      );
      expect(
        screen.getByRole("button", { name: "End conversation" }),
      ).toBeInTheDocument();
    });
  });
});
