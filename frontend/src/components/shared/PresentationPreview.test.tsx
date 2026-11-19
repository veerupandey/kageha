import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "../../api/client";
import { useAppStore } from "../../store";
import { PresentationPreview } from "./PresentationPreview";

vi.mock("../../api/client", () => ({
  api: vi.fn(),
}));

const deck = {
  path: "artifacts/deck.pptx",
  slideCount: 2,
  slides: [
    { index: 0, title: "Hojicha A+", text: "Amazon requirements", notes: "", images: [] },
    {
      index: 1,
      title: "Product",
      text: "",
      notes: "",
      images: [{ index: 0, contentType: "image/png", url: "/api/slide.png" }],
    },
  ],
};

describe("PresentationPreview", () => {
  beforeEach(() => {
    vi.mocked(api).mockResolvedValue(deck);
    useAppStore.setState({ sessionId: "session-1" });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    useAppStore.setState({ sessionId: null });
  });

  it("pages through extracted slides", async () => {
    render(
      <PresentationPreview
        item={{
          path: "artifacts/deck.pptx",
          url: "/api/sessions/session-1/files/artifacts/deck.pptx",
          kind: "presentation",
          caption: "deck.pptx",
        }}
      />,
    );

    expect(await screen.findByText("1 / 2 · Hojicha A+")).toBeInTheDocument();
    expect(screen.getByText("Amazon requirements")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Next slide" }));
    await waitFor(() => {
      expect(screen.getByText("2 / 2 · Product")).toBeInTheDocument();
    });
    expect(screen.getByRole("img", { name: "Product" })).toHaveAttribute("src", "/api/slide.png");
  });
});
