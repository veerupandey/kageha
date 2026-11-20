# Browser-control quality benchmarks

These suites exercise the standalone Rust runtime through both HTTP and native
MCP. They use disposable Chromium instances and local pages, including a pinned
public MiniWoB++ subset. No user profile, live account or application draft is
opened. All task inputs go through the public Rust interface.

## Reproduce the quality gate

Build the browser binary, then obtain the reviewed MiniWoB++ fixture source:

```sh
cargo build --locked -p kageha-browser

git clone https://github.com/Farama-Foundation/miniwob-plusplus.git /tmp/miniwob-bench
git -C /tmp/miniwob-bench checkout 33c3b4ddef8c6eb67c57a29663d844b1eda7e614

KAGEHA_BROWSER_BIN=/absolute/path/to/kageha-browser \
CHROMIUM_BIN=/absolute/path/to/disposable/chromium \
MINIWOB_ROOT=/tmp/miniwob-bench \
node crates/kageha-browser/bench/gate.mjs
```

The gate rejects a missing or modified public fixture checkout. It does not
silently download dependencies or skip the public suite. Both transports must
pass every task and refusal check. Adversarial and subset tasks have a 40-RPC budget;
the keyboard terminal regression allows 60 RPCs for individual physical keys. The
suite driver enforces bounded waits, and the public tasks retain their original
episode timers and reward logic. The gate exits nonzero on failures.

`BROWSER_BENCH_SEEDS` defaults to 5, `MINIWOB_SEEDS` to 10 and
`BROWSER_TERMINAL_SEEDS` to 3. Set
`BROWSER_BENCH_REPORT_DIR` to preserve each CI run in a separate artifact directory.
The scripts also run independently:

```sh
node crates/kageha-browser/bench/adversarial.mjs \
  --seeds=5 --transport=both --output=/tmp/adversarial.json
node crates/kageha-browser/bench/miniwob.mjs \
  --seeds=10 --transport=both --output=/tmp/miniwob.json
```

`--transport=http` or `--transport=mcp` selects one path. The adversarial suite
also accepts a comma-separated `--scenarios=dense,shadow,partial` for diagnosis.
Both suites support `--seed-start=101` for separate validation seeds.
## What the gate measures

| Layer | Coverage | Passing criterion |
|---|---|---|
| Dense and dynamic controls | More than 18 controls, unrelated continuous DOM updates, delayed hydration | Intended control discovered and acted upon within budget |
| Visibility and interaction | Offscreen controls, hidden native radios with labels, partially obstructed buttons | Exactly one trusted intended effect, no obstruction clicked |
| Encapsulation | Open shadow buttons and text inputs, shadow overlays and disabled hosts | Correct composed-tree interaction; blocked input refused |
| Identity and recovery | Replaced node, replaced document, nested frames | Old identity refused before dispatch; fresh identity succeeds |
| Refusal checks | Full overlays, ambiguous locators, disabled controls | No dispatch and no wrong side effect |
| Interactive surfaces | Editable and read-only input focus, native select opening, ARIA switches and tabs | Trusted focus/click without changing read-only values |
| Native selection | Scalar compatibility, multi-select, clearing, disabled options, duplicate values, batch path | Exact selected-values readback and no extra change events |
| Public tasks | MiniWoB click-button, enter-text, enter-text-dynamic, click-checkboxes, click-checkboxes-large, click-dialog, click-button-sequence | Original task reports done and raw reward exactly 1 |

The adversarial fixture has a server-side event ledger. It checks which effect
occurred, `event.isTrusted`, duplicate effects and wrong targets. Environment
commands simulate external rerenders/navigation; they never perform the task
input. A successful receipt alone does not pass the task.
Click, key and text-input cases require trusted events. Native select uses DOM
selection and input/change events, with exact selected-values readback; these
events are not represented as trusted physical input.

For MiniWoB, a separate evaluator channel seeds and resets the episode, then reads
its original reward after the Rust-controlled actions. It does not read hidden
answers for the driver, select elements, fill inputs or click controls. The driver
reads instructions and references from Rust observations. Source files and reward
code are unmodified. The instruction parser is deterministic and task-specific;
this is a browser-control evaluation, not an LLM reasoning or general-agent score.

The tests emit per-task results, seeds, transport, RPC count, duration, failures,
source revision and browser-binary SHA-256. Adversarial reports also include wrong
effects and RPC latency distributions. Latency is diagnostic: these runs share a
machine with other work and are not a controlled language-performance comparison.

## Full catalog model evaluation

`agent.mjs` evaluates all 125 families registered by the pinned BrowserGym
MiniWoB catalog. This is an independent vision/observation agent using the Rust
HTTP interface, not Kageha's planner or an official BrowserGym leaderboard run.
The evaluator only seeds, resets and grades original tasks. The model cannot
execute JavaScript or inspect evaluator state. Each episode owns a disposable
daemon and Chromium instance, so an unresolved receipt cannot poison later tasks.

Create a private (`chmod 600`) JSON provider file outside the repository. For
OpenAI use `provider: "openai_responses"`, `base_url: "https://api.openai.com/v1"`,
`model: "gpt-5.4"` and `api_key`. For Cloudflare use `provider: "openai_compat"`,
the account's Workers AI `/ai/v1` base URL, the available DeepSeek model ID and
`api_key`. Credentials are never included in reports. Then run:

```sh
BROWSER_EVAL_PROVIDER_FILE=/absolute/private/provider.json \
KAGEHA_BROWSER_BIN=/absolute/path/to/kageha-browser \
MINIWOB_ROOT=/tmp/miniwob-bench \
node crates/kageha-browser/bench/agent.mjs \
  --workers=2 --turns=12 --seed=1 --output=/tmp/model-evaluation.json
```

`--tasks=click-button,enter-text` selects a diagnostic pilot. Full runs use the
same 12-decision, 40-action and 180-second outer task budgets; model requests
have a 45-second deadline. Reports retain strict reward-1 successes, positive
partial rewards, provider/setup errors and browser refusals separately. Only
model inference transport failures are retried; browser inputs are never
automatically replayed. Three consecutive provider failures stop scheduling
episodes and mark the run incomplete. Detailed model traces are ignored by Git.
`--reasoning=low` is the default; `--reasoning=none` explicitly disables reasoning
for providers supporting that setting. Reports record the mode. Compare model
scores only after accounting for mode, concurrency, and provider failures. The
DeepSeek low-reasoning diagnostic is retained separately because repeated
truncation prevented an action plan from being returned on harder episodes.
Cloudflare defaults to 18 requests/minute, leaving headroom under its documented
20 requests/minute standard frontier-model limit. `--rpm` overrides the pacing;
HTTP 429 responses slow subsequent inference calls across all workers. See [Cloudflare's limits](https://developers.cloudflare.com/workers-ai/platform/limits/).

The preflight runner checks whether external fixtures are configured. Reports default to `output/benchmarks/browser/`.

Profile switching, uploads, leases, disconnect recovery and right-panel streaming
have separate integration regressions. Run those alongside this gate:

```sh
node crates/kageha-browser/tests/http_api.mjs
node crates/kageha-browser/tests/mcp.mjs
node crates/kageha-loop/tests/webui_browser_e2e.mjs
```

Keep held-out seeds and additional task families for future changes; do not tune
against these results and describe the resulting score as general web autonomy.

Sources: [MiniWoB++](https://github.com/Farama-Foundation/miniwob-plusplus),
[BrowserGym](https://github.com/ServiceNow/BrowserGym),
[AgentLab](https://github.com/ServiceNow/AgentLab),
[WebArena](https://github.com/web-arena-x/webarena).
