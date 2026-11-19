---
name: web_research
description: Blink-speed research via research_run (parallel search+fetch), optional headless enrich, then a sourced answer (chat by default; brief.md only when asked).
triggers:
  - research
  - look up
  - find sources
  - cite sources
  - with citations
  - investigate
  - news about
  - background on
  - research_run
allowed-tools: research_run parallel_web_fetch headless_fetch web_search parallel_web_search web_fetch browser_connect browser_open browser_snapshot browser_click browser_fill browser_screenshot browser_close
---

# web_research

Chat-first output, no unsolicited brief files, and inline `[n]` citations with a
`## Sources` list are already in the always-loaded policy. Each research tool
documents its own behaviour. This skill covers the depth ladder, routing, and
the pitfalls learned from real runs.

## Depth ladder

`research_run(query=…, depth=…)` — one call does parallel search + extract.
`queries_json` extra angles; first item is used if `query` is omitted:

- `flash` (default) — parallel search + HTTP extract, no Chromium. Fastest.
- `standard` — flash plus warm headless JS for thin or SPA pages.
- `deep` — standard, then hand off to `browser_*` for login or interaction.

Add `parallel_web_fetch` / `headless_fetch` for extra known URLs. Escalate to
the `web_browse` skill only when a source needs clicks or a login wall —
`browser_open(url)` then act on the snapshot refs. Stay on the private
browser. Call `browser_connect` only for named comet/chrome/cdp.
`browser_close` when finished.

## Fan-out

For multi-angle topics, spawn one subagent per angle, each calling
`research_run`, then merge into a single answer. Do not serialise angles.

## Learned pitfalls

- **Search backend down.** `research_run` / `web_search` failing with
  "GEMINI_API_KEY not configured", or a DDG fallback returning nothing, is a
  signal to *switch methods* — not to retry the same query. Go straight to
  `web_fetch` / `headless_fetch` on known domains.
- **Blocked profile pages.** LinkedIn, Tracxn and similar return HTTP 999 /
  403 / 404. Do not retry the same URL. Pivot to authoritative alternatives —
  company site, university page, conference bio, public registry. Prefer one or
  two good sources, extract name variants, role, employer, location, education,
  and public identifiers, and cite only what actually fetched.
