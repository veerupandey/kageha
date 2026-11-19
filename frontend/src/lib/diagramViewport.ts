const contentBoundsCache = new WeakMap<SVGSVGElement, [number, number, number, number]>();

/**
 * Extract the true visual content bounds of an SVG.
 * Mermaid (especially flowcharts) often leaves an inflated viewBox (e.g. >2000px height)
 * due to temporary measuring foreignObjects. Measuring the real graphical content via
 * getBBox() recovers the tight, accurate bounds. Results are cached per SVG element.
 */
export function getSvgContentBounds(svg: SVGSVGElement): [number, number, number, number] | null {
  const cached = contentBoundsCache.get(svg);
  if (cached) return cached;
  try {
    if (typeof svg.getBBox === "function") {
      const b = svg.getBBox();
      if (Number.isFinite(b.x) && Number.isFinite(b.y) && b.width > 0 && b.height > 0) {
        let result: [number, number, number, number] | null = null;
        if (b.width > 1800 || b.height > 1800) {
          const mainG = svg.querySelector<SVGGElement>("g");
          if (mainG && typeof mainG.getBBox === "function") {
            const gb = mainG.getBBox();
            if (Number.isFinite(gb.x) && Number.isFinite(gb.y) && gb.width > 0 && gb.height > 0 && gb.width < 1800 && gb.height < 1800) {
              const pad = 24;
              result = [gb.x - pad, gb.y - pad, gb.width + pad * 2, gb.height + pad * 2];
            }
          }
        }
        if (!result) {
          const pad = 24;
          result = [b.x - pad, b.y - pad, b.width + pad * 2, b.height + pad * 2];
        }
        contentBoundsCache.set(svg, result);
        return result;
      }
    }
  } catch {
    // getBBox might throw if element is detached or layout is unavailable
  }
  return null;
}

export function parseViewBox(svg: SVGSVGElement): [number, number, number, number] {
  const values = svg.getAttribute("viewBox")?.trim().split(/[\s,]+/).map(Number);
  if (values?.length === 4 && values.every(Number.isFinite) && values[2] > 0 && values[3] > 0) {
    return [values[0], values[1], values[2], values[3]];
  }
  return [0, 0, 800, 600];
}

/** Navigate the SVG camera, rather than transforming an overflowing DOM box. */
export function diagramViewport(svg: SVGSVGElement, viewport: HTMLElement, onZoom: (zoom: number) => void, options: {
  wheelRequiresModifier?: boolean;
  touchEnabled?: () => boolean;
  onResize?: () => void;
} = {}) {
  let bounds = getSvgContentBounds(svg) || parseViewBox(svg);
  let camera = [...bounds];
  let zoom = 1;
  const pointers = new Map<number, { x: number; y: number; startX: number; startY: number }>();
  let dragged = false;
  let activeViewportRect: DOMRect | null = null;

  const render = () => {
    svg.setAttribute("viewBox", camera.join(" "));
    onZoom(zoom);
  };
  const fit = () => {
    const fresh = getSvgContentBounds(svg);
    if (fresh) bounds = fresh;
    const rect = activeViewportRect || viewport.getBoundingClientRect();
    const aspect = rect.width > 0 && rect.height > 0 ? rect.width / rect.height : bounds[2] / bounds[3];
    const width = Math.max(bounds[2], bounds[3] * aspect);
    const height = width / aspect;
    camera = [bounds[0] + (bounds[2] - width) / 2, bounds[1] + (bounds[3] - height) / 2, width, height];
    zoom = 1;
    render();
  };
  const zoomTo = (next: number, x = 0.5, y = 0.5) => {
    next = Math.min(12, Math.max(0.1, next));
    const factor = zoom / next;
    camera = [camera[0] + camera[2] * x * (1 - factor), camera[1] + camera[3] * y * (1 - factor), camera[2] * factor, camera[3] * factor];
    zoom = next;
    render();
  };
  const pan = (dx: number, dy: number) => {
    const rect = activeViewportRect || viewport.getBoundingClientRect();
    camera[0] -= dx * camera[2] / (rect.width || 1);
    camera[1] -= dy * camera[3] / (rect.height || 1);
    render();
  };
  const abort = new AbortController();
  const listenerOptions = { signal: abort.signal };
  viewport.addEventListener("wheel", (event) => {
    if (options.wheelRequiresModifier && !event.ctrlKey && !event.metaKey) return;
    event.preventDefault();
    const rect = activeViewportRect || viewport.getBoundingClientRect();
    const delta = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? rect.height : 1);
    zoomTo(zoom * Math.exp(-Math.max(-200, Math.min(200, delta)) * 0.003), (event.clientX - rect.left) / (rect.width || 1), (event.clientY - rect.top) / (rect.height || 1));
  }, { ...listenerOptions, passive: false });
  viewport.addEventListener("pointerdown", (event) => {
    if (event.pointerType === "touch" && options.touchEnabled && !options.touchEnabled()) return;
    if (event.button !== 0 || (event.target as Element).closest("a,button")) return;
    if (!pointers.size) dragged = false;
    activeViewportRect = viewport.getBoundingClientRect();
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY, startX: event.clientX, startY: event.clientY });
  }, listenerOptions);
  viewport.addEventListener("pointermove", (event) => {
    const previous = pointers.get(event.pointerId);
    if (!previous) return;
    const dx = event.clientX - previous.x;
    const dy = event.clientY - previous.y;
    if (Math.hypot(event.clientX - previous.startX, event.clientY - previous.startY) > 4) {
      dragged = true;
      if (!viewport.hasPointerCapture(event.pointerId)) viewport.setPointerCapture(event.pointerId);
      viewport.classList.add("is-panning");
    }
    const other = [...pointers.entries()].find(([id]) => id !== event.pointerId)?.[1];
    if (other) {
      const before = Math.hypot(previous.x - other.x, previous.y - other.y);
      const after = Math.hypot(event.clientX - other.x, event.clientY - other.y);
      const rect = activeViewportRect || viewport.getBoundingClientRect();
      if (before > 0) zoomTo(zoom * after / before, ((previous.x + other.x) / 2 - rect.left) / rect.width, ((previous.y + other.y) / 2 - rect.top) / rect.height);
      pan(dx / 2, dy / 2);
      dragged = true;
    } else pan(dx, dy);
    pointers.set(event.pointerId, { ...previous, x: event.clientX, y: event.clientY });
  }, listenerOptions);
  const release = (event: PointerEvent) => {
    pointers.delete(event.pointerId);
    if (viewport.hasPointerCapture(event.pointerId)) viewport.releasePointerCapture(event.pointerId);
    if (!pointers.size) {
      activeViewportRect = null;
      viewport.classList.remove("is-panning");
    }
  };
  viewport.addEventListener("pointerup", release, listenerOptions);
  viewport.addEventListener("pointercancel", release, listenerOptions);
  viewport.addEventListener("lostpointercapture", (event) => {
    // Touch starts with implicit capture on the hit SVG child. Its bubbling
    // loss event during handoff must not cancel our newly captured gesture.
    if (event.target === viewport) release(event);
  }, listenerOptions);
  // Keep node clicks working while suppressing the click generated after a drag.
  viewport.addEventListener("click", (event) => {
    if (!dragged) return;
    dragged = false;
    event.preventDefault();
    event.stopImmediatePropagation();
  }, { ...listenerOptions, capture: true });
  viewport.addEventListener("dblclick", (event) => {
    if ((event.target as Element).closest("a")) return;
    fit();
  }, listenerOptions);
  viewport.addEventListener("keydown", (event) => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === "+" || event.key === "=") zoomTo(zoom * 1.25);
    else if (event.key === "-") zoomTo(zoom * 0.8);
    else if (event.key === "0") fit();
    else if (event.key === "ArrowLeft") pan(48, 0);
    else if (event.key === "ArrowRight") pan(-48, 0);
    else if (event.key === "ArrowUp") pan(0, 48);
    else if (event.key === "ArrowDown") pan(0, -48);
    else return;
    event.preventDefault();
    event.stopPropagation();
  }, listenerOptions);
  const resize = typeof ResizeObserver !== "undefined" ? new ResizeObserver(() => { options.onResize?.(); fit(); }) : null;
  resize?.observe(viewport);
  fit();
  return {
    fit,
    zoomBy: (factor: number) => zoomTo(zoom * factor),
    pan,
    destroy: () => { abort.abort(); resize?.disconnect(); },
  };
}
