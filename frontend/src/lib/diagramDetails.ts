export type DiagramNodeDetails = { description?: string; files: string[] };
export type DiagramEdge = { id?: string; start: string; end: string; label?: string };
export type DiagramModel = { details: Map<string, DiagramNodeDetails>; edges: DiagramEdge[] };

export function diagramAnnotations(source: string): Map<string, DiagramNodeDetails> {
  const details = new Map<string, DiagramNodeDetails>();
  for (const line of source.split("\n")) {
    const match = line.match(/^\s*%%\s*@node\s+([\w-]+)\s+(\{.*\})\s*$/);
    if (!match || match[2].length > 12000) continue;
    try {
      const value = JSON.parse(match[2]);
      if (!value || typeof value !== "object" || Array.isArray(value)) continue;
      details.set(match[1], {
        description: typeof value.description === "string" ? value.description.slice(0, 4000) : undefined,
        files: Array.isArray(value.files) ? value.files.filter((file: unknown): file is string => typeof file === "string").slice(0, 20).map((file: string) => file.slice(0, 1000)) : [],
      });
    } catch { /* Malformed optional annotations must not break a valid diagram. */ }
  }
  return details;
}

/** Adapt only the relationship data exposed by Mermaid's flowchart database. */
export function diagramEdges(db: unknown): DiagramEdge[] {
  if (!db || typeof db !== "object") return [];
  const data = db as { getEdges?: () => unknown };
  if (typeof data.getEdges !== "function") return [];
  const edges = data.getEdges();
  if (!Array.isArray(edges)) return [];
  return edges.flatMap((edge) => {
    if (typeof edge?.start !== "string" || typeof edge?.end !== "string") return [];
    const label = typeof edge.text === "string" ? edge.text.replace(/<br\s*\/?\s*>/gi, " ").replace(/<[^>]*>/g, "") : undefined;
    return [{ id: typeof edge.id === "string" ? edge.id : undefined, start: edge.start, end: edge.end, label }];
  });
}

function nodeKey(node: Element, rootId: string): string {
  const original = node.id.replace(/-explore$/, "");
  return (original.startsWith(`${rootId}-`) ? original.slice(rootId.length + 1) : original)
    .replace(/^flowchart-/, "").replace(/-\d+$/, "");
}

/** Shared inspector for inline and expanded diagrams; all authored text is inert. */
export function diagramInspector(svg: SVGSVGElement, inspector: HTMLElement, model: DiagramModel, rootId: string) {
  const abort = new AbortController();
  const nodes = [...svg.querySelectorAll<SVGElement>(".node, g.actor, .classGroup")];
  if (!nodes.length) inspector.textContent = "Use the controls to explore this diagram.";
  const keys = new Map(nodes.map((node) => [nodeKey(node, rootId), node]));
  const label = (key: string) => (keys.get(key)?.textContent || key).replace(/\s+/g, " ").trim();
  const select = (node: SVGElement) => {
    const key = nodeKey(node, rootId);
    const details = model.details.get(key);
    const incoming = model.edges.filter((edge) => edge.end === key);
    const outgoing = model.edges.filter((edge) => edge.start === key);
    svg.querySelectorAll(".mermaid-node-selected, .mermaid-edge-selected").forEach((item) => item.classList.remove("mermaid-node-selected", "mermaid-edge-selected"));
    nodes.forEach((item) => item.setAttribute("aria-pressed", String(item === node)));
    node.classList.add("mermaid-node-selected");
    const related = [...incoming, ...outgoing];
    svg.querySelectorAll(".flowchart-link").forEach((edge) => {
      const id = edge.getAttribute("data-id") || "";
      const connected = model.edges.length
        ? related.some((relation) => relation.id === id || id.startsWith(`L_${relation.start}_${relation.end}_`))
        : edge.classList.contains(`LS-${key}`) || edge.classList.contains(`LE-${key}`) || id.startsWith(`L_${key}_`) || id.replace(/_\d+$/, "").endsWith(`_${key}`);
      if (connected) edge.classList.add("mermaid-edge-selected");
    });
    inspector.replaceChildren();
    inspector.classList.add("has-selection");
    const heading = document.createElement("strong");
    heading.textContent = label(key);
    const clear = document.createElement("button");
    clear.type = "button";
    clear.className = "mermaid-inspector-clear";
    clear.textContent = "Clear";
    clear.setAttribute("aria-label", "Clear component selection");
    clear.addEventListener("click", () => {
      svg.querySelectorAll(".mermaid-node-selected, .mermaid-edge-selected").forEach((item) => item.classList.remove("mermaid-node-selected", "mermaid-edge-selected"));
      nodes.forEach((item) => item.setAttribute("aria-pressed", "false"));
      inspector.classList.remove("has-selection");
      inspector.textContent = "Select a component to see details and connections.";
      node.focus();
    });
    inspector.append(heading, clear);
    if (details?.description) {
      const description = document.createElement("p");
      description.textContent = details.description;
      inspector.append(description);
    }
    if (details?.files.length) {
      const files = document.createElement("div");
      files.className = "mermaid-component-files";
      const title = document.createElement("span");
      title.textContent = "Related files";
      files.append(title);
      details.files.forEach((path) => {
        const file = document.createElement("code");
        file.textContent = path;
        files.append(file);
      });
      inspector.append(files);
    }
    const connections = (title: string, edges: DiagramEdge[], incoming: boolean) => {
      if (!edges.length) return;
      const section = document.createElement("div");
      section.className = "mermaid-connections";
      const caption = document.createElement("span");
      caption.textContent = title;
      section.append(caption);
      edges.forEach((edge) => {
        const targetKey = incoming ? edge.start : edge.end;
        const target = keys.get(targetKey);
        const row = document.createElement(target ? "button" : "span");
        row.textContent = `${incoming ? "←" : "→"} ${label(targetKey)}${edge.label ? ` · ${edge.label}` : ""}`;
        if (row instanceof HTMLButtonElement && target) {
          row.type = "button";
          row.addEventListener("click", () => { select(target); inspector.querySelector<HTMLButtonElement>("button")?.focus(); });
        }
        section.append(row);
      });
      inspector.append(section);
    };
    connections("Incoming", incoming, true);
    connections("Outgoing", outgoing, false);
  };
  nodes.forEach((node) => {
    node.setAttribute("tabindex", "0");
    node.setAttribute("role", "button");
    node.setAttribute("aria-pressed", "false");
    node.setAttribute("aria-label", `Inspect ${label(nodeKey(node, rootId))}`);
    node.addEventListener("click", () => select(node), { signal: abort.signal });
    node.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") { event.preventDefault(); event.stopPropagation(); select(node); }
    }, { signal: abort.signal });
  });
  return () => abort.abort();
}
