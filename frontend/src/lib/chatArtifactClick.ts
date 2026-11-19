/** Resolve a chat click onto a session artifact, and whether to open the lightbox. */

import { canvasKindForPath } from "./artifactMedia";

export function artifactPathFromSessionFileUrl(src: string): string | null {
  const raw = String(src || "");
  const marker = "/files/";
  const idx = raw.indexOf(marker);
  if (idx < 0) return null;
  try {
    const path = decodeURIComponent(
      raw.slice(idx + marker.length).split("?")[0] || "",
    );
    return path || null;
  } catch {
    return null;
  }
}

export function shouldLightboxArtifact(path: string): boolean {
  const kind = canvasKindForPath(path);
  return kind === "image" || kind === "video";
}

export function chatArtifactFromClick(
  target: EventTarget | null,
): { path: string; expand: boolean } | null {
  const el = target instanceof HTMLElement ? target : null;
  if (!el) return null;

  const img =
    el.tagName === "IMG"
      ? (el as HTMLImageElement)
      : el.closest("img");
  if (img) {
    const src = img.currentSrc || img.getAttribute("src") || "";
    const fromSrc = artifactPathFromSessionFileUrl(src);
    const fromLink = img
      .closest("a.artifact-path")
      ?.getAttribute("data-artifact");
    const path = fromSrc || fromLink || "";
    if (path) return { path, expand: true };
  }

  const link = el.closest("a.artifact-path");
  if (link) {
    const path = link.getAttribute("data-artifact") || "";
    if (!path) return null;
    return { path, expand: shouldLightboxArtifact(path) };
  }

  return null;
}
