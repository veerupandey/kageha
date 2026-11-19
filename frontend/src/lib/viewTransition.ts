/**
 * Executes a UI state transition wrapped in the browser's native View Transitions API
 * when supported, falling back to immediate execution otherwise. Respects user's
 * prefers-reduced-motion setting.
 */
export function withViewTransition(fn: () => void | Promise<void>): void {
  if (
    typeof document !== "undefined" &&
    "startViewTransition" in document &&
    typeof (document as { startViewTransition?: unknown }).startViewTransition === "function" &&
    !window.matchMedia?.("(prefers-reduced-motion: reduce)").matches
  ) {
    (document as { startViewTransition: (cb: () => void | Promise<void>) => void }).startViewTransition(() => {
      return fn();
    });
  } else {
    void fn();
  }
}
