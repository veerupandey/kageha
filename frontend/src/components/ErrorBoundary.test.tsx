import { afterEach, describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary";

vi.mock("../store", () => ({ useAppStore: {} }));
afterEach(() => vi.unstubAllGlobals());

describe("module load recovery", () => {
  it.each([
    "Importing a module script failed.",
    "Failed to fetch dynamically imported module: http://localhost/assets/old.js",
    "error loading dynamically imported module",
  ])("recognizes %s and reloads at most once", (message) => {
    const getItem = vi.fn().mockReturnValue(null);
    vi.stubGlobal("sessionStorage", { getItem });
    expect(ErrorBoundary.getDerivedStateFromError(new TypeError(message)).reloading).toBe(true);
    getItem.mockReturnValue("1");
    expect(ErrorBoundary.getDerivedStateFromError(new TypeError(message)).reloading).toBe(false);
  });

  it("does not reload unrelated errors", () => {
    expect(ErrorBoundary.getDerivedStateFromError(new Error("render failed")).reloading).toBe(false);
  });

  it("does not reload endlessly when browser storage is blocked", () => {
    vi.stubGlobal("sessionStorage", { getItem: () => { throw new Error("blocked"); } });
    expect(ErrorBoundary.getDerivedStateFromError(new TypeError("Importing a module script failed.")).reloading).toBe(false);
  });
});
