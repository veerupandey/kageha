# Real agent fan-out demo

Run a read-only release review of Kageha's actual Rust source using a configured
live model. The parent agent spawns three specialists in one tool-call response:

- Dispatch ordering, resource conflicts and cancellation.
- Worker lifecycle, wait-any and crash recovery.
- Approval authority, receipt persistence and unknown writes.

Each specialist reads three source files through Kageha's real `read_file` tool.
The parent gathers results through `agent_wait_any` and writes a combined answer.
Model findings remain review suggestions, not independently confirmed defects.

```sh
cargo build -p kageha-loop
node scripts/demos/agent_repo_review.mjs --output /tmp/kageha-review
```

The driver needs Node with `node:sqlite` (tested with Node 23.10). It starts a
separate Rust server on an ephemeral loopback port, creates isolated demo chats,
and stops that server afterward. Its backend PATH is empty; no Python backend,
Node backend, shell tool or browser mutation is involved. Existing running servers
and chats are unchanged. Legacy chat import is explicitly disabled for the demo.

By default, it copies only the selected model definition, provider credential
reference and owner metadata into a private temporary state. It snapshots the
encrypted vault and registry using SQLite's online backup, then lets the Rust
backend resolve the credential through its native Keychain or configured key
file. No plaintext vault credential is exported to the launcher, logs or reports.
The temporary state is removed when the run exits unless `--keep-state` is used.
Only an active embedded vault is supported; resolve pending vault replacements
before running. If the selected model has no vault binding, an explicitly
configured and present `api_key_env` remains supported. The isolated process
uses its own `KAGEHA_HOME` for learning data.
If macOS reports that saved credentials are locked, rerun with `--unlock-vault`
and authorize the Keychain prompt yourself. Each trial uses a separate Rust
process, so macOS may prompt repeatedly. If offered, **Always Allow** can avoid
repeated prompts for that executable; choose it only if you want to grant that
ongoing Keychain access. An interrupted run deletes its temporary credential
snapshot even when `--keep-state` was requested.
Override configuration with:

```sh
node scripts/demos/agent_repo_review.mjs \
  --settings-db /path/to/native-chats.sqlite \
  --model-id YOUR_CONFIGURED_MODEL_ID \
  --output /tmp/kageha-review
```

Or set `KAGEHA_PROVIDER_URL`, `KAGEHA_MODEL`, and `KAGEHA_API_KEY` for an HTTPS
OpenAI-compatible provider. Optional flags are `--workspace`, `--binary` and
`--keep-state`. Keeping state retains only the isolated demo's database.

The output directory contains `review.md`, `report.json`, and `backend.log`.
The report includes parent/child results, sampled timing transitions, ordered
durable lifecycle evidence and tool results. It verifies all three workers
succeeded, their execution overlapped, each completed at least three source reads,
and the parent completed. This is a real-use-case demonstration, not a controlled
serial-versus-parallel speed benchmark; model latency varies.

Recheck saved evidence without making new model calls:

```sh
node scripts/demos/agent_repo_review.mjs --verify-report /tmp/kageha-review/report.json
```

Compare another live run with a saved successful run using the same model/tasks:

```sh
node scripts/demos/agent_repo_review.mjs \
  --compare-report /tmp/kageha-review-before/report.json \
  --output /tmp/kageha-review-after
```

The report records before/after wall time, peak concurrency, matching model/tasks,
and source characters actually read. It does not attribute live timing differences
to Rust alone: the reviewed source and provider latency can change.

## Controlled-width agent speed benchmark

The separate benchmark runs four fixed, read-only token-extraction tasks with one,
two and four concurrent specialists. The trial order rotates between rounds. Each
trial starts a fresh isolated Rust server, and timing covers chat submission through
the parent's response. The parent and every child must return the expected tokens,
every child must have a successful `read_file` receipt for its token, and the
recorded peak concurrency must match the requested width. The coordinator's
inherited policy permits `read_file` so workers can use it; a trial is rejected
if the coordinator reads a fixture itself. Failed or nonconforming
trials remain in the output and are excluded from latency summaries.
An authentication or provider configuration failure stops the remaining trials;
the partial summary preserves the failure and a safe explanation.

This deliberately runs **real model calls**. Each round makes three complete runs
with multiple child model calls, so provider charges may apply. It never runs as
part of the normal test suite. With an HTTPS provider already configured:

```sh
cargo build -p kageha-loop
node scripts/demos/agent_speed_benchmark.mjs \
  --output /tmp/kageha-agent-speed-01 --rounds 3 --model-id YOUR_CONFIGURED_MODEL_ID
```

It accepts the same provider environment variables and `--binary`, `--settings-db`
and `--model-id` options as the demo above. Output must be a new directory. It stores
the fixture, a SHA-256 fingerprint, the runtime binary hash, a hashed provider/model
identity, per-trial logs and evidence, plus `summary.json` with attempts, verified
counts, p50 and p95. Cost is `null` when it cannot be measured; it is not estimated.
The report separately records server acceptance, first model commentary, first
successful non-cached tool result, and completion as observed by a headless event
subscriber. Each milestone reports sample and missing counts. These observations
include transport and event delivery; a tool result is not its invocation start.
The runner cannot measure the browser's immediate acknowledgment, which is `null`.
When each width has at least three verified trials and no failed trials, the
summary also identifies the width or tied widths with the lowest observed median.
This describes the sample; it is not a statistically established improvement.
Observational cohorts and benchmark results do not change runtime routing.
For a later run, keep the runtime and provider configuration comparable and pass
`--fixture /tmp/kageha-agent-speed-01/fixture` to use the identical input. A custom
fixture root may also contain four distinct single-line UTF-8 tokens, each at
least eight characters and not contained in another token, in
`fixtures/task-1.txt` through `fixtures/task-4.txt`.

This benchmark verifies extraction and coordination, not general answer quality.
The p95 of a small sample is noisy, and provider load, cache state and model output
vary. An observed timing difference does not prove an architectural improvement.

To compare the original `agent_wait_any` loop with `agent_wait_all` at the same
four-worker width, alternate three paired rounds against the same runtime and fixture:

```sh
node scripts/demos/agent_speed_benchmark.mjs \
  --output /tmp/kageha-wait-comparison --rounds 3 --compare-wait --unlock-vault \
  --fixture /tmp/kageha-agent-speed-01/fixture
```

The comparison verifies which wait tool was actually used and reports failed trials
alongside timings. Each benchmark server opts in to recording model-adapter call
durations, without prompts or responses. Those durations include local adapter work,
network transport, provider latency, retries and backoff; they cannot isolate provider
latency. Worker time outside observed model calls is an unclassified remainder. Model
timings are unavailable when instrumentation is incomplete, including for old reports.
Recording diagnostics adds some overhead to both modes; this comparison measures
instrumented runs rather than the exact latency of uninstrumented production runs.
Run its offline verification tests without model calls using:

```sh
node --test scripts/demos/agent_speed_benchmark.test.mjs
```
