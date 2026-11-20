# Codex app-server runtime

Kageha supports an opt-in Codex app-server model. This uses the local Codex
CLI's ChatGPT OAuth session and lets Codex own its native agent loop while
Kageha exposes additional tools through MCP.

## Setup

```bash
npm install -g @openai/codex
codex login

# Confirm the login
codex login status

# Start Kageha normally, then select the opt-in model:
./start
```

Select `gpt-codex-app` in the model menu or use `/model gpt-codex-app`.

In Settings, **ChatGPT Codex login** reflects the single CLI-owned OAuth
connection used by **Codex Runtime**. Kageha does not copy this credential into
its own auth store. The composer exposes the public Codex Runtime lineup
(`gpt-codex-app` for Sol, plus Terra, Luna, GPT-5.5, GPT-5.4, and GPT-5.4
Mini) as native-tool-loop models.

## Runtime behavior

`gpt-codex-app` starts `codex app-server` over JSONL and uses the app-server
thread/turn protocol. Codex supplies shell execution, patches, planning,
image viewing, sandboxing, and hosted web search. In a live controller turn,
Kageha adds browser, research, document, media, skill, and connector
capabilities as session-scoped dynamic tools. Direct/standalone model use has
no live session context, so it uses the safe `kageha` MCP fallback instead.

At startup Kageha queries `modelProvider/capabilities/read` and `model/list`.
This verifies namespace tools and records native web-search, image-generation,
and image-input support instead of assuming that every installed CLI/provider
combination exposes the same tools. Installed Codex apps/connectors are also
read through the stable `app/installed` endpoint; Kageha inherits the user's
Codex configuration and does not mutate experimental plugin state.

The WebUI separates these OAuth-backed models from OpenAI API-key entries.
It deliberately excludes the app-server's internal reserve and auto-review
rows. Every Codex Runtime selection keeps `tools.web_search=true`; Search
settings explains that Codex uses hosted web search while that picker controls
Kageha's provider-backed web tools and non-Codex models.

In the normal CLI/WebUI controller path, the selected model is given the live
`HarnessContext`: Codex calls a curated Kageha registry through
`item/tool/call`, and its approval requests use the same Kageha HITL gate.
Codex-native shell, patch, planning, image-view, and web-search tools remain
Codex-owned. The curated bridge exposes Kageha-only capabilities such as
browser/research, skills, documents, and image/vision tools when those tools
are loaded in the session. Persistent memory and agent-loop tools such as
delegation remain on Kageha's normal runtime because they require the parent
loop.

Both native and Kageha web paths are available. Codex can use its hosted
`webSearch`, or call Kageha's `web_search`, `parallel_web_search`, `web_fetch`,
and `parallel_web_fetch` through the guarded dynamic namespace. Compact,
redacted native search previews are projected into the normal tool UI; raw
app-server tool payloads and inline media are not copied into the journal.

User image attachments are sent as app-server `image` or `localImage` input
items. Native `imageGeneration` results are copied into the session artifact
directory. Images produced by Kageha media tools are returned to Codex as
typed `inputImage` content as well as the JSON artifact envelope, allowing the
same turn to inspect or refine the generated image.

When no live Kageha context is available, Kageha MCP is launched with
`--safe-only` by default. This prevents tools with mutating or interactive risk
classes from being silently auto-approved inside the separate MCP process.
Inside the normal CLI/WebUI turn, the in-process bridge is used instead, so
Kageha's tool policy and approval gate remain authoritative.

## Current boundaries

- The runtime is opt-in and does not replace Kageha's normal durable loop.
- `gpt-codex-app` currently delegates the active turn to Codex; Kageha
  receives the streamed Codex events and remains responsible for the WebUI
  surface and surrounding session state.
- Direct model construction without a live controller context uses the safe
  MCP bridge; the controller path injects session-aware dynamic tools.
- Codex app-server dynamic tools are experimental and versioned with the
  installed Codex CLI. The adapter is tested against the installed CLI and
  falls back to the MCP path for hosts that do not provide a live context.
- Native Codex image generation is surfaced as a normal Kageha image artifact
  when the installed Codex CLI returns a completed `savedPath` or inline image
  result. Kageha's `nano_banana_*` tools remain available as a separate
  provider-backed image path.
- App-server stdout uses a bounded 64 MiB JSONL reader so inline image results
  can be decoded within the transport limit. Stderr is drained into a
  bounded diagnostic tail and included when the subprocess exits unexpectedly.

## Live contract tests

Mocked unit tests cover transport and policy behavior. The opt-in suite checks
the installed CLI and real ChatGPT OAuth session for capability discovery,
chat, token usage, dynamic tools, image input, native web search, native image
generation, and artifact delivery:

```bash
KAGEHA_CODEX_LIVE=1 pytest -q tests/test_codex_app_server_live.py
```
