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

`HttpDecisions::from_env` recognizes Jev, Clef/Clef-flash, GPT-6 Luna Decisions and Gemini Decision 1 model
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
the browser-purpose override. Select provider confidence explicitly when the backend supplies it. The default
is selected probability (0.90 for an action and 0.99 for a batch). Existing
cutoffs retain their behavior until explicitly changed. Missing/invalid confidence, refusals,
invalid distributions, transport errors and insufficient scores use the regular
agent route; they are never silently upgraded to a probability-based acceptance.

## Legacy probability configuration

Kageha policies that opt into `browser_actions_follow_mode` let browser actions
inherit the default Off, Compare, or Active mode. An explicit browser action
override takes precedence. Browser guidance and recovery are separate opt-ins
under this policy. Legacy policies retain their existing default behavior. A
portable policy can use an explicit browser action override:

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

In Kageha, open **Settings → Decisions**, choose a configured model, and select
**Use decisions (Active)**. **Test decision model** sends a synthetic question
through the configured typed endpoint. It checks the response contract and
selected score type. The browser route indicator separates proposals from actions
verified by the native executor. Saving applies the policy to new runs. Caller workflows
provide `known_values`; data entry without explicit caller values remains with the
heavy model. In ordinary Kageha chats the host can grant an explicitly requested link or managed tab, a named native disclosure, or same-path numeric pagination with unchanged filters. Workflows and the standalone daemon default to the regular executor for navigation.
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
* `gemini_decisions` uses `gemini-decision-1` at Google’s `/v1beta/decisions`
  with `GEMINI_API_KEY`. It submits the observed state as text, sends named
  choice questions, and preserves each answer’s probability and confidence.
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

Run `browser_decision_bench.mjs --qualify` with the isolated provider
credentials, frozen host and browser binaries. The report covers ten alternating
Active/Off pairs each for ordinary search/filter and form-fill. It records
verified actions, independently checked task outcomes, provider usage and paired
latency. The default qualification provider is `typesafe/jev-1.14.0`. The
runner needs `CMD_API_KEY` and a main-model API key in its environment or
`KAGEHA_TEST_ENV_FILE`. It does not read the live credential vault.

## Operating the decision route

Start in Compare mode and review the proposed actions before enabling Active. Approval, execution and receipt handling stay with the native executor. Omit optional cutoffs to inherit the configured host policy. See the [standalone configuration](../kageha-browser/STANDALONE.md#framework-neutral-decision-contract) for service deployments.

### Settings and browser panel

Kageha freezes decision settings with the run spec; changing settings affects new runs. The browser footer exposes the latest host-owned route, configured backend, score metric/cutoffs and reason for regular-agent fallback. It uses the existing browser state response, with no extra polling or preview lifecycle changes. Metadata is stored separately per chat/turn, so late workers cannot overwrite a newer run. No caller values or prompts are included. Route proposals do not claim native execution succeeded.

The UI requires a connected typed backend before enabling browser actions. The loop checks user takeover before inference. Ordinary chats can use quoted values, named fields and search terms from the current operator request, including steering. The planner considers observed fields and native select options after browser activity starts. The browser panel reports the latest routed action and its native execution result.
