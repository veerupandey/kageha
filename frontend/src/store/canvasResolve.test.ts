import { beforeEach, describe, expect, it } from "vitest";
import { useAppStore } from "./index";
import type { CanvasItem } from "../lib/artifactMedia";

/** Regression: clicking a bare/mis-prefixed filename in chat (e.g. `slide_01.png`,
 * which the markdown renderer maps to artifacts/slide_01.png) must open the real
 * nested artifact (artifacts/instagram-pack/carousel/slide_01.png) rather than an
 * empty lightbox. openCanvasItem resolves by basename against known canvasItems. */
describe("openCanvasItem basename resolution", () => {
  const nested = "artifacts/instagram-pack/carousel/slide_01.png";

  beforeEach(() => {
    const item: CanvasItem = {
      path: nested,
      kind: "image",
      url: `/api/sessions/s1/files/${nested}`,
      caption: "slide_01.png",
    } as CanvasItem;
    useAppStore.setState({
      sessionId: "s1",
      canvasItems: [item],
      canvasSelectedPath: null,
      canvasOpen: false,
      prefs: { ...useAppStore.getState().prefs, rightPanelCollapsed: true },
    });
  });

  it("resolves a mis-prefixed bare filename to the real nested artifact path", () => {
    useAppStore.getState().openCanvasItem("artifacts/slide_01.png", { expand: true });
    expect(useAppStore.getState().canvasSelectedPath).toBe(nested);
    expect(useAppStore.getState().canvasOpen).toBe(true);
    expect(useAppStore.getState().prefs.rightPanelCollapsed).toBe(false);
  });

  it("keeps an exact known path unchanged", () => {
    useAppStore.getState().openCanvasItem(nested, { expand: true });
    expect(useAppStore.getState().canvasSelectedPath).toBe(nested);
  });

  it("prefers an exact workspace-relative path over an unrelated basename", () => {
    useAppStore.setState({ canvasItems: [
      { path: "artifacts/profile.md", kind: "markdown", url: "/other", caption: "profile.md" },
      { path: "workspace/outputs/profile.md", kind: "markdown", url: "/profile", caption: "profile.md" },
    ] });
    useAppStore.getState().openCanvasItem("outputs/profile.md");
    expect(useAppStore.getState().canvasSelectedPath).toBe("workspace/outputs/profile.md");
  });

  it("leaves an unknown filename as-is when no basename match exists", () => {
    useAppStore.getState().openCanvasItem("artifacts/does-not-exist.png");
    expect(useAppStore.getState().canvasSelectedPath).toBe("artifacts/does-not-exist.png");
  });
});
