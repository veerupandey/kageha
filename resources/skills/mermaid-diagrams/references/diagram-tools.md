# Diagram tools and artifact delivery

Read the section for the selected tool. These are optional generation routes, not a claim that every renderer is installed or embedded in Kageha. Only Mermaid fences render natively in chat; other formats need a rendered preview or an HTML/file artifact.

## Shared workflow

1. Use verified components and relationships. Label a proposed architecture as proposed. Do not infer deployed cloud services from the availability of their icons.
2. Check executable/package availability before choosing the render path. Prefer existing project dependencies or an isolated task environment. Follow the current environment's package-install permissions; do not change application dependencies just to render one diagram.
3. Write output in the session's artifact directory (use `KAGEHA_ARTIFACTS` when provided), under `diagrams/<descriptive-name>/`. Preserve editable source alongside final output. Avoid overwriting unrelated files.
4. Render locally and inspect the result. Check long labels, group boundaries, arrow direction, contrast, and the full bounds at the intended viewing size. For HTML, also check a narrow viewport and exercise the requested controls. A PNG/SVG from a CLI is a static export; do not call it interactive unless a working viewer is included.
5. Deliver accessible artifact links and a concise explanation. Include a preview where supported, the editable source, and the render command or dependency versions needed to reproduce it. If rendering fails, identify the limitation and provide a working alternative where possible.

## Python Diagrams: cloud and infrastructure icons

Use [mingrammer/diagrams](https://github.com/mingrammer/diagrams) for AWS, Azure, GCP, Kubernetes, and on-prem topology. It uses the Graphviz `dot` executable in addition to the Python `diagrams` package. Check both in the interpreter/environment that will run the source; a Python Graphviz package alone does not install `dot`.

Consult the [provider node catalog](https://diagrams.mingrammer.com/docs/nodes/aws) or inspect installed modules for exact import names. Choose real service icons and label them with their role. Use clusters for actual region, VPC, namespace, or ownership boundaries. Prefer one primary direction and three or four role colors; icons already provide visual variety. A light neutral canvas generally keeps provider logos legible.

Illustrative AWS topology (replace with the user's verified or proposed components). Run from the selected artifact directory, with `show=False` to avoid launching a desktop viewer:

```python
from diagrams import Cluster, Diagram, Edge
from diagrams.aws.compute import EC2
from diagrams.aws.database import RDS
from diagrams.aws.network import ELB

with Diagram(
    "Web service", filename="web-service", outformat="svg",
    show=False, direction="LR",
    graph_attr={"bgcolor": "#f8fafc", "pad": "0.5", "nodesep": "0.7", "ranksep": "1.0"},
    node_attr={"fontname": "sans-serif", "fontsize": "13"},
    edge_attr={"color": "#64748b"},
):
    ingress = ELB("HTTPS ingress")
    with Cluster("Application"):
        app = EC2("API service")
    database = RDS("Application data")
    ingress >> Edge(label="HTTPS") >> app
    app >> Edge(label="SQL") >> database
```

Deliver `web-service.svg` and the `.py` source; PNG is also useful for previews. If `diagrams` or `dot` cannot be made available under current permissions, produce a styled Mermaid topology and clearly state that provider icons were unavailable. For a requested interactive cloud diagram, wrap the finished SVG in a real pan/zoom HTML viewer with Fit and keyboard controls, or build an HTML/SVG diagram with equivalent interactions. Do not substitute a static image for requested controls.

## D2: polished architecture artifacts

Check `d2 --version`. Write a `.d2` source and render with `d2 architecture.d2 architecture.svg`. Use nested containers for ownership, short connection labels, and a consistent theme. Check `d2 --help` and available layout engines for the installed version before selecting optional flags; do not assume a specific engine or extension is bundled. Preserve the `.d2` source. Kageha does not natively render a `d2` code fence.

References: [repository](https://github.com/d2lang/d2), [language tour](https://d2lang.com/tour/intro/).

## Excalidraw and draw.io: editable diagrams

For a hand-drawn whiteboard, create a valid `.excalidraw` document using the installed Excalidraw schema/export utilities or an available diagram tool. Bind labels to their containers and arrows to their endpoints so editing preserves the relationships. Include a rendered SVG/PNG preview and visually verify labels and routing. Hand-authored JSON alone is not evidence that it renders. Use an available local editor/export path; do not invent an MCP integration or upload to a public sharing service without authorization.

For draw.io, produce valid `.drawio` XML with real cells, geometry, and edge source/target references; export a preview with an available editor/CLI. A screenshot alone does not fulfill a request for an editable diagram.

References: [Excalidraw](https://github.com/excalidraw/excalidraw), [export utilities](https://docs.excalidraw.com/docs/@excalidraw/excalidraw/api/utils/export), [draw.io](https://github.com/jgraph/drawio).

## HTML/SVG and React Flow: richer interaction

For a standalone visual explanation, create an HTML artifact with scoped CSS, readable SVG or DOM labels, and working controls. Prefer self-contained output. Provide pan/zoom and Fit for navigation; add filtering, node details, or step controls only when they help answer the question. Make pointer, touch, and keyboard navigation work, and respect reduced-motion preferences. Never hide essential relationships behind hover alone. Inspect the actual browser output and test the controls before delivery.

For an existing React app with draggable cards and custom handles, use [React Flow](https://reactflow.dev/). It provides diagram interaction; choose or implement a layout separately rather than expecting it to auto-arrange a graph. Prefer the project's established design system and dependencies.

The [Visual Explainer skill](https://github.com/nicobailon/visual-explainer) is a useful optional workflow for styled HTML explanations. Invoke it only if available; this reference does not install it. Similarly, Beautiful Mermaid is an optional renderer to evaluate for Mermaid syntax, not a new diagram language and not a claim that Kageha uses it.

## Architecture models and mathematical diagrams

Use [LikeC4](https://likec4.dev/) when multiple views should derive from one architecture model. Keep elements and relationships in the model and generate focused views; avoid independently drawing contradictory versions. Preserve existing Structurizr conventions if the repository uses them. Check the installed CLI and preview/export workflow before generating artifacts.

Use [Penrose](https://github.com/penrose/penrose) for mathematical relationships when its toolchain is available and the constraint model benefits the task. For a small geometric figure, precise SVG may be simpler. Label quantities and verify geometry rather than relying on an illustrative raster image for mathematical accuracy.
