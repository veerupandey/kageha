import { diagramViewport, getSvgContentBounds } from "./diagramViewport";
import { diagramAnnotations, diagramEdges, diagramInspector, type DiagramModel } from "./diagramDetails";
import { checkDiagramQuality, showDiagramQuality } from "./diagramQuality";

/**
 * Enhance rendered markdown HTML with:
 * - Copy buttons on all code blocks
 * - Syntax highlighting for common languages (lightweight, no external lib)
 * - Collapsible long code blocks (>20 lines)
 * - CSV blocks rendered as tables
 * - Mermaid blocks rendered client-side with a safe source fallback
 * - YAML syntax coloring
 */

const LANG_LABELS: Record<string, string> = {
  js: "JavaScript",
  javascript: "JavaScript",
  ts: "TypeScript",
  typescript: "TypeScript",
  tsx: "TypeScript",
  jsx: "JavaScript",
  py: "Python",
  python: "Python",
  bash: "Bash",
  sh: "Shell",
  shell: "Shell",
  zsh: "Shell",
  json: "JSON",
  yaml: "YAML",
  yml: "YAML",
  html: "HTML",
  css: "CSS",
  sql: "SQL",
  rust: "Rust",
  rs: "Rust",
  go: "Go",
  java: "Java",
  rb: "Ruby",
  ruby: "Ruby",
  cpp: "C++",
  c: "C",
  md: "Markdown",
  markdown: "Markdown",
  toml: "TOML",
  dockerfile: "Dockerfile",
  csv: "CSV",
  mermaid: "Mermaid",
  text: "Text",
  plaintext: "Text",
};

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function unescapeHtml(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'");
}

/** Lightweight syntax highlighting for Python */
function highlightPython(code: string): string {
  return code
    .replace(/\b(def|class|import|from|return|if|elif|else|for|while|with|as|try|except|finally|raise|yield|lambda|and|or|not|in|is|True|False|None|async|await|pass|break|continue)\b/g, '<span class="syn-kw">$1</span>')
    .replace(/(#[^\n]*)/g, '<span class="syn-comment">$1</span>')
    .replace(/(&quot;{3}[\s\S]*?&quot;{3}|&quot;[^&]*?&quot;|&#39;[^&]*?&#39;)/g, '<span class="syn-str">$1</span>')
    .replace(/\b(\d+\.?\d*)\b/g, '<span class="syn-num">$1</span>');
}

/** Lightweight syntax highlighting for JS/TS */
function highlightJs(code: string): string {
  return code
    .replace(/\b(const|let|var|function|return|if|else|for|while|class|import|export|from|default|async|await|new|this|typeof|instanceof|throw|try|catch|finally|switch|case|break|continue|yield|true|false|null|undefined|void)\b/g, '<span class="syn-kw">$1</span>')
    .replace(/(\/\/[^\n]*)/g, '<span class="syn-comment">$1</span>')
    .replace(/(&quot;[^&]*?&quot;|&#39;[^&]*?&#39;|`[^`]*?`)/g, '<span class="syn-str">$1</span>')
    .replace(/\b(\d+\.?\d*)\b/g, '<span class="syn-num">$1</span>');
}

/** Lightweight syntax highlighting for Bash/Shell */
function highlightBash(code: string): string {
  return code
    .replace(/(#[^\n]*)/g, '<span class="syn-comment">$1</span>')
    .replace(/\b(if|then|else|elif|fi|for|do|done|while|case|esac|function|return|exit|export|source|alias|cd|ls|cat|grep|echo|rm|mkdir|cp|mv|chmod|sudo|apt|pip|npm|git|docker|curl|wget)\b/g, '<span class="syn-kw">$1</span>')
    .replace(/(&quot;[^&]*?&quot;|&#39;[^&]*?&#39;)/g, '<span class="syn-str">$1</span>')
    .replace(/(\$\w+|\$\{[^}]+\})/g, '<span class="syn-var">$1</span>');
}

/** Lightweight syntax highlighting for YAML */
function highlightYaml(code: string): string {
  return code
    .replace(/(#[^\n]*)/g, '<span class="syn-comment">$1</span>')
    .replace(/^(\s*[\w.-]+)(:)/gm, '<span class="syn-kw">$1</span><span class="syn-punct">$2</span>')
    .replace(/:\s*(&quot;[^&]*?&quot;|&#39;[^&]*?&#39;)/g, ': <span class="syn-str">$1</span>')
    .replace(/\b(true|false|null|yes|no)\b/gi, '<span class="syn-bool">$1</span>')
    .replace(/\b(\d+\.?\d*)\b/g, '<span class="syn-num">$1</span>');
}

/** Lightweight syntax highlighting for SQL */
function highlightSql(code: string): string {
  return code
    .replace(/\b(SELECT|FROM|WHERE|AND|OR|INSERT|INTO|VALUES|UPDATE|SET|DELETE|CREATE|TABLE|ALTER|DROP|INDEX|JOIN|LEFT|RIGHT|INNER|OUTER|ON|AS|ORDER|BY|GROUP|HAVING|LIMIT|OFFSET|UNION|DISTINCT|COUNT|SUM|AVG|MAX|MIN|NOT|NULL|IN|LIKE|BETWEEN|EXISTS|PRIMARY|KEY|FOREIGN|REFERENCES|CASCADE|DEFAULT|CONSTRAINT|UNIQUE|CHECK|VIEW|TRIGGER|GRANT|REVOKE)\b/gi, '<span class="syn-kw">$&</span>')
    .replace(/(--[^\n]*)/g, '<span class="syn-comment">$1</span>')
    .replace(/(&#39;[^&]*?&#39;)/g, '<span class="syn-str">$1</span>')
    .replace(/\b(\d+\.?\d*)\b/g, '<span class="syn-num">$1</span>');
}

/** Lightweight syntax highlighting for CSS */
function highlightCss(code: string): string {
  return code
    .replace(/(\/\*[\s\S]*?\*\/)/g, '<span class="syn-comment">$1</span>')
    .replace(/([\w-]+)\s*:/g, '<span class="syn-kw">$1</span>:')
    .replace(/(#[0-9a-fA-F]{3,8})\b/g, '<span class="syn-num">$1</span>')
    .replace(/(\d+\.?\d*(?:px|em|rem|%|vh|vw|s|ms))/g, '<span class="syn-num">$1</span>');
}

function highlightCode(code: string, lang: string): string {
  const l = lang.toLowerCase();
  if (l === "python" || l === "py") return highlightPython(code);
  if (["js", "javascript", "ts", "typescript", "tsx", "jsx"].includes(l)) return highlightJs(code);
  if (["bash", "sh", "shell", "zsh"].includes(l)) return highlightBash(code);
  if (["yaml", "yml", "toml"].includes(l)) return highlightYaml(code);
  if (l === "sql") return highlightSql(code);
  if (l === "css") return highlightCss(code);
  return code; // No highlighting for unknown languages
}

/** Parse CSV text into an HTML table */
function csvToTable(csv: string): string {
  const lines = csv.trim().split("\n").filter(Boolean);
  if (lines.length < 2) return ""; // Need at least header + 1 row

  const parseRow = (line: string): string[] => {
    const cells: string[] = [];
    let current = "";
    let inQuotes = false;
    for (const ch of line) {
      if (ch === '"') {
        inQuotes = !inQuotes;
      } else if (ch === "," && !inQuotes) {
        cells.push(current.trim());
        current = "";
      } else {
        current += ch;
      }
    }
    cells.push(current.trim());
    return cells;
  };

  const headers = parseRow(lines[0]);
  const rows = lines.slice(1).map(parseRow);

  let html = '<div class="csv-table-wrapper"><table class="csv-table"><thead><tr>';
  for (const h of headers) {
    html += `<th>${escapeHtml(h)}</th>`;
  }
  html += "</tr></thead><tbody>";
  for (const row of rows) {
    html += "<tr>";
    for (let i = 0; i < headers.length; i++) {
      html += `<td>${escapeHtml(row[i] || "")}</td>`;
    }
    html += "</tr>";
  }
  html += "</tbody></table>";
  html += "</div>";
  return html;
}

/** Build a mount point; initMermaidBlocks fills it asynchronously. */
function mermaidPlaceholder(code: string): string {
  return `<div class="mermaid-block" data-mermaid-source="${escapeHtml(code)}">
    <div class="mermaid-header" hidden>
      <span class="mermaid-status" aria-live="polite">Rendering…</span>
    </div>
    <div class="mermaid-inline-controls" role="group" aria-label="Diagram navigation">
      <span class="mermaid-navigation-hint">Drag to pan · Ctrl/⌘ + scroll to zoom</span>
      <button type="button" data-inline-action="pan" aria-pressed="false" title="Enable touch panning; turn off to scroll the page" disabled>Pan</button>
      <button type="button" data-inline-action="out" aria-label="Zoom out" disabled>−</button>
      <output class="mermaid-zoom-level" aria-label="Zoom level">100%</output>
      <button type="button" data-inline-action="in" aria-label="Zoom in" disabled>+</button>
      <button type="button" data-inline-action="fit" title="Fit entire diagram (0)" disabled>Fit</button>
      <button type="button" class="mermaid-expand" disabled title="Expand diagram">Explore</button>
      <button type="button" data-inline-action="menu" aria-label="Diagram options" title="More options (or right-click canvas)" disabled>···</button>
    </div>
    <div class="mermaid-diagram" role="group" aria-label="Interactive diagram. Drag to pan. Use arrow keys to pan, plus and minus to zoom, and zero to fit. Right-click for options." tabindex="0"></div>
    <div class="mermaid-inspector" aria-live="polite">Select a component to see details and connections.</div>
    <details class="mermaid-quality" hidden style="display: none;"></details>
    <pre class="mermaid-source" hidden style="display: none;">${escapeHtml(code)}</pre>
  </div>`;
}

type MermaidApi = {
  initialize: (options: Record<string, unknown>) => void;
  render: (id: string, code: string) => Promise<{ svg: string }>;
  getDiagramFromText?: (source: string) => Promise<{ db: unknown }>;
  mermaidAPI?: { getDiagramFromText?: (source: string) => Promise<{ db: unknown }> };
};

let mermaidApiPromise: Promise<MermaidApi> | null = null;

function loadMermaid(): Promise<MermaidApi> {
  mermaidApiPromise ??= import("mermaid").then((module) => {
    const api = (module.default ?? module) as unknown as MermaidApi;
    api.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: "base",
      flowchart: {
        useMaxWidth: true,
        htmlLabels: false,
        curve: "basis",
        diagramPadding: 24,
        nodeSpacing: 48,
        rankSpacing: 64,
        padding: 20,
      },
      sequence: {
        useMaxWidth: true,
        diagramMarginX: 48,
        diagramMarginY: 28,
        actorMargin: 72,
        width: 180,
        height: 64,
        messageMargin: 44,
      },
      themeVariables: {
        darkMode: true,
        background: "#101827",
        primaryColor: "#1e304e",
        primaryTextColor: "#f8fafc",
        primaryBorderColor: "#7da8ee",
        lineColor: "#94a3b8",
        secondaryColor: "#173b46",
        secondaryTextColor: "#ecfeff",
        secondaryBorderColor: "#5bc9bd",
        tertiaryColor: "#302a49",
        tertiaryTextColor: "#f5f3ff",
        tertiaryBorderColor: "#a998df",
        clusterBkg: "#151f32",
        clusterBorder: "#44526b",
        edgeLabelBackground: "#101827",
        textColor: "#e2e8f0",
        actorTextColor: "#f8fafc",
        signalColor: "#b6c6dd",
        signalTextColor: "#e2e8f0",
        noteBkgColor: "#302a49",
        noteTextColor: "#f5f3ff",
        noteBorderColor: "#a998df",
        fontFamily: "ui-sans-serif, system-ui, sans-serif",
        // Keep Mermaid text readable at the app's default 125% UI scale.
        fontSize: "20px",
      },
    });
    return api;
  });
  return mermaidApiPromise;
}

type CachedDiagram = { svg: string; model: DiagramModel; origId: string };
const MAX_DIAGRAM_CACHE = 64;
const diagramCache = new Map<string, CachedDiagram>();

function getCachedDiagram(source: string, newId: string): { svg: string; model: DiagramModel } | null {
  const key = source.trim();
  const cached = diagramCache.get(key);
  if (!cached) return null;
  diagramCache.delete(key);
  diagramCache.set(key, cached);
  const namespacedSvg = cached.svg.replaceAll(cached.origId, newId);
  return { svg: namespacedSvg, model: cached.model };
}

function setCachedDiagram(source: string, origId: string, svg: string, model: DiagramModel): void {
  const key = source.trim();
  if (diagramCache.size >= MAX_DIAGRAM_CACHE) {
    const oldestKey = diagramCache.keys().next().value;
    if (oldestKey !== undefined) diagramCache.delete(oldestKey);
  }
  diagramCache.set(key, { svg, model, origId });
}

// Mermaid's parser databases are shared. Keep model extraction beside its
// render in the same queue even when several messages mount simultaneously.
let mermaidRenderQueue: Promise<void> = Promise.resolve();
function renderMermaid(api: MermaidApi, id: string, source: string) {
  const hit = getCachedDiagram(source, id);
  if (hit) return Promise.resolve(hit);

  const job = mermaidRenderQueue.then(async () => {
    const doubleCheck = getCachedDiagram(source, id);
    if (doubleCheck) return doubleCheck;

    const rendered = await api.render(id, source);
    const model: DiagramModel = { details: diagramAnnotations(source), edges: [] };
    try {
      const parser = api.mermaidAPI ?? api;
      const diagram = await parser.getDiagramFromText?.(source);
      if (diagram) model.edges = diagramEdges(diagram.db);
    } catch { /* Optional inspection data must not prevent diagram rendering. */ }
    setCachedDiagram(source, id, rendered.svg, model);
    return { ...rendered, model };
  });
  mermaidRenderQueue = job.then(() => undefined, () => undefined);
  return job;
}

let mermaidId = 0;
const mermaidOverlayCleanup = new WeakMap<HTMLElement, () => void>();
const mermaidModels = new WeakMap<HTMLElement, DiagramModel>();
const mermaidBounds = new WeakMap<SVGSVGElement, string>();
const inlineViewers = new Map<HTMLElement, () => void>();
let inlineCleanupObserver: MutationObserver | null = null;

/** Activate touch controls on release, including browsers that swallow the
 * synthetic click after SVG panning. Keep mouse and keyboard click semantics. */
function bindDiagramControl(button: HTMLElement, action: () => void, signal?: AbortSignal): void {
  let lastTouch = -Infinity;
  button.addEventListener("touchend", (event) => {
    if (event.touches.length || event.changedTouches.length !== 1) return;
    event.preventDefault();
    const touch = event.changedTouches[0];
    const rect = button.getBoundingClientRect();
    if (touch.clientX < rect.left || touch.clientX > rect.right || touch.clientY < rect.top || touch.clientY > rect.bottom) return;
    lastTouch = performance.now();
    button.focus({ preventScroll: true });
    action();
  }, { passive: false, signal });
  button.addEventListener("pointerdown", (event) => {
    if (event.pointerType === "mouse") lastTouch = -Infinity;
  }, { signal });
  button.addEventListener("click", (event) => {
    const pointerType = (event as MouseEvent & { pointerType?: string }).pointerType;
    const touchClick = pointerType ? pointerType === "touch" : event.detail > 0;
    if (touchClick && performance.now() - lastTouch < 1000) return;
    action();
  }, { signal });
}

function trackInlineViewer(block: HTMLElement, cleanup: () => void): void {
  inlineViewers.set(block, cleanup);
  inlineCleanupObserver ??= new MutationObserver(() => {
    for (const [element, dispose] of inlineViewers) {
      if (element.isConnected) continue;
      dispose();
      inlineViewers.delete(element);
    }
    if (!inlineViewers.size) { inlineCleanupObserver?.disconnect(); inlineCleanupObserver = null; }
  });
  inlineCleanupObserver.observe(document.body, { childList: true, subtree: true });
}

function attachInlineViewer(block: HTMLElement, svg: SVGSVGElement): void {
  const viewport = block.querySelector<HTMLElement>(".mermaid-diagram")!;
  const zoomLabel = block.querySelector("output")!;
  const qualityHost = block.querySelector<HTMLElement>(".mermaid-quality")!;
  const check = () => showDiagramQuality(qualityHost, checkDiagramQuality(svg));
  check();

  mermaidBounds.set(svg, svg.getAttribute("viewBox") || "0 0 800 600");

  const updateAdaptiveHeight = () => {
    const raw = mermaidBounds.get(svg) || svg.getAttribute("viewBox");
    const vals = raw ? raw.trim().split(/[\s,]+/).map(Number) : null;
    if (vals && vals.length === 4 && vals[2] > 0 && vals[3] > 0) {
      const containerWidth = viewport.clientWidth || 600;
      const idealHeight = Math.round((containerWidth / vals[2]) * vals[3] + 24);
      const clampedHeight = Math.min(460, Math.max(180, idealHeight));
      const current = parseInt(viewport.style.getPropertyValue("--mermaid-height"), 10);
      if (!Number.isNaN(current) && Math.abs(current - clampedHeight) <= 2) return;
      viewport.style.setProperty("--mermaid-height", `${clampedHeight}px`);
    }
  };

  updateAdaptiveHeight();
  const abort = new AbortController();
  const pan = block.querySelector<HTMLButtonElement>('[data-inline-action="pan"]')!;
  const camera = diagramViewport(svg, viewport, (zoom) => { zoomLabel.textContent = `${Math.round(zoom * 100)}%`; }, {
    wheelRequiresModifier: true,
    touchEnabled: () => pan.getAttribute("aria-pressed") === "true",
    onResize: () => {
      updateAdaptiveHeight();
    },
  });
  const openMenuAt = (pos: { clientX: number; clientY: number }) => {
    showMermaidContextMenu(pos, block, svg, camera, () => openMermaidOverlay(block));
  };

  viewport.addEventListener("contextmenu", (event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    openMenuAt({ clientX: event.clientX, clientY: event.clientY });
  }, { signal: abort.signal });

  block.querySelectorAll<HTMLButtonElement>("[data-inline-action]").forEach((button) => {
    button.disabled = false;
    bindDiagramControl(button, () => {
      switch (button.dataset.inlineAction) {
        case "in": camera.zoomBy(1.25); break;
        case "out": camera.zoomBy(0.8); break;
        case "fit": camera.fit(); break;
        case "pan": {
          const enabled = pan.getAttribute("aria-pressed") !== "true";
          pan.setAttribute("aria-pressed", String(enabled));
          viewport.classList.toggle("touch-panning", enabled);
          break;
        }
        case "menu": {
          const rect = button.getBoundingClientRect();
          openMenuAt({ clientX: rect.left - 140, clientY: rect.bottom + 6 });
          break;
        }
      }
    }, abort.signal);
  });
  const stopInspector = diagramInspector(svg, block.querySelector<HTMLElement>(".mermaid-inspector")!, mermaidModels.get(block)!, svg.id);
  trackInlineViewer(block, () => { camera.destroy(); stopInspector(); abort.abort(); });
}

let activeContextMenu: HTMLElement | null = null;
let activeContextCleanup: (() => void) | null = null;

function closeContextMenu(): void {
  activeContextCleanup?.();
  activeContextCleanup = null;
  activeContextMenu?.remove();
  activeContextMenu = null;
}

function showDiagramToast(block: HTMLElement, text: string): void {
  const existing = block.querySelector(".mermaid-toast");
  if (existing) existing.remove();
  const toast = document.createElement("div");
  toast.className = "mermaid-toast";
  toast.textContent = text;
  block.append(toast);
  setTimeout(() => {
    toast.classList.add("is-leaving");
    setTimeout(() => toast.remove(), 250);
  }, 1800);
}

function showSourceModal(source: string): void {
  const existing = document.querySelector(".mermaid-source-modal");
  if (existing) existing.remove();
  const modal = document.createElement("div");
  modal.className = "mermaid-source-modal";
  modal.setAttribute("role", "dialog");
  modal.setAttribute("aria-modal", "true");
  modal.innerHTML = `
    <div class="mermaid-source-dialog">
      <div class="mermaid-source-dialog-header">
        <span>Mermaid Source</span>
        <div class="mermaid-source-dialog-actions">
          <button type="button" class="mermaid-copy-btn">Copy</button>
          <button type="button" class="mermaid-close-btn" aria-label="Close source">✕</button>
        </div>
      </div>
      <pre class="mermaid-source-dialog-pre"><code>${escapeHtml(source)}</code></pre>
    </div>
  `;
  const copyBtn = modal.querySelector<HTMLButtonElement>(".mermaid-copy-btn")!;
  copyBtn.addEventListener("click", () => {
    void navigator.clipboard.writeText(source);
    copyBtn.textContent = "Copied!";
    setTimeout(() => { copyBtn.textContent = "Copy"; }, 1500);
  });
  const close = () => {
    document.removeEventListener("keydown", keyHandler);
    modal.remove();
  };
  const keyHandler = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };
  document.addEventListener("keydown", keyHandler);
  modal.querySelector(".mermaid-close-btn")?.addEventListener("click", close);
  modal.addEventListener("click", (e) => { if (e.target === modal) close(); });
  document.body.append(modal);
}

function showNotesModal(notes: string[]): void {
  const existing = document.querySelector(".mermaid-source-modal");
  if (existing) existing.remove();
  const modal = document.createElement("div");
  modal.className = "mermaid-source-modal";
  modal.setAttribute("role", "dialog");
  modal.setAttribute("aria-modal", "true");
  modal.innerHTML = `
    <div class="mermaid-source-dialog">
      <div class="mermaid-source-dialog-header">
        <span>Layout Notes</span>
        <button type="button" class="mermaid-close-btn" aria-label="Close notes">✕</button>
      </div>
      <ul class="mermaid-notes-list">
        ${notes.map((n) => `<li>${escapeHtml(n)}</li>`).join("")}
      </ul>
    </div>
  `;
  const close = () => {
    document.removeEventListener("keydown", keyHandler);
    modal.remove();
  };
  const keyHandler = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
  };
  document.addEventListener("keydown", keyHandler);
  modal.querySelector(".mermaid-close-btn")?.addEventListener("click", close);
  modal.addEventListener("click", (e) => { if (e.target === modal) close(); });
  document.body.append(modal);
}

function showMermaidContextMenu(
  pos: { clientX: number; clientY: number },
  block: HTMLElement,
  svg: SVGSVGElement,
  camera: { fit: () => void; zoomBy: (factor: number) => void },
  onExpand?: () => void,
): void {
  closeContextMenu();

  const source = block.querySelector<HTMLElement>(".mermaid-source")?.textContent?.trim() || block.dataset.mermaidSource || "";
  const qualityEl = block.querySelector<HTMLElement>(".mermaid-quality");
  const qualityNotes = qualityEl ? Array.from(qualityEl.querySelectorAll("li")).map((li) => li.textContent || "") : [];

  const menu = document.createElement("div");
  menu.className = "mermaid-context-menu";
  menu.setAttribute("role", "menu");

  menu.innerHTML = `
    <button type="button" class="mermaid-menu-item" data-action="fit">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/></svg>
      <span>Fit to View</span>
      <kbd>0</kbd>
    </button>
    <button type="button" class="mermaid-menu-item" data-action="in">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/><line x1="11" y1="8" x2="11" y2="14"/><line x1="8" y1="11" x2="14" y2="11"/></svg>
      <span>Zoom In</span>
      <kbd>+</kbd>
    </button>
    <button type="button" class="mermaid-menu-item" data-action="out">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/><line x1="8" y1="11" x2="14" y2="11"/></svg>
      <span>Zoom Out</span>
      <kbd>−</kbd>
    </button>
    ${onExpand ? `
    <button type="button" class="mermaid-menu-item" data-action="expand">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 3 21 3 21 9"/><polyline points="9 21 3 21 3 15"/><line x1="21" y1="3" x2="14" y2="10"/><line x1="3" y1="21" x2="10" y2="14"/></svg>
      <span>Fullscreen Explore</span>
    </button>` : ""}
    <div class="mermaid-menu-separator"></div>
    <button type="button" class="mermaid-menu-item" data-action="copy-source">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
      <span>Copy Mermaid Source</span>
    </button>
    <button type="button" class="mermaid-menu-item" data-action="view-source">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>
      <span>View Source</span>
    </button>
    <button type="button" class="mermaid-menu-item" data-action="copy-svg">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
      <span>Copy SVG</span>
    </button>
    <button type="button" class="mermaid-menu-item" data-action="download-svg">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>
      <span>Download SVG</span>
    </button>
    ${qualityNotes.length ? `
    <div class="mermaid-menu-separator"></div>
    <button type="button" class="mermaid-menu-item" data-action="notes">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>
      <span>Layout Notes (${qualityNotes.length})</span>
    </button>` : ""}
  `;

  const menuWidth = 190;
  const menuHeight = 260;
  const x = Math.min(pos.clientX, window.innerWidth - menuWidth - 8);
  const y = Math.min(pos.clientY, window.innerHeight - menuHeight - 8);
  menu.style.left = `${Math.max(8, x)}px`;
  menu.style.top = `${Math.max(8, y)}px`;

  document.body.append(menu);
  activeContextMenu = menu;

  const onPointerDown = (e: PointerEvent) => {
    if (!menu.contains(e.target as Node)) {
      closeContextMenu();
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      closeContextMenu();
    }
  };

  activeContextCleanup = () => {
    window.removeEventListener("pointerdown", onPointerDown, true);
    window.removeEventListener("keydown", onKeyDown, true);
  };

  setTimeout(() => {
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
  }, 10);

  menu.querySelectorAll<HTMLButtonElement>("[data-action]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const action = btn.dataset.action;
      closeContextMenu();
      switch (action) {
        case "fit": camera.fit(); break;
        case "in": camera.zoomBy(1.25); break;
        case "out": camera.zoomBy(0.8); break;
        case "expand": onExpand?.(); break;
        case "copy-source":
          if (source) {
            void navigator.clipboard.writeText(source);
            showDiagramToast(block, "Mermaid source copied");
          }
          break;
        case "view-source":
          if (source) showSourceModal(source);
          break;
        case "copy-svg": {
          const exported = svg.cloneNode(true) as SVGSVGElement;
          exported.setAttribute("viewBox", mermaidBounds.get(svg) || svg.getAttribute("viewBox")!);
          exported.querySelectorAll(".mermaid-node-selected, .mermaid-edge-selected").forEach((node) => node.classList.remove("mermaid-node-selected", "mermaid-edge-selected"));
          const xml = new XMLSerializer().serializeToString(exported);
          void navigator.clipboard.writeText(xml);
          showDiagramToast(block, "SVG copied to clipboard");
          break;
        }
        case "download-svg": {
          const exported = svg.cloneNode(true) as SVGSVGElement;
          exported.setAttribute("viewBox", mermaidBounds.get(svg) || svg.getAttribute("viewBox")!);
          exported.querySelectorAll(".mermaid-node-selected, .mermaid-edge-selected").forEach((node) => node.classList.remove("mermaid-node-selected", "mermaid-edge-selected"));
          const xml = new XMLSerializer().serializeToString(exported);
          const blob = new Blob([xml], { type: "image/svg+xml;charset=utf-8" });
          const url = URL.createObjectURL(blob);
          const a = document.createElement("a");
          a.href = url;
          a.download = `${svg.id || "diagram"}.svg`;
          a.click();
          URL.revokeObjectURL(url);
          break;
        }
        case "notes":
          if (qualityNotes.length) showNotesModal(qualityNotes);
          break;
      }
    });
  });
}


function closeMermaidOverlay(overlay: HTMLElement): void {
  mermaidOverlayCleanup.get(overlay)?.();
  mermaidOverlayCleanup.delete(overlay);
  overlay.remove();
}

/** Give the inline diagram a sensible initial footprint without cropping it. */
function prepareMermaidSvg(svg: SVGSVGElement): void {
  svg.setAttribute("preserveAspectRatio", "xMidYMid meet");
  // Mermaid can emit a fixed width/max-width style. Remove those inline
  // constraints so the viewport can fit both wide and tall diagrams.
  svg.style.removeProperty("width");
  svg.style.removeProperty("height");
  svg.style.removeProperty("max-width");

  // Normalize the SVG viewBox to true visual content bounds so inflated
  // measuring viewBoxes from Mermaid don't shrink the diagram into a tiny box at the top.
  const bounds = getSvgContentBounds(svg);
  if (bounds) {
    svg.setAttribute("viewBox", bounds.join(" "));
  }
}

function openMermaidOverlay(block: HTMLElement): void {
  const existing = document.querySelector<HTMLElement>(".mermaid-overlay");
  if (existing) closeMermaidOverlay(existing);
  const sourceSvg = block.querySelector<SVGSVGElement>(".mermaid-diagram svg");
  if (!sourceSvg) return;
  const previousFocus = document.activeElement as HTMLElement | null;
  const previousOverflow = document.body.style.overflow;
  const overlay = document.createElement("div");
  overlay.className = "mermaid-overlay";
  overlay.setAttribute("role", "dialog");
  overlay.setAttribute("aria-modal", "true");
  overlay.setAttribute("aria-label", "Interactive diagram");
  overlay.innerHTML = `<div class="mermaid-overlay-panel">
    <div class="mermaid-overlay-header">
      <div><span class="mermaid-overlay-kicker">Explore diagram</span><span class="mermaid-overlay-hint">Drag to pan · Scroll or pinch to zoom · Double-click to fit</span></div>
      <div class="mermaid-zoom-controls">
        <button type="button" data-action="out" aria-label="Zoom out">−</button>
        <output class="mermaid-zoom-level" aria-label="Zoom level">100%</output>
        <button type="button" data-action="in" aria-label="Zoom in">+</button>
        <button type="button" data-action="fit" title="Fit entire diagram (0)">Fit</button>
        <button type="button" data-action="source" title="View Mermaid source">Source</button>
        <button type="button" data-action="download" title="Download complete diagram as SVG">SVG ↓</button>
      </div>
      <button type="button" class="mermaid-overlay-close" aria-label="Close diagram">×</button>
    </div>
    <div class="mermaid-overlay-diagram" tabindex="0" role="group" aria-label="Diagram canvas. Use arrow keys to pan, plus and minus to zoom, and zero to fit. Right-click for options."></div>
    <div class="mermaid-inspector" aria-live="polite">Select a node to highlight its connections.</div>
  </div>`;
  const diagram = overlay.querySelector<HTMLElement>(".mermaid-overlay-diagram")!;
  const inspector = overlay.querySelector<HTMLElement>(".mermaid-inspector")!;
  const svg = sourceSvg.cloneNode(true) as SVGSVGElement;
  svg.setAttribute("viewBox", mermaidBounds.get(sourceSvg) || sourceSvg.getAttribute("viewBox")!);
  svg.querySelectorAll(".mermaid-node-selected, .mermaid-edge-selected").forEach((node) => node.classList.remove("mermaid-node-selected", "mermaid-edge-selected"));
  // Mermaid styles and markers reference IDs. Namespace the clone so its
  // definitions never resolve to an SVG hidden behind the dialog.
  const ids = new Map<string, string>();
  svg.querySelectorAll("[id]").forEach((element) => {
    ids.set(element.id, `${element.id}-explore`);
    element.id += "-explore";
  });
  for (const element of [svg, ...svg.querySelectorAll("*")]) {
    for (const attribute of [...element.attributes]) {
      let value = attribute.value;
      value = value.replace(/url\(#([^)]*)\)/g, (match, id: string) => ids.has(id) ? `url(#${ids.get(id)})` : match);
      if (attribute.name === "aria-labelledby" || attribute.name === "aria-describedby") value = value.split(" ").map((id) => ids.get(id) || id).join(" ");
      if ((attribute.name === "href" || attribute.name === "xlink:href") && value.startsWith("#")) value = `#${ids.get(value.slice(1)) || value.slice(1)}`;
      element.setAttribute(attribute.name, value);
    }
  }
  // The generated stylesheet is scoped to the root SVG ID.
  const oldRootId = svg.id;
  svg.id = `${svg.id}-explore`;
  svg.querySelectorAll("style").forEach((style) => { style.textContent = (style.textContent || "").replaceAll(`#${oldRootId}`, `#${svg.id}`); });
  diagram.append(svg);
  document.body.append(overlay);
  document.body.style.overflow = "hidden";
  const zoomLabel = overlay.querySelector("output")!;
  const camera = diagramViewport(svg, diagram, (zoom) => { zoomLabel.textContent = `${Math.round(zoom * 100)}%`; });
  diagram.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    event.stopPropagation();
    showMermaidContextMenu({ clientX: event.clientX, clientY: event.clientY }, block, svg, camera);
  });
  const action = (name: string, callback: () => void) => bindDiagramControl(overlay.querySelector<HTMLElement>(`[data-action="${name}"]`)!, callback);
  action("in", () => camera.zoomBy(1.25));
  action("out", () => camera.zoomBy(0.8));
  action("fit", camera.fit);
  action("source", () => {
    const src = block.querySelector(".mermaid-source")?.textContent || block.dataset.mermaidSource || "";
    if (src) showSourceModal(src);
  });
  action("download", () => {
    // Export the original bounds, irrespective of the current camera position.
    const exported = sourceSvg.cloneNode(true) as SVGSVGElement;
    exported.setAttribute("viewBox", mermaidBounds.get(sourceSvg) || sourceSvg.getAttribute("viewBox")!);
    exported.querySelectorAll(".mermaid-node-selected, .mermaid-edge-selected").forEach((node) => node.classList.remove("mermaid-node-selected", "mermaid-edge-selected"));
    exported.querySelectorAll('[role="button"]').forEach((node) => {
      node.removeAttribute("role");
      node.removeAttribute("tabindex");
      node.removeAttribute("aria-pressed");
      node.removeAttribute("aria-label");
    });
    exported.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    const bounds = exported.getAttribute("viewBox")?.split(/[\s,]+/).map(Number);
    if (bounds?.length === 4) {
      exported.setAttribute("width", String(bounds[2]));
      exported.setAttribute("height", String(bounds[3]));
      const background = document.createElementNS("http://www.w3.org/2000/svg", "rect");
      background.setAttribute("x", String(bounds[0]));
      background.setAttribute("y", String(bounds[1]));
      background.setAttribute("width", String(bounds[2]));
      background.setAttribute("height", String(bounds[3]));
      background.setAttribute("fill", "#101827");
      exported.prepend(background);
    }
    const url = URL.createObjectURL(new Blob([new XMLSerializer().serializeToString(exported)], { type: "image/svg+xml" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = "kageha-diagram.svg";
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  const stopInspector = diagramInspector(svg, inspector, mermaidModels.get(block) || { details: new Map(), edges: [] }, sourceSvg.id);
  const escapeHandler = (event: KeyboardEvent) => {
    if (event.key === "Escape") { closeMermaidOverlay(overlay); return; }
    if (event.key === "Tab") {
      const focusable = [...overlay.querySelectorAll<HTMLElement>('button, [tabindex="0"], summary')];
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      return;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === "+" || event.key === "=") camera.zoomBy(1.25);
    else if (event.key === "-") camera.zoomBy(0.8);
    else if (event.key === "0") camera.fit();
    else if (event.key === "ArrowLeft") camera.pan(48, 0);
    else if (event.key === "ArrowRight") camera.pan(-48, 0);
    else if (event.key === "ArrowUp") camera.pan(0, 48);
    else if (event.key === "ArrowDown") camera.pan(0, -48);
    else return;
    event.preventDefault();
  };
  document.addEventListener("keydown", escapeHandler);
  mermaidOverlayCleanup.set(overlay, () => {
    camera.destroy();
    stopInspector();
    document.removeEventListener("keydown", escapeHandler);
    document.body.style.overflow = previousOverflow;
    previousFocus?.focus();
  });
  bindDiagramControl(overlay.querySelector<HTMLElement>(".mermaid-overlay-close")!, () => closeMermaidOverlay(overlay));
  overlay.addEventListener("click", (event) => { if (event.target === overlay) closeMermaidOverlay(overlay); });
  diagram.focus();
}

function attachMermaidExpand(block: HTMLElement): void {
  const button = block.querySelector<HTMLButtonElement>(".mermaid-expand");
  if (!button || button.dataset.bound) return;
  button.dataset.bound = "true";
  bindDiagramControl(button, () => openMermaidOverlay(block));
}

/** Render Mermaid placeholders inside sanitized markdown after mount. */
export function initMermaidBlocks(container: HTMLElement): void {
  const blocks = Array.from(container.querySelectorAll<HTMLElement>(".mermaid-block"))
    .filter((block) => !block.dataset.mermaidState);
  if (!blocks.length) return;
  blocks.forEach((block) => { block.dataset.mermaidState = "pending"; });
  blocks.forEach(attachMermaidExpand);

  void loadMermaid().then(async (api) => {
    for (const block of blocks) {
      const source = block.querySelector<HTMLElement>(".mermaid-source")?.textContent?.trim() || "";
      const mount = block.querySelector<HTMLElement>(".mermaid-diagram");
      const status = block.querySelector<HTMLElement>(".mermaid-status");
      if (!mount || !source) continue;
      try {
        if (!block.isConnected) continue;
        const { svg, model } = await renderMermaid(api, `kageha-mermaid-${mermaidId++}`, source);
        if (!block.isConnected) continue;
        mermaidModels.set(block, model);
        mount.innerHTML = svg;
        const renderedSvg = mount.querySelector<SVGSVGElement>("svg");
        if (renderedSvg) {
          prepareMermaidSvg(renderedSvg);
          attachInlineViewer(block, renderedSvg);
        }
        block.dataset.mermaidState = "rendered";
        status?.replaceChildren();
        if (status) status.textContent = "Pan · Zoom · Inspect";
        const expand = block.querySelector<HTMLButtonElement>(".mermaid-expand");
        if (expand) expand.disabled = false;
      } catch (error) {
        block.dataset.mermaidState = "error";
        mount.innerHTML = `<p class="mermaid-error">Couldn’t render this diagram. The Mermaid source is available below.</p>`;
        if (status) status.textContent = "Source fallback";
        console.warn("Mermaid diagram render failed", error);
      }
    }
  }).catch((error) => {
    blocks.forEach((block) => {
      block.dataset.mermaidState = "error";
      const status = block.querySelector<HTMLElement>(".mermaid-status");
      if (status) status.textContent = "Source fallback";
    });
    console.warn("Mermaid could not be loaded", error);
  });
}

/**
 * Enhance all code blocks in rendered HTML with:
 * - Language label badge
 * - Copy button
 * - Syntax highlighting
 * - Collapse for long blocks
 * - CSV → table conversion
 * - Mermaid → placeholder
 */
export function enhanceCodeBlocks(html: string): string {
  if (!html || !html.includes("<pre>")) return html;
  let blockIndex = 0;

  return html.replace(
    /<pre><code(?:\s+class="([^"]*)")?>([\s\S]*?)<\/code><\/pre>/gi,
    (_match, classAttr: string | undefined, content: string) => {
      // Skip if already enhanced by jsonRenderer
      if (_match.includes("json-data-card")) return _match;

      // Extract language from class="language-xxx"
      const langMatch = (classAttr || "").match(/language-(\w+)/);
      const lang = langMatch ? langMatch[1].toLowerCase() : "";
      const rawText = unescapeHtml(content);
      const id = `code-block-${blockIndex++}`;

      // CSV → render as table
      if (lang === "csv" && rawText.includes(",") && rawText.split("\n").length >= 2) {
        const table = csvToTable(rawText);
        if (table) return table;
      }

      // Mermaid → placeholder
      if (lang === "mermaid") {
        void loadMermaid();
        return mermaidPlaceholder(rawText);
      }

      // Apply syntax highlighting
      const highlighted = lang ? highlightCode(content, lang) : content;

      // Determine if collapsible (>20 lines)
      const lineCount = rawText.split("\n").length;
      const collapsible = lineCount > 20;

      // Language label
      const label = LANG_LABELS[lang] || (lang ? lang.toUpperCase() : "");

      // Build enhanced block
      let enhanced = `<div class="code-block-enhanced" data-code-id="${id}">`;
      enhanced += `<div class="code-block-header">`;
      if (label) {
        enhanced += `<span class="code-block-lang">${escapeHtml(label)}</span>`;
      }
      enhanced += `<button type="button" class="code-copy-btn" data-code-id="${id}" title="Copy code">`;
      enhanced += `<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="5" y="5" width="9" height="9" rx="1.5"/><path d="M5 11H3.5A1.5 1.5 0 012 9.5v-7A1.5 1.5 0 013.5 1h7A1.5 1.5 0 0112 2.5V5"/></svg>`;
      enhanced += ` Copy</button>`;
      enhanced += `</div>`;

      if (collapsible) {
        enhanced += `<details class="code-block-collapse"><summary class="code-block-expand">${lineCount} lines — click to expand</summary>`;
        enhanced += `<pre><code>${highlighted}</code></pre>`;
        enhanced += `</details>`;
      } else {
        enhanced += `<pre><code>${highlighted}</code></pre>`;
      }

      enhanced += `<textarea class="code-block-clipboard" style="position:absolute;left:-9999px" aria-hidden="true">${escapeHtml(rawText)}</textarea>`;
      enhanced += `</div>`;
      return enhanced;
    },
  );
}

/**
 * Initialize copy button handlers for enhanced code blocks.
 */
export function initCodeBlockHandlers(container: HTMLElement): void {
  container.querySelectorAll<HTMLButtonElement>(".code-copy-btn").forEach((btn) => {
    const clone = btn.cloneNode(true) as HTMLButtonElement;
    btn.parentNode?.replaceChild(clone, btn);

    clone.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      const codeId = clone.dataset.codeId;
      const block = container.querySelector(`[data-code-id="${codeId}"]`);
      if (!block) return;
      const textarea = block.querySelector<HTMLTextAreaElement>(".code-block-clipboard");
      if (textarea) {
        void navigator.clipboard.writeText(textarea.value).then(() => {
          const orig = clone.innerHTML;
          clone.classList.add("is-copied");
          clone.innerHTML = `<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" class="text-emerald-400 shrink-0"><path d="M3 8.5l3.5 3.5 6.5-7"/></svg> Copied!`;
          setTimeout(() => {
            clone.classList.remove("is-copied");
            clone.innerHTML = orig;
          }, 1800);
        });
      }
    });
  });
}
