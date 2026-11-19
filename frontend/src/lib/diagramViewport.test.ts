import { afterEach, describe, expect, it, vi } from "vitest";
import { diagramViewport } from "./diagramViewport";

function fixture(bounds: string, width = 800, height = 600) {
  const viewport = document.createElement("div");
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", bounds);
  viewport.append(svg);
  document.body.append(viewport);
  vi.spyOn(viewport, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, left: 0, top: 0, right: width, bottom: height, width, height, toJSON() {} });
  const zoom = vi.fn();
  const camera = diagramViewport(svg, viewport, zoom);
  const view = () => svg.getAttribute("viewBox")!.split(" ").map(Number);
  return { viewport, camera, zoom, view };
}

afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });

describe("diagram camera", () => {
  it.each(["-10 -20 4000 200", "-10 -20 200 4000"])("fits all bounds of %s and preserves viewport aspect", (input) => {
    const { camera, view } = fixture(input);
    const [x, y, w, h] = input.split(" ").map(Number);
    const [cx, cy, cw, ch] = view();
    expect(cx).toBeLessThanOrEqual(x);
    expect(cy).toBeLessThanOrEqual(y);
    expect(cx + cw).toBeGreaterThanOrEqual(x + w);
    expect(cy + ch).toBeGreaterThanOrEqual(y + h);
    expect(cw / ch).toBeCloseTo(800 / 600);
    camera.destroy();
  });

  it("zooms around the pointer and restores the complete diagram with Fit", () => {
    const { viewport, camera, view } = fixture("0 0 800 600");
    const initial = view();
    viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -150, clientX: 200, clientY: 150, cancelable: true }));
    const [x, y, w, h] = view();
    expect(w).toBeLessThan(800);
    expect(x + w * 0.25).toBeCloseTo(200);
    expect(y + h * 0.25).toBeCloseTo(150);
    camera.pan(80, 60);
    expect(view()[0]).toBeLessThan(x);
    camera.fit();
    expect(view()).toEqual(initial);
    camera.destroy();
  });

  it("allows a large diagram to be explored at high zoom and removes listeners on cleanup", () => {
    const { viewport, camera, zoom, view } = fixture("0 0 8000 6000");
    camera.zoomBy(100);
    expect(zoom).toHaveBeenLastCalledWith(12);
    expect(view()[2]).toBeCloseTo(8000 / 12);
    camera.destroy();
    const last = view();
    viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: 100, clientX: 200, clientY: 150 }));
    expect(view()).toEqual(last);
  });

  it("continues panning when an SVG child hands touch capture to the viewport", () => {
    const { viewport, camera, view } = fixture("0 0 800 600");
    const child = viewport.querySelector("svg")!;
    let captured = false;
    viewport.setPointerCapture = vi.fn(() => { captured = true; });
    viewport.hasPointerCapture = vi.fn(() => captured);
    viewport.releasePointerCapture = vi.fn(() => { captured = false; });
    const pointer = (target: Element, type: string, x: number) => {
      const event = new MouseEvent(type, { bubbles: true, clientX: x, clientY: 100, button: 0 });
      Object.defineProperty(event, "pointerId", { value: 1 });
      target.dispatchEvent(event);
    };
    pointer(child, "pointerdown", 100);
    pointer(viewport, "pointermove", 120);
    const beforeHandoff = view();
    pointer(child, "lostpointercapture", 120);
    pointer(viewport, "pointermove", 150);
    expect(view()[0]).toBeLessThan(beforeHandoff[0]);
    pointer(viewport, "pointerup", 150);
    expect(viewport.releasePointerCapture).toHaveBeenCalledWith(1);
    camera.destroy();
  });

  it("recovers true bounds from getBBox when viewBox is inflated by Mermaid measuring artifacts", () => {
    const viewport = document.createElement("div");
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    // Inflated viewBox e.g. from Mermaid flowchart measuring bug
    svg.setAttribute("viewBox", "-164 -104 2188 2128");
    // True content bounds
    Object.defineProperty(svg, "getBBox", {
      value: () => ({ x: 8, y: 8, width: 800, height: 400 }),
    });
    viewport.append(svg);
    document.body.append(viewport);
    vi.spyOn(viewport, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, left: 0, top: 0, right: 800, bottom: 600, width: 800, height: 600, toJSON() {} });
    const camera = diagramViewport(svg, viewport, () => {});
    const [, , cw, ch] = svg.getAttribute("viewBox")!.split(" ").map(Number);
    // Camera fits around true content (around 848x448 with padding), NOT 2188x2128
    expect(cw).toBeLessThan(1200);
    expect(ch).toBeLessThan(800);
    camera.destroy();
  });
});
