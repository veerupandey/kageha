---
name: getting_started
description: Orient to Kageha — where skills/tools live, how chat routing works, and which knobs matter for a first successful run.
---

# getting_started

Load this when the user asks how Kageha works or is stuck on setup. Full
reference lives in `docs/SETUP.md` and `docs/USAGE.md` — point there rather
than reproducing it.

## Routing

Most messages become an agent turn and the model picks its own depth. Only
narrow paths skip the loop: `quick_where` / `quick_status` ("where did you save
it?", `/status`), and cancel. Deeper work goes through `/plan`, `/goal`, or
`escalate_plan(mode=…)`.

## Skills

Three levels: the ranked catalog (L1), the auto-loaded `SKILL.md` body once a
match clears `KAGEHA_SKILL_AUTOLOAD_MIN` (default `3`) (L2), then `skill_read` /
`skill_run` for references and scripts (L3). Matching uses the description,
optional `triggers:`, and embeddings. Exclusive families
(`computer_use` / `web_*`) load only the winning skill.

`/skill <name>` or `$<name>` invokes explicitly, bypassing both the score floor
and `disable-model-invocation`.

Skills resolve from `bundled_skills/` (shipped), `~/.kageha/skills/` (user
overrides by name), `.kageha/skills/` (project), and `KAGEHA_SKILLS_PATH`.

## Tool packs

Core packs load by default: `forge`, `skills`, `mcp`, `memory`, `subagent`,
`research`. The `browser`, `computer`, and `media` packs are opt-in via
`KAGEHA_TOOL_PACKS=browser,computer,media` or `packs:` in `tools.yaml` — so if a
browser or desktop tool is missing, that pack is simply off.

`/machine <task>` is a separate, explicit isolated Linux computer (Docker).
It is not enabled by `KAGEHA_TOOL_PACKS=all`. `/computer` is native macOS.
WebUI Live needs Playwright on the host (`uv sync --extra browser`); Chromium
is already in the box — do not install it from bash.

## Setup answers

- No model configured → `kageha setup`, then `kageha models list`.
- Skills missing → `kageha skills list`.
- MCP → `kageha mcp init`, then `kageha mcp add <name> -c <cmd>` (stdio) or
  `--url <url>` (SSE/HTTP). Remote HTTP needs `uv sync --extra mcp`.
- Custom extensions → `~/.kageha/skills/<name>/SKILL.md`, tools in
  `~/.kageha/tools/*.py` exposing `register(ctx)`.
