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
Both suites support `--seed-start=101` for separate validation seeds. After the
runtime fixes, the MiniWoB subset was also run on seeds 101–105 through both
transports, without further tuning; all 70 additional episodes passed.

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

## Failures that drove runtime improvements

| Reproduction | Before | Result after the fix |
|---|---|---|
| Original 12 adversarial scenarios, seeds 1–2, both transports | 40/48; dense targets and open shadow controls missing | All matching cases pass in the expanded run |
| Partial obstruction, seeds 1–2, both transports | 0/4; center hit rejected an otherwise clickable button | Visible safe hit point selected before one click |
| Seven public tasks, seeds 1–10, both transports | 138/140; two-button sequence seed 6 failed in both transports | Same episodes pass with obstruction-aware hit testing |
| Script-backed anchor without `href`, both transports | 0/2; observed link rejected before dispatch | Observed anchor activates through the same safe hit testing |
| Printable keyboard events, both transports | 0/2; punctuation rejected and shifted digits incorrect | Trusted key events and expected typed text verified |
| Public terminal task, seeds 1–3, both transports | 4/6; dotted filenames could not be typed | 6/6 using visible `ls` output and native key presses |
| Multi-select, both transports | 0/2; array values rejected | Exact selection, clearing, idempotence, invalid-value guards and batch path pass |

The changes are general runtime behavior:

- Expanded observations have their own bounded control/text budget and expose
  later references. Reference resolution accepts the observation's bounded
  control set instead of imposing the compact view's 18-reference limit.
- Open shadow controls participate in observation and reference resolution.
  Hit testing descends open roots, focus uses the control's own root, and disabled
  composed ancestors remain enforced. Closed-root controls remain excluded.
- Click preparation checks a bounded set of interior hit points. It dispatches
  one click only after finding a point belonging to the intended surface. It
  never probes an overlay by clicking it, and completely covered controls still
  fail before input.

Saved reports in `results/` retain the baseline and the current seeded runs.
The fixture set expanded after the first baseline; compare matching scenario/seed/
transport identities rather than comparing the two totals as identical workloads.

Recorded final runs on 2026-11-07:

| Suite | Result |
|---|---|
| 24 adversarial scenarios × 5 seeds × 2 transports | 240/240, zero wrong effects |
| 7 public MiniWoB tasks × 10 seeds × 2 transports | 140/140 |
| MiniWoB validation seeds 101–105 × 7 tasks × 2 transports | 70/70 |
| Public MiniWoB terminal × 3 seeds × 2 transports | 6/6 |

Rust unit tests, the native HTTP integration test, and Kageha's browser integration
test also passed with the updated runtime. The Kageha integration used disposable
Chrome/Comet fixture profiles and checked continuous preview, binding-aware paint
acknowledgments, exact upload approval, control fencing and human handoff.
The Rust host now exposes select/check tools, forwards frame scope, and sends
focused-key actions using the native schema. Approval refresh requires the same
document and backend DOM identity. The integration also checks multi-select,
radio and punctuation-key input through approved agent tools.

## Remaining evaluation work

These are not full MiniWoB++, WebArena, VisualWebArena or WorkArena scores.
The public subset covers seven task families. Full realistic web benchmarks need
additional environment provisioning and LLM-agent evaluation. BrowserGym/AgentLab
can provide that outer evaluation harness while browser execution remains Rust.

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
HTTP 429 responses slow subsequent inference calls across all workers. An earlier
60-RPM run is retained as `*-rate-diagnostic.json`, marked incomplete rather than
reported as a completed model score. See [Cloudflare's limits](https://developers.cloudflare.com/workers-ai/platform/limits/).

The initial full GPT-5.4 and Cloudflare DeepSeek runs (`*-before.json`) exposed
provider transport failures and teardown isolation errors. They are diagnostic
baselines, **not valid model rankings**. Browser and host-adapter changes from
those runs include reference clicks on text fields/selects, observed read-only
and checked states, document-scoped identity refresh, and keyboard action batching.
Full model runs are defect-discovery passes, not a controlled model ranking:
their binary hashes and reasoning modes differ as fixes are evaluated. In
particular, the broad passes predate the final punctuation-key fix. Targeted
regressions validate the patched capability separately. An independent model
attempt still failed the terminal task even after native keys were fixed;
successful deterministic control must not be reported as autonomous model success.

Completed one-seed catalog passes on 2026-11-07:

| Provider/model | Reasoning | Families attempted | Strict reward 1 | Positive reward | Provider/setup failures |
|---|---|---:|---:|---:|---:|
| OpenAI API / GPT-5.4 | low | 125 | 76 | 82 | 0 / 0 |
| Cloudflare / DeepSeek V4.1 Flash 1017 | none | 125 | 62 | 68 | 0 / 0 |

These runs exposed faults that were then patched. They are not scores for the
final patched binary. Both requested models passed the patched `choose-list`
and `click-scroll-list` retests, two tasks each; their full passes had failed the
multi-select task. Calendar booking
retests still failed to complete the workflow even after anchor clicks worked.
Remaining model failures include multi-step email/calendar planning, premature
completion claims, and precise visual interaction. Every report grades the
original task outcome independently of the model's claim.

Kageha's integration regression uses a deterministic provider fixture to verify
its native tool/approval/preview contract. The live model catalog runs use the
independent standalone adapter, not Kageha's task planner. The native Rust Kageha
tool surface still needs parity qualification for the standalone runtime's
script, scroll/drag, dialog and download capabilities. Broader held-out model
runs and provisioned WebArena/WorkArena environments remain production gates.

`preflight.mjs` checks external environment readiness without resetting sites or
accepting gated access terms. WebArena currently lacks isolated site endpoints
and images; WorkArena's instance configuration returned HTTP 403. Neither suite
has a completed score. A local MiniWoB result must not be relabeled as either.

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
