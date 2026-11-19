export type DiagramBounds = { x: number; y: number; width: number; height: number };
export type DiagramQuality = { padded: boolean; smallLabels: boolean; overlappingNodes: number; overflowingLabels: number };

export function paddedDiagramBounds(view: DiagramBounds, content: DiagramBounds, padding = 16): DiagramBounds {
  if (content.x >= view.x && content.y >= view.y && content.x + content.width <= view.x + view.width && content.y + content.height <= view.y + view.height) return view;
  const x = Math.min(view.x, content.x - padding);
  const y = Math.min(view.y, content.y - padding);
  return { x, y, width: Math.max(view.x + view.width, content.x + content.width + padding) - x, height: Math.max(view.y + view.height, content.y + content.height + padding) - y };
}

function overlaps(a: DOMRect, b: DOMRect): boolean {
  const width = Math.min(a.right, b.right) - Math.max(a.left, b.left);
  const height = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
  return width > 3 && height > 3 && width * height > Math.min(a.width * a.height, b.width * b.height) * 0.08;
}

/** Measure actual browser geometry. Correct outer clipping; report layout issues. */
export function checkDiagramQuality(svg: SVGSVGElement): DiagramQuality {
  const result: DiagramQuality = { padded: false, smallLabels: false, overlappingNodes: 0, overflowingLabels: 0 };
  if (!svg.isConnected || typeof svg.getBBox !== "function") return result;
  try {
    const view = svg.viewBox.baseVal;
    const content = svg.getBBox();
    if (view.width <= 0 || view.height <= 0 || content.width <= 0 || content.height <= 0) return result;
    const padded = paddedDiagramBounds(view, content);
    if (padded !== view) {
      svg.setAttribute("viewBox", `${padded.x} ${padded.y} ${padded.width} ${padded.height}`);
      result.padded = true;
    }
    const rect = svg.getBoundingClientRect();
    const fitScale = Math.min(rect.width / padded.width, rect.height / padded.height);
    const textElements = [...svg.querySelectorAll<SVGElement>("text, .nodeLabel, .edgeLabel")].filter((el) => el.textContent?.trim());
    result.smallLabels = textElements.some((el) => {
      const size = Number.parseFloat(getComputedStyle(el).fontSize);
      return Number.isFinite(size) && size * fitScale < 11;
    });
    const nodes = [...svg.querySelectorAll<SVGGraphicsElement>(".node")].slice(0, 100);
    const shapes = nodes.map((node) => node.querySelector<SVGGraphicsElement>(".label-container, rect, polygon, circle, ellipse")?.getBoundingClientRect());
    shapes.forEach((shape, i) => {
      if (!shape?.width || !shape.height) return;
      for (let j = i + 1; j < shapes.length; j++) {
        const other = shapes[j];
        if (other?.width && other.height && !nodes[i].contains(nodes[j]) && !nodes[j].contains(nodes[i]) && overlaps(shape, other)) result.overlappingNodes++;
      }
      const label = nodes[i].querySelector<SVGGraphicsElement>(".label");
      if (!label) return;
      const box = label.getBoundingClientRect();
      if (box.width > shape.width + 3 || box.height > shape.height + 3) result.overflowingLabels++;
    });
  } catch { /* SVGs in hidden tabs or unsupported diagram types may not be measurable. */ }
  return result;
}

export function showDiagramQuality(host: HTMLElement, quality: DiagramQuality) {
  const notes: string[] = [];
  if (quality.padded) notes.push("Added space around the diagram to keep all content visible.");
  if (quality.smallLabels) notes.push("Some labels are small at this size. Zoom in or open Explore for a closer look.");
  if (quality.overlappingNodes) notes.push("Some components overlap. A simpler layout or a separate detail diagram may be easier to read.");
  if (quality.overflowingLabels) notes.push("Some labels extend beyond their shapes. Shorter labels may improve readability.");
  host.replaceChildren();
  host.hidden = !notes.length;
  host.dataset.quality = quality.overlappingNodes || quality.overflowingLabels ? "needs-attention" : "ok";
  if (!notes.length) return;
  const summary = document.createElement("summary");
  summary.textContent = `Layout notes (${notes.length})`;
  const list = document.createElement("ul");
  notes.forEach((note) => { const item = document.createElement("li"); item.textContent = note; list.append(item); });
  host.append(summary, list);
}
