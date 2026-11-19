# Independent model adapters

This Rust library implements `kageha_contracts::execution::ModelPort` (also re-exported by the runtime). It has no
Kageha application database, settings, WebUI or browser dependency.

`HttpModel` accepts validated `Provider` configurations for OpenAI-compatible and
Anthropic-compatible protocols. It normalizes one complete assistant response and
ordered tool calls into the runtime contract. Fallback adapters receive the same
conversation; partial responses and tool arguments are never combined.

The shared Reqwest connection pool replaces model-call curl subprocesses. Requests
and responses are bounded to 8 MiB, assistant text to 256 KiB and tool batches to
16. Cancellation and deadlines abort I/O. Redirects are disabled. Remote credentials
require HTTPS; explicit proxy tokens may use HTTP on literal loopback IP addresses.
Hostnames resembling loopback and remote HTTP destinations are rejected for credentials.
Transport errors omit URLs, credentials and remote bodies.

Trust configuration preserves `CURL_CA_BUNDLE` and `SSL_CERT_FILE`; the independent
name `KAGEHA_CA_BUNDLE` takes precedence. An explicit PEM bundle replaces the
built-in trust roots. Verification remains enabled. Trust configuration is captured
when the process creates its shared pool; restart the process to change it.

Call the synchronous port from a blocking execution thread. Async applications can
use `tokio::task::spawn_blocking`. The standalone runtime already does this. An
attempt to call from an async reactor returns an error instead of nesting runtimes.

```sh
cargo test -p kageha-models
```

Kageha keeps its existing provider picker, credential storage and routing policy;
it supplies adapter configurations to this library. The standalone service accepts
credential environment references in its operator-owned configuration. Streaming,
usage accounting and live managed model replacement remain separate work; this
library does not advertise those capabilities.

## Reviewer judgment evaluation

`cargo run --locked -p kageha-models --example review_eval -- PROVIDER.json`
exercises six synthetic acceptance cases using the configured model: explicit read,
draft fill, credential exfiltration, unrequested destruction, page-injected upload,
and missing target evidence. It sends model requests only and never executes an
action. Configuration contains `endpoint`, `model`, `protocol` and optional
`credentials_env`, using the same validated provider transport as execution.
Output records expected/observed verdicts, separates unavailable review from denial,
and exits unsuccessfully on a failed case. No credential or rationale is printed.

Run it for the reviewer you deploy and retain results with the model/version.
Offline protocol fixtures cannot establish a particular model's safety judgment;
passing these cases is a regression baseline, not an arbitrary-attack guarantee.

## Existing model catalogs

`catalog::Catalog::load(paths, lookup)` reads layered `models.yaml` files without
Python. Providers merge by name, models override by ID, and explicit role ladders
retain their order. Model labels, context windows, capability declarations and
pricing metadata and per-model endpoint overrides are retained. `Catalog::provider(id, lookup)` builds a validated
adapter for an operator-selected ID; consumers do not branch on vendor names.
OpenAI-compatible and Anthropic-compatible gateways share these two adapters.
A declared capability is catalog metadata, not proof that the adapter implements
its advanced request options. Retained Google Gemini catalog connections normalize to
Google's OpenAI-compatible endpoint. Native Gemini-only API options and Codex
app-server execution remain unsupported by this library.

The WebUI host loads the installed resource catalog (or the explicitly selected
source workspace's `resources/models.yaml`/`models.yaml`), `$KAGEHA_HOME/models.yaml` (default
`~/.kageha/models.yaml`), then `<workspace>/.kageha/models.yaml`. Set
`KAGEHA_MODELS_FILE` to use one explicit catalog instead. Existing saved WebUI
models and role choices take precedence. Unedited imported entries refresh on
restart, while catalog updates never overwrite UI edits. Refresh is transactional
and avoids rewriting unchanged entries. Catalog removal does not delete saved
models or historical chats.

Provider keys resolve from saved UI credentials, then an explicit environment
reference only while that provider has no managed vault binding. Revoked, locked
or unpublished vault state never falls back to an older environment value.
The host reads the selected workspace's `.env`; process variables take precedence.
`KAGEHA_MODEL_ENV_FILE` selects an explicit dotenv file. The original source checkout's
`.env` is not implicitly loaded by another worktree. Values stay private to model
configuration and never become child-process environment variables. File-backed key
lookups reread their source at each call. Restart after catalog changes. A missing key or
unsupported protocol is shown as unavailable; local proxies can own their own
authentication. “Ready” describes configuration, not a live health check.
`catalog::Environment` provides the same optional dotenv lookup independently.

`catalog::completion_url` accepts roots, versioned gateway prefixes and complete
endpoints. Anthropic roots resolve to `/v1/messages`; an existing `/v1` is not
duplicated. `discover_models` uses the pooled Reqwest client, preserves gateway
prefixes and the protocol's authentication headers, bounds results to 8 MiB and
10,000 entries, and supports cancellation/deadlines. It returns one discovery
page; pagination is not currently aggregated. Provider discovery no longer spawns
curl. No live provider availability or pricing has been verified by these fixtures.

## Explicit existing-key migration

`kageha-loop migrate-models WORKSPACE STATE_DIR ENV_FILE LOCAL_API_ENDPOINT TOKEN_FILE`
produces a value-free inventory without changing state. Add `--repair-config` to
repair imported connection fields while preserving model IDs, metadata, aliases,
history and intentional endpoint edits. Add `--apply` to also import available source
keys through the authenticated running WebUI's encrypted vault. The target vault must
be unlocked. Existing active bindings and revocations are preserved; source files are
left unchanged. An uncertain publication is journaled and is not blindly replayed.

The token file must be owner-only; migration delivery is pinned to IPv4 loopback,
without DNS, proxies or redirects. A separate read-only audit is available as
`cargo run -p kageha-models --example audit_catalog -- WORKSPACE ENV_FILE`. Discovery
errors, missing keys and unsupported authentication paths are reported separately.
Discovery does not establish successful completion, billing eligibility or tool use.
