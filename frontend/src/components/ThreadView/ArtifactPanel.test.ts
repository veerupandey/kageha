import { describe, expect, it } from "vitest";
import type { CanvasItem } from "../../lib/artifactMedia";
import { matchesFilter, resolveArtifactSelection } from "./artifactFilter";

function item(path: string, kind: CanvasItem["kind"]): CanvasItem {
  return { path, url: `/${path}`, kind, caption: path.split("/").pop() || path };
}

const image = item("artifacts/hero.png", "image");
const deck = item("artifacts/kageha_hojicha_aplus_6slides.pptx", "presentation");
const script = item("artifacts/build_pptx.py", "code");

describe("artifact filter selection", () => {
  it("treats presentations as documents", () => {
    expect(matchesFilter(deck, "documents")).toBe(true);
    expect(matchesFilter(image, "documents")).toBe(false);
    expect(matchesFilter(script, "code")).toBe(true);
  });

  it("does not bounce back to a store selection hidden by the filter", () => {
    const first = resolveArtifactSelection([deck], image.path, image.path, new Set());
    expect(first).toEqual({ path: deck.path, syncStore: true });

    const next = resolveArtifactSelection([deck], first.path, first.path, new Set());
    expect(next).toEqual({ path: deck.path, syncStore: false });
  });

  it("keeps a store selection that is still visible", () => {
    expect(resolveArtifactSelection([image, deck], deck.path, deck.path, new Set())).toEqual({
      path: deck.path,
      syncStore: false,
    });
  });

  it("does not clear the store when the filter is empty", () => {
    expect(resolveArtifactSelection([], image.path, image.path, new Set())).toEqual({
      path: null,
      syncStore: false,
    });
  });
});
