---
name: deep-explain
description: Evidence-backed codebase archaeology and architecture explanations. Map unfamiliar repositories, trace execution and dependencies, explain symbols and data flow, identify change impact, and render compact Mermaid diagrams without inventing relationships.
triggers:
  - explain this codebase
  - understand this repository
  - how does this work
  - explain the architecture
  - explain the code
  - trace the flow
  - trace execution
  - what calls this
  - where is this used
  - find dependents
  - blast radius
  - impact analysis
  - explain function
  - explain class
  - walk me through the repo
  - map the codebase
paths:
  - "**/*.py"
  - "**/*.ts"
  - "**/*.tsx"
  - "**/*.js"
  - "**/*.jsx"
  - "**/*.go"
  - "**/*.rs"
  - "**/*.java"
  - "**/*.rb"
  - "**/*.yaml"
  - "**/*.yml"
  - "**/pyproject.toml"
  - "**/package.json"
  - "**/Cargo.toml"
  - "**/go.mod"
---

# deep-explain

Turn a vague “how does this work?” into a defensible, navigable explanation of the repository. Work from the smallest useful evidence set, then expand only where the question requires it.

## Operating rules

- **Inspect before explaining.** Establish the repository root, relevant entry points, and language/tooling before making architectural claims.
- **Evidence over intuition.** Every important claim should name a file and symbol, preferably with a line link such as `src/app.py:42`. Label claims as `observed`, `inferred`, or `unknown` when the distinction matters.
- **Trace, do not dump.** Follow the requested path through callers, callees, state, I/O, and error boundaries. Do not paste whole files or produce a file-by-file inventory unless asked.
- **Separate static from runtime truth.** Imports and call sites show possible paths; tests, logs, configuration, and executed code show exercised paths. Never present a static possibility as a runtime fact.
- **Stay read-only by default.** Analysis may inspect files, history, tests, and safe metadata. Do not edit source, create reports, install packages, run migrations, or invoke mutating commands unless the user asks.
- **Respect boundaries.** Exclude generated output, vendored dependencies, caches, secrets, `.env*`, credentials, and build artifacts from the map unless the user explicitly asks about them.
- **Handle uncertainty explicitly.** If dynamic dispatch, reflection, dependency injection, generated code, or missing configuration prevents certainty, state what was proven and what would confirm it.

## Investigation protocol

### 1. Frame the question

Identify the requested scope and deliverable:

- **Repository map:** topology, entry points, layers, external boundaries, and primary data flows.
- **Symbol explanation:** definition, callers, callees, state changes, side effects, and failure behavior.
- **Execution trace:** trigger → dispatch → transformations → I/O → response, including alternate/error paths.
- **Impact analysis:** direct and transitive dependents, contracts, tests, configuration, and likely regression surfaces.
- **Comparison:** two implementations, providers, routes, or versions with behavior and ownership differences.

If the request is broad, start with a compact map and offer deeper levels rather than reading the entire repository indiscriminately.

### 2. Build a structural index

Use fast, bounded inspection first:

1. List the root and likely source/test/config directories with `rg --files -g ...`.
2. Read the manifest and entry-point candidates (`pyproject.toml`, `package.json`, `Makefile`, `src/main.*`, CLI/server modules).
3. Search definitions and references with `rg`, including the exact symbol and likely aliases.
4. Read only the relevant surrounding regions; widen the window when control flow or contracts cross files.
5. Inspect tests, configuration, adapters, and persistence boundaries for the selected path.

For each node, record: `path`, `symbol`, `kind`, `responsibility`, `inputs`, `outputs`, `side effects`, and `evidence`. For each edge, record: `from`, `to`, `relationship` (`imports`, `calls`, `extends`, `publishes`, `reads`, `writes`, `configures`), and confidence.

Use language-aware tools when already available (for example TypeScript compiler metadata, Python AST tooling, `go list`, or `cargo metadata`), but do not require a new dependency merely to answer an explanation.

### 3. Trace the behavior

Follow the narrowest path that answers the question:

```text
trigger / caller
  → entry point or route
  → validation and normalization
  → orchestration / dispatch
  → domain operation
  → persistence, network, process, or model boundary
  → response, event, or rendered state
```

At every hop, note:

- the symbol and file;
- the values or contract crossing the boundary;
- synchronous vs asynchronous behavior;
- retries, fallbacks, cancellation, and error translation;
- user-visible state or durable side effects.

Check both forward and reverse edges. A function’s implementation alone is insufficient; callers reveal assumptions and tests reveal supported behavior.

### 4. Analyze impact when requested

Start with direct references, then classify transitive dependents:

| Layer | Questions |
| --- | --- |
| API/UI | Which routes, commands, components, or serialized fields change? |
| orchestration | Which controllers, workers, queues, or event handlers depend on it? |
| contracts | Which types, schemas, environment variables, provider protocols, or persisted data are coupled? |
| operations | Which logs, metrics, permissions, retries, deployment settings, or external services are affected? |
| verification | Which focused and end-to-end tests exercise the changed behavior? |

Rank findings as **must update**, **should verify**, or **probably unaffected**, and explain why. Call out dynamic or reflection-based edges as a separate uncertainty section.

### 5. Verify and render

Use the strongest inexpensive verification available: focused tests, type checks, config validation, static reference searches, and—only when safe and relevant—a read-only smoke path. Report commands actually run and their result; do not claim runtime behavior from source inspection alone.

Use one small Mermaid diagram when it materially clarifies the relationship:

- `flowchart LR` for request/data flow;
- `graph TD` for ownership/dependencies;
- `sequenceDiagram` for time-ordered interactions;
- `classDiagram` only when inheritance/interfaces are central.

Keep diagrams to the relevant 5–12 nodes. Label edges with the real relationship and add file/symbol anchors in nearby prose, not huge code blocks inside the diagram.

## Response format

Adapt depth to the question, but prefer this order:

1. **Answer first:** one or two sentences stating the main mechanism.
2. **Map:** compact diagram or layered bullets with file/symbol anchors.
3. **Trace:** numbered execution path, including important branches.
4. **Contracts and side effects:** inputs, outputs, state, I/O, auth, retries, and errors.
5. **Evidence and uncertainty:** observed facts, inferences, missing runtime confirmation, and the next most useful inspection.
6. **Impact checklist:** only for change-impact questions.

For a repository overview, use three levels:

- **L1 — blueprint:** purpose, entry points, major subsystems, and boundaries.
- **L2 — component flow:** selected modules, contracts, dependencies, and runtime path.
- **L3 — deep dive:** line-level logic, state transitions, edge cases, and tests.

Stop at the requested level. Ask before expanding into unrelated subsystems.

## Common failure modes

- Do not equate directory structure with architecture; confirm ownership through imports, calls, and runtime entry points.
- Do not confuse a model/provider listed in configuration with one that is ready, selected, or actually exercised.
- Do not call every import a runtime dependency when lazy imports, optional extras, or plugin registration are involved.
- Do not infer a caller from a same-named symbol; verify the import, receiver type, route, or test.
- Do not hide a broken or unverified path behind polished prose. Put it in **Uncertainty** with the exact missing evidence.
- Do not create `architecture.md`, indexes, caches, or generated graphs unless the user explicitly asks for an artifact.
