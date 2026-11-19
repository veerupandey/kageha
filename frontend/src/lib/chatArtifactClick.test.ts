import { describe, expect, it } from "vitest";
import {
  artifactPathFromSessionFileUrl,
  chatArtifactFromClick,
  shouldLightboxArtifact,
} from "./chatArtifactClick";

describe("chatArtifactClick", () => {
  it("parses a session files URL", () => {
    expect(
      artifactPathFromSessionFileUrl(
        "/api/sessions/abc/files/artifacts/batch-book-front.jpg?v=3",
      ),
    ).toBe("artifacts/batch-book-front.jpg");
  });

  it("lightboxes images and videos, not markdown", () => {
    expect(shouldLightboxArtifact("artifacts/hero.jpg")).toBe(true);
    expect(shouldLightboxArtifact("artifacts/clip.mp4")).toBe(true);
    expect(shouldLightboxArtifact("artifacts/notes.md")).toBe(false);
  });

  it("opens an inline chat image in the lightbox", () => {
    const img = document.createElement("img");
    img.src =
      "http://localhost/api/sessions/s1/files/artifacts/batch-book-front.jpg";
    expect(chatArtifactFromClick(img)).toEqual({
      path: "artifacts/batch-book-front.jpg",
      expand: true,
    });
  });

  it("opens an image path chip in the lightbox", () => {
    const link = document.createElement("a");
    link.className = "artifact-path";
    link.setAttribute("data-artifact", "artifacts/batch-book-front.jpg");
    const code = document.createElement("code");
    code.textContent = "artifacts/batch-book-front.jpg";
    link.appendChild(code);
    expect(chatArtifactFromClick(code)).toEqual({
      path: "artifacts/batch-book-front.jpg",
      expand: true,
    });
  });

  it("keeps non-image path chips in the files panel", () => {
    const link = document.createElement("a");
    link.className = "artifact-path";
    link.setAttribute("data-artifact", "artifacts/notes.md");
    expect(chatArtifactFromClick(link)).toEqual({
      path: "artifacts/notes.md",
      expand: false,
    });
  });
});
