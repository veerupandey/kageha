import { afterEach, describe, expect, it, vi } from "vitest";
import { checkDiagramQuality, paddedDiagramBounds, showDiagramQuality } from "./diagramQuality";

afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });

describe("diagram layout checks", () => {
  it("expands all four sides around out-of-bounds content without shrinking existing bounds", () => {
    const bounds = paddedDiagramBounds({ x: 0, y: 0, width: 800, height: 600 }, { x: -20, y: -10, width: 850, height: 650 });
    expect(bounds.x).toBeLessThan(-20);
    expect(bounds.y).toBeLessThan(-10);
    expect(bounds.x + bounds.width).toBeGreaterThan(830);
    expect(bounds.y + bounds.height).toBeGreaterThan(640);
    const original = { x: 0, y: 0, width: 800, height: 600 };
    expect(paddedDiagramBounds(original, { x: 10, y: 10, width: 100, height: 100 })).toBe(original);
  });

  it("detects measured label overflow, overlapping nodes, and unreadable overview text", () => {
    const host = document.createElement("div");
    host.innerHTML = '<svg viewBox="0 0 800 600"><g class="node"><rect/><g class="label"><text style="font-size:20px">API</text></g></g><g class="node"><rect/></g></svg>';
    document.body.append(host);
    const svg = host.querySelector("svg")!;
    Object.defineProperty(svg, "viewBox", { value: { baseVal: { x: 0, y: 0, width: 800, height: 600 } } });
    Object.defineProperty(svg, "getBBox", { value: () => ({ x: -10, y: 0, width: 800, height: 600 }) });
    const box = (x: number, y: number, width: number, height: number) => ({ x, y, left: x, top: y, right: x + width, bottom: y + height, width, height, toJSON() {} });
    vi.spyOn(svg, "getBoundingClientRect").mockReturnValue(box(0, 0, 300, 200));
    const rects = svg.querySelectorAll("rect");
    vi.spyOn(rects[0], "getBoundingClientRect").mockReturnValue(box(10, 10, 100, 50));
    vi.spyOn(rects[1], "getBoundingClientRect").mockReturnValue(box(60, 20, 100, 50));
    vi.spyOn(svg.querySelector(".label")!, "getBoundingClientRect").mockReturnValue(box(5, 5, 120, 60));
    const result = checkDiagramQuality(svg);
    expect(result).toEqual({ padded: true, smallLabels: true, overlappingNodes: 1, overflowingLabels: 1 });
    expect(svg.getAttribute("viewBox")).toBe("-26 -16 832 632");
    const notes = document.createElement("details");
    showDiagramQuality(notes, result);
    expect(notes.hidden).toBe(false);
    expect(notes.dataset.quality).toBe("needs-attention");
    expect(notes.querySelectorAll("li")).toHaveLength(4);
  });

  it("does not report a clean diagram as needing attention", () => {
    const notes = document.createElement("details");
    showDiagramQuality(notes, { padded: false, smallLabels: false, overlappingNodes: 0, overflowingLabels: 0 });
    expect(notes.hidden).toBe(true);
    expect(notes.dataset.quality).toBe("ok");
  });
});
