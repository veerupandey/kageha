# Pluggable browser decisions

Rust supports two separate execution paths:

* **Regular route:** the configured heavy agent model plans tool calls.
* **Typed decision backend:** Jev/Clef evaluates bounded questions about one
  observed browser state; Rust builds validated actions from their distributions.
  Uncertain or unsupported work falls back to the regular route.

A small chat model printing JSON and a confidence number is **not** a typed
backend. `kageha_contracts::decisions::DecisionPort` (re-exported as
`typed::DecisionPort`) is separate from the ordinary `ModelPort`. Provider adapters
depend on shared contracts, not browser orchestration. The portable browser
decision crate depends only on contracts, Serde and hashing; it contains no provider,
browser, database or UI dependencies. Any framework can inject this port.
`kageha-models::decisions::HttpDecisions` supplies the real System One transports.

## Batched decisions and execution

`Session::route` prepares field-to-fact questions plus independent navigation and
batch-readiness questions. Each candidate is constructed by Rust from an exact
observed control and a caller-owned value. Equivalent input values are deduplicated
so aliases for the same value do not split probability mass.

The adapter supports 1–64 questions, 2–255 fixed options per question, and validates
exact answer coverage, allowed choices, finite probabilities, total probability
mass and the winning choice. It uses `probabilities[choice]`, not a generated
confidence number. Provider-native confidence is retained as a separate score;
legacy probability gating and explicit provider-confidence gating are distinct.
Question types used by this browser planner are Choice; boolean judgments are
two-option Choices supported by Jev, Clef and Luna Decisions.

Each field has an independent task-eligibility question and a value-mapping
question. Both must pass; merely having a profile fact does not request a write.
Each field's threshold is independent. Accepted independent mappings can become a
batch; a doubtful mapping does not discard other confident fields. The separate
batch-readiness judgment must also pass. Every mapping, eligibility answer and
batch-readiness answer must meet the resolved batch cutoff: the greater of the
single cutoff and the metric-specific batch cutoff. `batch_threshold` applies in
legacy probability mode; `batch_confidence_cutoff` applies in provider-confidence
mode. Both default to 0.99. Otherwise at most one mapping executes.
A batch never includes navigation, upload, scripts, credentials or Enter shortcuts.
No field values are invented. Filled fields (`value_present`) and secure inputs
are preserved. Missing facts or unsupported widgets use the heavy model.

LLMs can also answer multiple questions in one request. This path uses bounded
classification and provider-returned distributions, avoiding generated tool plans
for supported interactions; it does not assume that question batching is exclusive
to decision models or that every workflow will run faster.

The result contains typed `actions`, per-question distributions and confidence,
`score_metric`, resolved `cutoff` / `batch_cutoff`, `minimum_score`, and
`minimum_selected_probability`. The latter is **not** the joint probability that
an entire batch succeeds. Probabilities cannot authorize input or prove completion.

The integrating framework must execute in order through its existing native
executor, revalidate target/resource identity and permissions before every step,
and stop on failure, cancellation, changed documents or an uncertain outcome.
Kageha does this through its original tool path. After a field batch, it observes
again before deciding whether to navigate. Original receipts must be reconciled,
never replayed. The workflow controller verifies outcomes before declaring success.

## Provider-confidence configuration (recommended framework API)

Any framework can supply a decision model name and an optional cutoff:

```json
{"decision_model":"gpt-6-luna","confidence_cutoff":0.90}
```

Deserialize this as `DecisionConfig`, or call `DecisionConfig::new(model)`. Omitting
`confidence_cutoff` uses **0.90 provider confidence**; omitting
`batch_confidence_cutoff` uses **0.99** for every field/eligibility answer and the
batch-readiness answer. These are provisional defaults, not calibrated error rates.
`Session::from_decision_config(scope, &config, backends)` creates the standalone
planner. The adapter never generates missing confidence from probabilities.

```rust,ignore
let config = DecisionConfig::new("gpt-6-luna");
let provider = Arc::new(HttpDecisions::from_env(&config.decision_model)?);
let backends = BTreeMap::from([(config.decision_model.clone(), provider as Arc<dyn DecisionPort + Send + Sync>)]);
let session = Session::from_decision_config(scope, &config, backends)?;
let proposal = session.route(&objective, &observation_request, &control)?;
// Dispatch through the framework's normal authority/receipt executor, or use its
// regular model when proposal.actions is empty. The planner dispatches nothing.
```

`HttpDecisions::from_env` recognizes Jev, Clef/Clef-Flash and Luna decision model
names, selecting only typed endpoints. Custom gateways and credential resolvers
use `HttpDecisions::new(HttpModel)`; no framework or Kageha dependency is required.
The configured OpenAI decision model is `gpt-6-luna` at `/v1/decisions`, separate
from chat/Responses. It is not an alias for `gpt-5.6-luna`.

Cutoff precedence is request `DecisionOverrides` → browser-purpose override →
framework policy → the `DecisionConfig` default. `route_with_overrides` supports
an explicit model name and/or confidence cutoff; models must already be registered
in the framework-owned backend map. Overrides never create credentials or target
permissions. Duplicate/unchanged-state protection still applies; overrides do not
replay input. Audit/results report metric, resolved cutoff, selected probability
and minimum decision score separately.

In Kageha, **Decision-first browser actions → Provider confidence cutoff** saves
the browser-purpose override. New UI opt-ins start at 0.90. A separate batch
confidence input defaults to 0.99. Existing saved probability configurations retain
their behavior until explicitly changed. Missing/invalid confidence, refusals,
invalid distributions, transport errors and insufficient scores use the regular
agent route; they are never silently upgraded to a probability-based acceptance.

## Legacy probability configuration

Action routing is opt-in even if the global decision mode is active:

```json
{
  "mode": "off",
  "model": "cloudflare/clef-flash",
  "threshold": 0.9,
  "timeout_s": 3,
  "max_action_decisions": 64,
  "max_batch_actions": 8,
  "batch_threshold": 0.99,
  "overrides": {"browser_action": {"mode": "active", "threshold": 0.95}}
}
```

`max_batch_actions` accepts 1–16. `max_action_decisions` accepts 1–256 per run.
Primary and backup backends share the same deadline. Invalid distributions,
provider errors, timeouts and low probabilities return no executable proposal.
Shadow mode records decisions without applying them. Semantic state deduplication
and scope-bound checkpoints prevent unchanged-state replay. Once a layout needs
the heavy executor, it retains that layout until the controls change.

In Kageha, configure **Decision Models → Decision-first browser actions**. The
catalog separates Clef/Clef-flash and TypeSafe Jev from chat models. Caller workflows
provide `known_values`; data entry without explicit caller values remains with the
heavy model. Kageha and the daemon leave all navigation on the regular executor.
Embedded frameworks may pass `trusted_click_targets` for known routine controls,
scoped to the exact observation. These grants must come from trusted host policy,
never page attributes, agent arguments or decision output. Without such a grant,
no probability can produce a fast-path click. Unsupported-only pages skip the
decision call entirely. Existing permission profiles, right-panel previews and user takeover apply.
No live service is restarted just by editing/building this component.

## Standalone/API integration

The daemon's private `--decision-config` supplies operator-owned `models`, `policy`
and `contexts`. Supported decision transport protocols are:

* `clef`: full Workers AI `/ai/run/@cf/cloudflare/clef[-flash]` URL; payload contains
  `model`, shared `state` and `questions`; response uses the Workers AI envelope.
* `openai_decisions`: OpenAI `/v1/decisions`; shared state becomes `input_json`,
  fixed Choice questions become the native question array. Native returned
  confidence is preserved; duplicate answers/options and refusals fail closed.
* `system_one`: Jev `/systemone` endpoint or base URL; same typed payload and a flat
  response. Python Kageha's CommandCode aliases use `typesafe/jev[-1.14.0]`.

Keys are environment references in daemon configuration; Kageha uses its existing
credential resolver, including destination binding and revocation checks. Regular
chat transports remain separate. Contexts pin workspace/client/run, intent and
optional named values. Page/model output cannot change these settings.

`browser_decide` with `purpose: "browser_action"`, `run_id`, and a current
`observation_id` returns the proposal **without dispatching input**. The agent maps
its actions to approved native methods; ordinary authority is still required.
Embedded frameworks use `Session::with_decision_backends` and `Session::route`.
Only one component should own decisions for a deployment: Kageha or the daemon.
Persist embedded session checkpoints before dispatch; daemon-only call budgets
currently reside in memory.

## Validation

```sh
cargo test -p kageha-browser-decisions -p kageha-models
cargo test -p kageha-loop --no-default-features --lib
```

`workflow_e2e.mjs --decision-first --typed-decisions --decisions=active` exercises
real Rust browser dispatch with controlled typed distributions. Add `--real
--real-decisions` and configure provider credentials outside chat for live testing.
`browser_decision_bench.mjs --decision-first --typed-decisions` compares the real
typed backend against the same heavy executor on the regular route. Record model,
threshold, task success, latency, actions, failures and usage. Do not treat earlier
GPT-Mini chat-router measurements as Jev/Clef results.

### Earlier probability-cutoff evaluation

The results below predate the provider-confidence option and used
`probabilities[choice]`. They are not confidence-score measurements. The initial
recommendation is **0.95 for a single field** (both value mapping and task
eligibility) and **0.99 for a batch** (every selected mapping, eligibility answer,
and the independent batch-readiness answer). The batch cutoff is now independently
configurable in Decision Settings. These are provisional operating cutoffs, not a
calibration certificate or probabilities of workflow success. Existing saved single
thresholds are preserved; no live settings or hosts were changed by these tests.

Keep submissions, payments, destructive actions, legal commitments, authentication,
uploads and unknown widgets on the regular executor and its existing policy path.
A probability of 1.0 never substitutes for a host target grant, approval, a receipt
or outcome verification. Kageha currently grants no fast-path click targets.
Use Shadow mode for a new backend/version, then qualify narrow operations before
turning on Active. Do not lower the cutoff just to improve apparent coverage.

### Research and practical use

* TypeSafe recommends asking independent and speculative questions together, then
  consuming applicable branches in code: [speculative fan-out](https://docs.typesafe.ai/patterns/fan-out).
  Its [companion Noul pattern](https://docs.typesafe.ai/patterns/companion-noul)
  separates whether a decision applies from choosing the closest option. Our
  eligibility question supplies a related applicability check using a two-option
  Choice; this is not a measured comparison with native Noul.
* [TypeSafe probability routing](https://docs.typesafe.ai/patterns/probability-routing)
  distinguishes identifying an intended operation from authorizing its effects.
  [Confidence documentation](https://docs.typesafe.ai/confidence) explains why a
  distribution concentration score should not be treated as answer accuracy.
* Community implementations include [JevLoop's shared execution kernel](https://github.com/typesafe-ai/skills/issues/11),
  [SkillRanker's Rust skill selection and abstention](https://github.com/Dicklesworthstone/skillranker),
  and [n8n classification with a review branch](https://github.com/khmuhtadin/n8n-nodes-jev-classification).
  These show useful integration patterns, not production evidence for browser use.
  JevLoop's published six-turn example avoided the LLM on only 2 of 22 assisted steps.
* The [Fast Models, Slow Evidence evaluation](https://arxiv.org/abs/2610.02267)
  shows why thresholds need out-of-sample evaluation and why gate accuracy or omitted
  screening costs can produce misleading end-to-end claims. Its workloads are not
  a browser benchmark. A [community calibration report](https://github.com/typesafe-ai/skills/issues/19)
  also reports highly confident errors on issue classification; its author notes
  label noise and single-prompt/single-run limits. Neither source establishes the
  calibration of our browser questions.

### 2026-11-07 cutoff evaluation and fixes

The new Rust evaluation example runs the actual planner and real typed transports.
Each provider response is replayed through the planner over 12 cutoffs; this avoids
new model responses confounding the threshold comparison. Labels are authored
before the run and never sent to the provider. Nothing is clicked in these tests.

Initial testing covered 72 baseline cases and 72 harder cases per backend. The
harder cases exposed two Clef-Flash delete proposals and three Jev payment proposals
at 0.90. Raising the cutoff filtered these examples, but that was insufficient as
an action policy. The fix requires a trusted host allowlist for fast clicks; page
flags and model probabilities cannot create that grant. Pages with no supported
candidates now bypass the decision call entirely. Navigation is delegated to the
regular Kageha agent, which retains full browser-tool access.

After the fix, 150 cases per backend (450 total evaluations, 351 actual provider
responses and 99 deterministic bypasses) produced:

| Backend | Accepted proposals at single 0.95 / batch 0.99 | Wrong accepted | Held-out expected actions covered |
| --- | ---: | ---: | ---: |
| Jev 1.14.0 | 55 | 0 | 48 / 63 |
| Clef | 30 | 0 | 24 / 63 |
| Clef-Flash | 2 | 0 | 2 / 63 |

Abstention invokes the regular route; it is not a failed workflow. Clef-Flash's
coverage at this cutoff is too low to recommend it for automatic field execution
on these fixtures. Jev retained the most coverage. These results are synthetic,
small and correlated: three name/value variants per scenario are not independent
trials. Families are separated between the named baseline calibration and holdout
splits, but related language/templates remain. The additional stress set was used
to discover defects and must not be presented as an untouched validation set for
future prompt tuning. No production error-rate or calibration claim follows.

Even with zero errors, about 299 representative independent accepted decisions
would be needed for a one-sided 95% binomial upper bound below 1% error; repeated
templates do not supply that evidence. Qualify by backend/version, action type,
question version, language and workload; retain fallback and deterministic guards.

Post-fix controlled E2E: two field batches, one upload, Review, no submission.
Real Jev + GPT-5.4 with the proposed 0.95 / 0.99 cutoffs also reached Review from one
request with no human replies, one upload and no submission. It routed two single
fields and no batches in 44.0 seconds: conservative cutoffs reduce acceleration.
This is a correctness test, not evidence of speed superiority. Before this safety
change, three GPT-5.4/Clef pairs had medians 37.4s active versus 37.9s off with one
substantially slower active run; no meaningful general speedup was established.

Rust router/contract/transport and host tests, component boundaries, 94 UI tests,
and TypeScript checks pass. User chats, profiles and real drafts were not touched.
Other heavy model families and production workloads need separate qualification;
standalone daemon budget persistence remains future work.

Reproduce planner tests with `cargo build -p kageha-loop --example browser_decision_eval`,
then `browser_decision_calibration.mjs` with `KAGEHA_DECISION_EVAL_BIN` set. Credentials
come from server-side environment variables or `KAGEHA_TEST_ENV_FILE`, never chat.
`KAGEHA_DECISION_CASES` selects the frozen baseline or stress JSONL fixture. The
result contains per-case distributions, proposed actions, threshold curves and
broad exploratory family-level error bounds, not a fitted calibration model.

Local raw evidence:
* Before fix: `/tmp/kageha-decision-calibration-wDlK2r/report.json` and
  `/tmp/kageha-decision-calibration-YTZjMi/report.json`.
* After fix: `/tmp/kageha-decision-calibration-XO3xoy/report.json` and
  `/tmp/kageha-decision-calibration-DFOcLZ/report.json`.
* Real workflow: `/tmp/kageha-workflow-e2e-0XbhXH/result.json`.

## Provider-confidence comparison checkpoint

The confidence-mode tests use the provider-returned `confidence` field, not
selected-option probability and not a generated score. Defaults are 0.90 single
and 0.99 batch; framework and request overrides are tested separately. Missing
confidence explicitly produces no fast-path action, even if selected probability
is 1.0. Existing saved probability-based settings remain unchanged.

Real Jev 1.14.0, Clef and OpenAI `gpt-6-luna` on `/v1/decisions` evaluated the same
150 synthetic cases each (450 case evaluations, 351 provider responses plus 99
deterministic no-model bypasses). At confidence 0.90 / batch confidence 0.99:

| Backend | Held-out expected actions covered | Wrong accepted proposals | Median successful provider evaluation |
| --- | ---: | ---: | ---: |
| Jev 1.14.0 through CommandCode | 43 / 63 | 0 | 255 ms |
| OpenAI Luna Decisions | 32 / 63 | 0 | 195 ms |
| Cloudflare Clef | 14 / 63 | 0 | 303 ms |

This is operational coverage under one configured cutoff, not a universal model
capability ranking. Native confidence scores differ by provider; equal numeric
cutoffs are not equal error rates. Hosting/network/cache effects affect latency,
and repeated templates are correlated. These tests do not establish production
calibration or a statistically meaningful end-to-end speedup.

All three decision backends also reached Review through real Kageha + GPT-5.4 from
one request, without human replies, with one upload and no submission. Jev routed
three field actions, Luna two and Clef zero; Clef's regular-agent fallback completed
the workflow. The conservative batch confidence cutoff admitted no real-model
multi-field batches in these particular runs. Controlled typed fixtures executed
two batches; missing-confidence fixtures correctly used only the regular executor.

Wire tests confirm Luna uses `input_json` and native typed `questions` on
`/v1/decisions`, without chat messages or tool generation. Refusals and duplicate
answers/options fail closed. Raw confidence and probability remain distinct, and
request overrides take precedence over browser-purpose and framework defaults.
Model-only selection uses the typed provider registry; provider credentials are
resolved outside the framework-facing configuration. Luna is registered separately
from chat models and cannot silently enter a regular chat fallback ladder.

[OpenAI Decisions documentation](https://developers.openai.com/api/docs/guides/decisions)
currently specifies `gpt-6-luna`; no `gpt-5.6-luna` substitution is made. Provider
confidence is a distribution summary, not proof of a correct browser outcome;
see [TypeSafe's confidence documentation](https://docs.typesafe.ai/confidence).
The earlier probability evaluation above is intentionally labeled separately.

Raw comparison: `/tmp/kageha-decision-calibration-mTBk1x/report.json` and
`/tmp/kageha-decision-calibration-uCL76y/report.json`. Real workflow evidence:
Jev `/tmp/kageha-workflow-e2e-8GbPAg/result.json`, Clef
`/tmp/kageha-workflow-e2e-KbC3HD/result.json`, Luna
`/tmp/kageha-workflow-e2e-4Y2wvY/result.json`. Run
`browser_decision_calibration.mjs` with `KAGEHA_EVAL_CONFIDENCE_MODE=1` to reproduce
confidence sweeps; omit it only for an explicitly labeled probability comparison.

Latest checks: Rust decision/config/transport tests, 71 host library tests,
component dependency boundaries, 95 UI tests and TypeScript checks pass. No live
services were restarted, chats modified, or real application drafts submitted.

## Framework-neutral service integration checkpoint

The same minimal `DecisionConfig` is now accepted by the standalone daemon's
operator-owned `--decision-config` as the root `decision` or a per-context
`decision` override. It can resolve typed providers by model name without a
Kageha database. `apply_to` preserves framework review policy and budgets while
inheriting unspecified confidence cutoffs. See the
[standalone contract](../kageha-browser/STANDALONE.md#framework-neutral-decision-contract).

Independent HTTP and actual LangGraph/official-MCP integration caught two bugs
that embedded Kageha tests had masked: internal cached snapshots lacked public
control references, and Tokio blocking workers retained a runtime Handle rejected
by model ports. The daemon now projects the public control contract itself and
uses bounded, cancellation-aware OS workers for model execution. No transport
calls the Kageha host or duplicates the planner's browser logic.

Both external decision and regular-fallback cases pass, with one physical input,
a verified native receipt, and no save/submission. The full LangGraph regression
suite passed 32 tests. Evidence: `/tmp/kageha-portable-http-test.log` and
`/tmp/kageha-portable-langgraph-full-tests.log`. These use deterministic providers
and disposable Chromium, not real user accounts. Live context registration and
durable standalone decision-budget checkpoints remain explicit gaps; proposal
cutoffs and context facts are currently startup-owned, never model-editable.

### Kageha settings, loop and panel integration

Kageha freezes decision settings with the run spec; changing settings affects new runs. The browser footer exposes the latest host-owned route, configured backend, score metric/cutoffs and reason for regular-agent fallback. It uses the existing browser state response, with no extra polling or preview lifecycle changes. Metadata is stored separately per chat/turn, so late workers cannot overwrite a newer run. No caller values or prompts are included. Route proposals do not claim native execution succeeded.

The UI requires a connected typed backend before enabling browser actions. The loop checks user takeover before inference. Ordinary chats without caller-provided workflow values stay on the regular agent path without an extra decision snapshot or inference. Typed batching remains available to caller-configured workflows; this does not silently extract trusted facts from model-generated text.

Integration verification (2026-11-08): 72 Rust host unit tests, 100 focused UI tests, TypeScript build and component boundaries passed. A disposable six-step Kageha workflow reached Review from one request with two typed batches/six proposed actions, one upload and zero submissions. Changing settings while running preserved the original cutoff. A missing-native-confidence run routed zero fast actions and completed using the regular executor. Both fixtures deliberately exercise recovery from one tool failure. These are controlled integration tests, not new live-provider calibration or a claim that arbitrary websites are failure-free. Visual inspection covered 320, 390 and 900 px browser panel widths.
