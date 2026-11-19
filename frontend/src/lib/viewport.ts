/** Matches `@media (max-width: 1023px)` in app.css — phone/tablet chat-first layout. */
export const NARROW_VIEWPORT_MAX_PX = 1023;

function defaultMediaQuery(): Pick<MediaQueryList, "matches"> | null {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return null;
  }
  return window.matchMedia(`(max-width: ${NARROW_VIEWPORT_MAX_PX}px)`);
}

/** True when the workspace should stay collapsed so chat fills the viewport. */
export function isNarrowViewport(
  media: Pick<MediaQueryList, "matches"> | null | undefined = defaultMediaQuery(),
): boolean {
  return Boolean(media?.matches);
}
