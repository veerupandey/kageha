import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { VoiceCaptions } from "./VoiceCaptions";
import type { CaptionEntry } from "./voiceTypes";

/**
 * Unit tests for the presentational VoiceCaptions list.
 *
 * The component renders a placeholder while empty (Req 5.8), removes it once
 * the first entry lands (Req 5.9), renders entries in `seq`/chronological
 * order attributed by role (Req 5.5), scrolls the newest entry into view on
 * append (Req 5.6), and defensively slices display to the most-recent `limit`
 * entries (Req 5.7).
 *
 * jsdom does not implement Element.prototype.scrollIntoView, and the component
 * calls it from an effect, so we stub it with a vi.fn() spy for the whole
 * suite and inspect the calls for the Req 5.6 assertion.
 */

const scrollIntoViewSpy = vi.fn();

beforeAll(() => {
  Element.prototype.scrollIntoView = scrollIntoViewSpy;
});

afterEach(() => {
  cleanup();
  scrollIntoViewSpy.mockClear();
});

function entry(
  seq: number,
  role: CaptionEntry["role"],
  text: string,
): CaptionEntry {
  return { id: `c${seq}`, role, text, seq };
}

const PLACEHOLDER = /the conversation transcript will appear here/i;

describe("VoiceCaptions", () => {
  describe("placeholder lifecycle (Req 5.8, 5.9)", () => {
    it("shows the placeholder when captions is empty (Req 5.8)", () => {
      render(<VoiceCaptions captions={[]} />);
      expect(screen.getByText(PLACEHOLDER)).toBeInTheDocument();
    });

    it("removes the placeholder once the first entry is present (Req 5.9)", () => {
      const { rerender } = render(<VoiceCaptions captions={[]} />);
      expect(screen.getByText(PLACEHOLDER)).toBeInTheDocument();

      rerender(<VoiceCaptions captions={[entry(0, "user", "hello")]} />);
      expect(screen.queryByText(PLACEHOLDER)).not.toBeInTheDocument();
      expect(screen.getByText("hello")).toBeInTheDocument();
    });
  });

  describe("chronological order and role attribution (Req 5.5)", () => {
    it("renders entries in seq order with the newest last", () => {
      const captions = [
        entry(0, "user", "first"),
        entry(1, "assistant", "second"),
        entry(2, "user", "third"),
      ];
      const { container } = render(<VoiceCaptions captions={captions} />);

      const texts = Array.from(
        container.querySelectorAll(".ka-voice-caption"),
      ).map((el) => el.textContent);

      // Each caption cell contains the role label plus the text; assert the
      // spoken text appears in received (seq) order.
      expect(texts[0]).toContain("first");
      expect(texts[1]).toContain("second");
      expect(texts[2]).toContain("third");
    });

    it("attributes each entry by role", () => {
      const captions = [
        entry(0, "user", "from user"),
        entry(1, "assistant", "from assistant"),
      ];
      const { container } = render(<VoiceCaptions captions={captions} />);

      const cells = Array.from(
        container.querySelectorAll<HTMLElement>(".ka-voice-caption"),
      );
      expect(cells[0]).toHaveAttribute("data-role", "user");
      expect(cells[0]).toHaveTextContent("You");
      expect(cells[1]).toHaveAttribute("data-role", "assistant");
      expect(cells[1]).toHaveTextContent("Assistant");
    });
  });

  describe("newest entry scrolled into view (Req 5.6)", () => {
    it("scrolls into view when a new caption is appended", () => {
      const initial = [entry(0, "user", "hello")];
      const { rerender } = render(<VoiceCaptions captions={initial} />);

      // The initial mount already scrolls the first entry into view.
      scrollIntoViewSpy.mockClear();

      rerender(
        <VoiceCaptions
          captions={[...initial, entry(1, "assistant", "reply")]}
        />,
      );

      expect(scrollIntoViewSpy).toHaveBeenCalled();
    });
  });

  describe("retention bound (Req 5.7)", () => {
    it("renders only the most recent `limit` entries", () => {
      const captions = [
        entry(0, "user", "oldest"),
        entry(1, "assistant", "middle"),
        entry(2, "user", "newest"),
      ];
      const { container } = render(
        <VoiceCaptions captions={captions} limit={2} />,
      );

      const cells = container.querySelectorAll(".ka-voice-caption");
      expect(cells).toHaveLength(2);
      // The oldest entry is dropped; the most-recent two remain in order.
      expect(screen.queryByText("oldest")).not.toBeInTheDocument();
      expect(screen.getByText("middle")).toBeInTheDocument();
      expect(screen.getByText("newest")).toBeInTheDocument();
    });

    it("renders all entries when count is within `limit`", () => {
      const captions = [
        entry(0, "user", "a"),
        entry(1, "assistant", "b"),
      ];
      const { container } = render(
        <VoiceCaptions captions={captions} limit={5} />,
      );
      expect(container.querySelectorAll(".ka-voice-caption")).toHaveLength(2);
    });
  });
});
