import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAppStore } from "../../store";
import { ThreadView } from "./ThreadView";

vi.mock("./ThreadHeader", () => ({ ThreadHeader: () => null }));
vi.mock("./ConversationPanel", () => ({ ConversationPanel: () => <div>Chat</div> }));
vi.mock("./WorkspacePanel", () => ({ WorkspacePanel: () => <div>Workspace</div> }));
vi.mock("../WorkerDetailsPanel", () => ({ WorkerDetailsPanel: () => null }));
vi.mock("../../lib/useSelectedAgent", () => ({ useSelectedAgent: () => [null, vi.fn()] }));
let width = 1710;
let notify: () => void;
const panelWidth = () => parseFloat((document.querySelector(".ka-canvas-panel") as HTMLElement).style.width);
beforeEach(() => {
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => { storage.set(key, value); },
    removeItem: (key: string) => { storage.delete(key); } });
  localStorage.setItem("kageha.rightPanelWidth", "750");
  vi.stubGlobal("innerWidth", 1920);
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { notify = callback; }
    observe() {} disconnect() {}
  });
  width = 1710;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(() => ({width, height: 900, x: 0, y: 0, left: 0, right: width, top: 0, bottom: 900, toJSON() { return {}; }}));
  useAppStore.setState({ sessionId: "split-layout", machineActive: false, liveSurface: "browser", canvasItems: [] });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("follows parent size changes and preserves the desktop split across phone overlays", () => {
  render(<ThreadView onOpenLightbox={vi.fn()} rightPanelCollapsed={false} />);
  expect(panelWidth()).toBe(750);
  act(() => { width = 1156; notify(); });
  expect(panelWidth()).toBeCloseTo(750 * 1156 / 1710);
  const desktop = panelWidth();
  act(() => { vi.stubGlobal("innerWidth", 390); width = 390; notify(); });
  expect(panelWidth()).toBe(desktop);
  act(() => { vi.stubGlobal("innerWidth", 1920); width = 1710; notify(); });
  expect(panelWidth()).toBeCloseTo(750);
});

it("uses a sensible desktop proportion when first opened on a phone with no saved split", () => {
  localStorage.removeItem("kageha.rightPanelWidth");
  vi.stubGlobal("innerWidth", 390);
  width = 390;
  render(<ThreadView onOpenLightbox={vi.fn()} rightPanelCollapsed={false} />);
  act(() => { vi.stubGlobal("innerWidth", 1920); width = 1710; notify(); });
  expect(panelWidth()).toBeCloseTo(1710 * .44);
});

it("makes the divider discoverable and resizes it with the arrow keys", () => {
  render(<ThreadView onOpenLightbox={vi.fn()} rightPanelCollapsed={false} />);
  const divider = screen.getByRole("separator", { name: "Resize chat and workspace panels" });
  expect(divider).toHaveAttribute("title", expect.stringContaining("arrow keys"));
  fireEvent.keyDown(divider, { key: "ArrowLeft" });
  expect(panelWidth()).toBe(774);
  fireEvent.keyDown(divider, { key: "ArrowRight" });
  expect(panelWidth()).toBe(750);
  act(() => { width = 900; notify(); });
  fireEvent.keyDown(divider, { key: "ArrowLeft" });
  expect(panelWidth()).toBeLessThanOrEqual(500);
});
