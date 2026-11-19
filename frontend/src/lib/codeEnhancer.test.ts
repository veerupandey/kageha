import { describe, expect, it } from "vitest";
import { afterEach, vi } from "vitest";
import mermaid from "mermaid";
import { enhanceCodeBlocks, initMermaidBlocks } from "./codeEnhancer";

vi.mock("mermaid", () => ({ default: {
  initialize: vi.fn(),
  mermaidAPI: { getDiagramFromText: vi.fn(async () => ({ db: { getEdges: () => [{ start: "A", end: "B", text: "requests" }] } })) },
  render: vi.fn(async (id: string) => ({ svg: `<svg id="${id}" viewBox="0 0 1200 200" style="max-width:1200px" xmlns="http://www.w3.org/2000/svg"><style>#${id} .node {fill:blue}</style><defs><marker id="arrow" /></defs><g id="${id}-flowchart-A-0" class="node"><text>Entry</text></g><g id="${id}-flowchart-B-1" class="node"><text>Worker</text></g><path class="flowchart-link" data-id="L_A_B_0" marker-end="url(#arrow)" /></svg>` })),
} }));

afterEach(() => {
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  document.body.replaceChildren();
});

describe("codeEnhancer", () => {
  it("turns Mermaid fences into renderable diagram mounts with source fallback", () => {
    const html = "<pre><code class=\"language-mermaid\">flowchart LR\n A-->B</code></pre>";
    const out = enhanceCodeBlocks(html);
    expect(out).toContain('class="mermaid-block"');
    expect(out).toContain('class="mermaid-diagram"');
    expect(out).toContain('class="mermaid-expand"');
    expect(out).toContain('class="mermaid-source"');
    expect(out).toContain("flowchart LR");
    expect(out).not.toContain("mermaid.live");
  });

  it("opens an interactive diagram, fits after zooming, inspects connections, and restores focus", async () => {
    const container = document.createElement("div");
    container.innerHTML = enhanceCodeBlocks('<pre><code class="language-mermaid">flowchart LR\nA-->B</code></pre>');
    document.body.append(container);
    initMermaidBlocks(container);
    await vi.waitFor(() => expect(container.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());
    const button = container.querySelector<HTMLButtonElement>(".mermaid-expand")!;
    button.focus();
    button.click();
    const overlay = document.querySelector<HTMLElement>('[role="dialog"]')!;
    const svg = overlay.querySelector("svg")!;
    const initial = svg.getAttribute("viewBox");
    expect(document.body.style.overflow).toBe("hidden");
    expect(svg.querySelector("path")!.getAttribute("marker-end")).toBe("url(#arrow-explore)");
    expect(svg.querySelector("style")!.textContent).toContain(`#${svg.id} .node`);
    overlay.querySelector<HTMLButtonElement>('[data-action="in"]')!.click();
    expect(svg.getAttribute("viewBox")).not.toBe(initial);
    overlay.querySelector<HTMLButtonElement>('[data-action="fit"]')!.click();
    expect(svg.getAttribute("viewBox")).toBe(initial);
    svg.querySelector(".node")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(overlay.querySelector(".mermaid-inspector")!.textContent).toContain("Entry");
    expect(overlay.querySelector(".mermaid-inspector")!.textContent).toContain("Outgoing");
    expect(overlay.querySelector(".mermaid-inspector")!.textContent).toContain("Worker · requests");
    expect(overlay.querySelectorAll(".mermaid-edge-selected")).toHaveLength(1);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(document.activeElement).toBe(button);
    expect(document.body.style.overflow).toBe("");
  });

  it("navigates inline without taking over page scrolling and shows authored evidence", async () => {
    const container = document.createElement("div");
    const source = 'flowchart LR\n%% @node A {"description":"Accepts incoming requests","files":["src/api.ts:12"]}\nA-->B';
    container.innerHTML = enhanceCodeBlocks(`<pre><code class="language-mermaid">${source}</code></pre>`);
    document.body.append(container);
    initMermaidBlocks(container);
    await vi.waitFor(() => expect(container.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());
    const viewport = container.querySelector<HTMLElement>(".mermaid-diagram")!;
    const svg = viewport.querySelector("svg")!;
    const initial = svg.getAttribute("viewBox");
    const scroll = new WheelEvent("wheel", { deltaY: -100, cancelable: true });
    viewport.dispatchEvent(scroll);
    expect(scroll.defaultPrevented).toBe(false);
    expect(svg.getAttribute("viewBox")).toBe(initial);
    viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -100, ctrlKey: true, cancelable: true }));
    expect(svg.getAttribute("viewBox")).not.toBe(initial);
    const fit = container.querySelector<HTMLButtonElement>('[data-inline-action="fit"]')!;
    fit.click();
    expect(svg.getAttribute("viewBox")).toBe(initial);
    svg.querySelector(".node")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(document.querySelector(".mermaid-overlay")).toBeNull();
    const inspector = container.querySelector<HTMLElement>(".mermaid-inspector")!;
    expect(inspector.textContent).toContain("Accepts incoming requests");
    expect(inspector.textContent).toContain("src/api.ts:12");
    inspector.querySelector<HTMLButtonElement>(".mermaid-connections button")!.click();
    expect(inspector.textContent).toContain("Worker");
    expect(inspector.textContent).toContain("Incoming");
    expect(inspector.textContent).toContain("Entry · requests");
    const pan = container.querySelector<HTMLButtonElement>('[data-inline-action="pan"]')!;
    pan.click();
    expect(viewport.classList.contains("touch-panning")).toBe(true);
    expect(pan.getAttribute("aria-pressed")).toBe("true");
    container.remove();
    await new Promise((resolve) => setTimeout(resolve, 0));
    const before = svg.getAttribute("viewBox");
    container.querySelector<HTMLButtonElement>('[data-inline-action="in"]')!.click();
    expect(svg.getAttribute("viewBox")).toBe(before);
  });

  it("activates a touch control once even if the browser also sends a synthetic click", async () => {
    const container = document.createElement("div");
    container.innerHTML = enhanceCodeBlocks('<pre><code class="language-mermaid">flowchart LR\nA-->B</code></pre>');
    document.body.append(container);
    initMermaidBlocks(container);
    await vi.waitFor(() => expect(container.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());
    const button = container.querySelector<HTMLButtonElement>('[data-inline-action="in"]')!;
    const end = new Event("touchend", { cancelable: true });
    Object.defineProperties(end, { touches: { value: [] }, changedTouches: { value: [{ clientX: 0, clientY: 0 }] } });
    button.dispatchEvent(end);
    expect(end.defaultPrevented).toBe(true);
    expect(container.querySelector("output")!.textContent).toBe("125%");
    const click = new MouseEvent("click", { detail: 1 });
    Object.defineProperty(click, "pointerType", { value: "touch" });
    button.dispatchEvent(click);
    expect(container.querySelector("output")!.textContent).toBe("125%");
    button.click();
    expect(container.querySelector("output")!.textContent).toBe("156%");
  });

  it("treats authored component descriptions and file names as plain text", async () => {
    const container = document.createElement("div");
    container.innerHTML = enhanceCodeBlocks('<pre><code class="language-mermaid">flowchart LR\n%% @node A {"description":"&lt;img src=x onerror=alert(1)&gt;","files":["javascript:alert(1)"]}\nA-->B</code></pre>');
    document.body.append(container);
    initMermaidBlocks(container);
    await vi.waitFor(() => expect(container.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());
    container.querySelector(".node")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    const inspector = container.querySelector(".mermaid-inspector")!;
    expect(inspector.textContent).toContain("<img src=x onerror=alert(1)>");
    expect(inspector.querySelector("img, a, script")).toBeNull();
  });

  it("adapts inline canvas height to diagram aspect ratio", async () => {
    const container = document.createElement("div");
    container.innerHTML = enhanceCodeBlocks('<pre><code class="language-mermaid">flowchart LR\nA-->B</code></pre>');
    document.body.append(container);
    initMermaidBlocks(container);
    await vi.waitFor(() => expect(container.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());
    const viewport = container.querySelector<HTMLElement>(".mermaid-diagram")!;
    expect(viewport.style.getPropertyValue("--mermaid-height")).toBeTruthy();
  });

  it("opens diagram context menu on right click with fit and source options", async () => {
    const container = document.createElement("div");
    container.innerHTML = enhanceCodeBlocks('<pre><code class="language-mermaid">flowchart LR\nA-->B</code></pre>');
    document.body.append(container);
    initMermaidBlocks(container);
    await vi.waitFor(() => expect(container.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());
    const viewport = container.querySelector<HTMLElement>(".mermaid-diagram")!;
    viewport.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 100, clientY: 100 }));
    const menu = document.querySelector(".mermaid-context-menu");
    expect(menu).not.toBeNull();
    expect(menu?.querySelector('[data-action="copy-source"]')).not.toBeNull();
    expect(menu?.querySelector('[data-action="view-source"]')).not.toBeNull();
    expect(menu?.querySelector('[data-action="fit"]')).not.toBeNull();
  });

  it("reuses cached diagram markup for identical diagram sources without re-rendering", async () => {
    const container1 = document.createElement("div");
    container1.innerHTML = enhanceCodeBlocks('<pre><code class="language-mermaid">flowchart LR\nCached1-->Cached2</code></pre>');
    document.body.append(container1);
    initMermaidBlocks(container1);
    await vi.waitFor(() => expect(container1.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());

    const callsBefore = vi.mocked(mermaid.render).mock.calls.length;

    const container2 = document.createElement("div");
    container2.innerHTML = enhanceCodeBlocks('<pre><code class="language-mermaid">flowchart LR\nCached1-->Cached2</code></pre>');
    document.body.append(container2);
    initMermaidBlocks(container2);
    await vi.waitFor(() => expect(container2.querySelector('[data-mermaid-state="rendered"]')).not.toBeNull());

    expect(vi.mocked(mermaid.render).mock.calls.length).toBe(callsBefore);
    expect(container2.querySelector("svg")).not.toBeNull();
  });
});


it("retains every CSV row in the rendered table", () => {
  const csv = "row,value\n" + Array.from({ length: 150 }, (_, i) => `${i},value-${i}`).join("\n");
  const html = enhanceCodeBlocks(`<pre><code class="language-csv">${csv}</code></pre>`);
  const el = document.createElement("div");
  el.innerHTML = html;
  expect(el.querySelectorAll("tbody tr")).toHaveLength(150);
  expect(el.querySelector("tbody")?.textContent).toContain("value-149");
});
