# Standalone Rust runtime service

This package hosts `kageha-runtime` execution and supervision with replaceable
`ModelPort`, `ToolPort` and `RunStore` implementations. The library and executable
have no dependency on Kageha's application server, application database, frontend,
Node, Python or a browser process.

## Optional remote execution

An operator-owned top-level `remote` setting composes the independent
`kageha-execution` library:

```json
{
  "remote": {
    "id": "development", "host": "dev.example.com", "user": "operator",
    "port": 22, "workdir": "/srv/workspace",
    "identity": "/absolute/private/id_ed25519",
    "known_hosts": "/absolute/private/known_hosts"
  },
  "approval_tools": ["remote_exec", "remote_upload"]
}
```

Merge these settings with the actual provider configuration below. OpenSSH and
the remote utilities listed in `../kageha-execution/README.md` are explicit runtime
dependencies. Strict host-key verification is the default. `--workspace` supplies
the local upload source; `read_file` then addresses the configured remote workspace.
An explicitly configured document worker continues reading local documents.
The model cannot change the target or authorize mutations through tool arguments.

The host owns `STATE/execution.sqlite`, separate from runtime/policy state. It
merges the authenticated `/v1/execution/receipts` inspection and acknowledgement
APIs. Unknown outcomes survive host restart and prevent fresh-ID retries on the
same account until an explicit operator continuation decision. The original action
is never replayed by that decision. `tests/execution_e2e.mjs` verifies approvals,
exact-byte uploads, environment scrubbing, cancellation and real host crash recovery
with an empty backend PATH using a disposable SSH transport fixture.

Kageha's existing WebUI backend uses the **same execution engine** with its existing
approval, browser, artifact, specialist and workspace adapters. This independent
executable supplies an explicitly read-only workspace tool adapter. It does not
silently launch Kageha to obtain additional tools.

## Start

```sh
cargo build --locked -p kageha-runtime-service
mkdir -p runtime-state workspace
openssl rand -base64 32 > runtime.token
chmod 600 runtime.token
```

Write `runtime.json` using an operator-selected model endpoint:

```json
{
  "providers": [{
    "endpoint": "https://model.example.com/v1/chat/completions",
    "protocol": "openai_compat",
    "model": "configured-model",
    "credentials_env": "MODEL_API_KEY"
  }]
}
```

`anthropic_compat` is also supported. Providers are attempted in configured order.
A credential-free local endpoint can omit `credentials_env`. Credentials are never
supplied in run payloads or returned by the API.

```sh
./target/debug/kageha-runtime-service \
  --state ./runtime-state --workspace ./workspace \
  --config ./runtime.json --token-file ./runtime.token \
  --bind 127.0.0.1:8787
```

OpenSSL above is a token-generation convenience, not a backend dependency. To
expose the service beyond the device, use authenticated HTTPS hosting/reverse proxy
and persistent storage. This is a single-tenant, single-writer host.

## Protocol

Every route requires `Authorization: Bearer TOKEN`.

| Route | Purpose |
|---|---|
| `GET /health`, `GET /v1/info` | Health, protocol and honest capabilities |
| `POST /v1/runs` | Durably admit and activate an idempotent run |
| `GET /v1/runs/{id}` | State, revision and completion receipt |
| `POST /v1/runs/{id}/steer` | Queue `{ "id": "stable-id", "body": "direction" }` |
| `POST /v1/runs/{id}/cancel` | Durable cancellation and cooperative interruption |
| `GET /v1/runs/{id}/events` | Ordered, resumable lifecycle SSE |
| `GET /v1/runs/{id}/receipts` | Late obsolete execution receipts |
| `GET /v1/approvals` | Bounded approval listing; filter by session/turn/pending |
| `GET /v1/approvals/{id}` | Current decision and one-time consumption state |
| `POST /v1/approvals/{id}/decision` | Exact authority and revision-scoped decision |

An execution request is the portable `RunSpec`:

```json
{
  "id": "request-1", "session": "chat-1", "turn": "chat-1-turn-1",
  "request": "request-1", "agent": "main", "objective": "Read my notes",
  "parent": null, "parent_run": null, "depth": 0,
  "input": {
    "messages": [{"role": "user", "content": "Read notes.txt"}],
    "max_model_calls": 8, "timeout_ms": 120000
  }
}
```

The caller supplies conversation context for each turn. Run and turn IDs are
stable and unique within the service journal. Reusing an ID with different input
is rejected. Callers cannot override the workspace, endpoint, credentials or tool
binding through execution input. Existing Kageha chat history stays managed by its
compatibility backend; this service does not reinterpret its database schema.

SSE sends `run` events with a durable sequence and `RunRecord`. Resume with
`Last-Event-ID` or `?after=SEQUENCE`. Streams are bounded and preserve per-run
ordering. Both service requests and execution admission have explicit limits.

SIGTERM/Ctrl-C stop admission, persist cancellation and drain cooperative workers.
On restart, committed completion receipts are restored without dispatching again;
admitted executions with unknown outcomes become interrupted. Potentially
previously dispatched tools are never automatically replayed.

## Embed or supply another adapter

Construct `ExecutionHost::new(store, model, tools, limits)` with implementations of
`RunStore`, `ModelPort + Send + Sync`, and `ToolPort + Send + Sync`. The same
supervisor runs it embedded or behind `http::router`. No vendor branches belong in
the caller. `src/lib.rs` tests use two independently supplied tool adapters and
recover a committed result before publication without invoking either adapter.

Tool adapters retain responsibility for domain permissions, resource authority and
mutation receipts. The default `ReadWorkspace` only exposes bounded UTF-8 reads
inside its configured root. Kageha's approved browser input remains in its existing
adapter. Broad remote tool hosting and approval/device protocols are not implied
by a `ToolPort` implementation.

```sh
cargo test --locked -p kageha-runtime-service -p kageha-runtime-store
cargo build --locked -p kageha-runtime-service -p kageha-loop
node crates/kageha-runtime-service/tests/service_e2e.mjs
```

The end-to-end test runs both backends with an empty executable PATH and compares
the same model/tool outcome. It also checks idempotency, isolation, authenticated
SSE replay, steering, deadlines, cancellation, actual process death, retained WebUI
history and graceful shutdown. Node is only the disposable test client/model fixture.

The current service bindings are `models.default` and `tools.workspace`, generation
1. `live_replacement` is explicitly false. Managed replacement of models, runtime
and tools, monetary budgets, streaming model protocols, deployment bundles and full
retained-feature parity remain release work.

## Policy composition

The default host applies the shared `PolicyPort` rules before dispatch. It checks
that the tool is actually exposed by the configured binding. Read-only workspace
reads remain available; unknown operations are denied. For another trusted tool
adapter, construct `ExecutionHost::with_policy` with explicit policy and optional
approval ports. Confirmed receipts can be inspected/reused without new approval
or dispatch; uncertain inputs cannot be replayed. The executable now attaches a component-owned policy journal through
`ExecutionHost::with_journal`. Remote approval list/status/decision APIs operate on
that journal. Custom embedded hosts can still provide another approval port.

## Require remote approval

Add an operator-owned setting alongside `providers` in the configuration:

```json
{ "approval_tools": ["read_file"] }
```

Keep the real provider entries from the earlier example. The server validates
that every configured tool exists. A model cannot override this setting. Without
this option, the default read-only workspace behavior stays unchanged.

Poll `GET /v1/approvals?turn=TURN` for pending requests. Each record contains the
reviewable operation, exact authority and revision. Decide with:

```json
{
  "expected_revision": 1,
  "authority": {
    "session": "chat-1", "turn": "chat-1-turn-1",
    "binding": "tools.workspace", "generation": 1, "resource": "notes.txt",
    "input_digest": "0000000000000000000000000000000000000000000000000000000000000000"
  },
  "approved": true,
  "scope": "once"
}
```

Use the complete authority object from the returned record unchanged; the values
above illustrate its shape. Send the resulting payload to `/v1/approvals/ID/decision`. Set `approved: false` to deny. The
backend checks the active run, revision and every authority field, then consumes
the decision once before input. Contradictory retries cannot overwrite a decision.
An identical successful retry returns its recorded outcome without new input.

`GET /v1/approvals` accepts `session`, `turn`, `pending` (default true) and an
`after` creation cursor. The cursor paginates created records; fetch an approval's
status to follow later changes. This is not a policy change-feed cursor. The
owned policy database and audit stay in `STATE/policy.sqlite`, separate from the
runtime journal. Pending/unconsumed decisions are interrupted at restart.

`node crates/kageha-runtime-service/tests/approvals_e2e.mjs` checks remote approval,
denial, revision/authority fencing, cancellation, expiry and real crash recovery
with no Python backend.

## Chat-scoped permission profiles

`GET /v1/permissions?session=CHAT` reads the explicit chat profile (default `ask`).
`POST /v1/permissions` accepts `{ "session": "CHAT", "profile": "auto_review" }`.
Available values are `ask`, `auto_review`, `full`. Full access additionally requires
`"confirm_full": true`. These endpoints use the same operator authentication as
run and approval APIs. Profile changes cancel that chat's outstanding decisions.

For operations requiring approval, automatic review uses a separate model call,
no tools, exact request identity and the run objective. Errors or uncertainty leave
a pending human request available through the existing approval API. Full access
uses a durable one-time decision without invoking the reviewer. Configured tool
exposure, workspace limits, explicit policy denies, deadlines and receipt recovery
still apply. The default executable still exposes only its bounded workspace reader.

These are tool permission profiles; `/v1/permissions` explicitly reports
`os_sandbox: false`. They do not disable a sandbox or provide unrestricted host-file
access. WebUI compatibility uses `/api/permissions` with `session_id` and `profile`
and preserves the retained legacy mode API separately.

## Independent MCP tools

Optional `mcp_servers` extends the workspace catalog through the shared independent
`kageha-mcp` adapter and `kageha-tools` composite. For example:

```json
{
  "providers": [{"endpoint":"https://model.example.com/v1/chat/completions","protocol":"openai_compat","model":"configured-model","credentials_env":"MODEL_API_KEY"}],
  "mcp_servers": [
    {"id":"local","transport":"stdio","command":"/absolute/path/to/server","args":[],"env_refs":{"SERVER_TOKEN":"MCP_SERVER_TOKEN"}},
    {"id":"remote","transport":"streamable_http","url":"https://tools.example.com/mcp","header_refs":{"Authorization":"MCP_AUTHORIZATION"}}
  ]
}
```

References name operator environment variables; header references contain the full
header value. Stdio servers receive a minimal environment plus configured values.
All MCP calls require approval by default. An operator can explicitly classify
safe operations with `read_only_tools`; remote annotations do not make that choice.
The existing policy profile and remote approval APIs govern these tools as well.
Approvals include server binding, generation, resource and configuration fingerprint.
The MCP journal lives in `STATE/mcp.sqlite`, separately from runtime and policy.

`GET /v1/tools` returns the configured catalog. After its runs stop,
`POST /v1/sessions/{session}/tools/release` releases that session's connections;
uncertain receipts are retained. The session-scoped `mcp_receipts` tool inspects
uncertainty. Changing configuration requires restarting this fixed-generation host;
managed live replacement is still a separate milestone. Existing WebUI adapters
continue supporting the broader application tool catalog.

## Parallel tool execution

The shared Rust engine runs up to four eligible tools per batch with rolling
admission and deterministic result order. Adapters opt in through `DispatchAccess`;
unknown tools remain exclusive barriers. Workspace reads can overlap. MCP calls
on distinct servers can overlap; one server/session connection stays ordered.
Each call retains its own approval, binding revalidation and durable receipt.
An approval wait does not prevent independent calls in the batch from finishing.
Cancellation stops admission and drains already admitted calls cooperatively.

The retained WebUI uses this same scheduler, including independent specialist
spawns and `agent_wait_any`. The standalone host retains its configured tool
catalog; it does not yet expose all WebUI specialist management tools.

## Logical memory tools and live replacement

Optional memory composition adds the same private memory tool adapter used by the
WebUI. For example, add this top-level property alongside `providers`:

```json
{
  "memory": {
    "initial": "local",
    "providers": [
      {"id":"local","adapter":"sqlite"},
      {"id":"alternative","adapter":"memory-http","endpoint":"https://memory.example.com","credentials_env":"MEMORY_ACCESS_TOKEN"}
    ]
  }
}
```

SQLite implementations own separate files under `STATE/memory`; the registry owns
`registry.sqlite`. HTTP credentials are operator environment references, not tool
arguments. Initial configuration does not override a persisted binding. Required
capabilities and implementation identity are checked at startup. Registered
alternatives are prepared destinations; ordinary work is routed only to the active
logical binding. Operator-selected destinations require an explicit transfer
approval before replacement.

The service adds the existing registry protocol on the same authenticated server:
`GET /v1/bindings`, `GET /v1/providers`, `POST /v1/replacements`, replacement status
and resume routes, resumable `/v1/events`, and operator `/v1/calls`. The provider
catalog exposes implementation descriptors needed to approve an exact destination;
it does not expose credentials. Memory SDK clients can use the same protocol.
Mutation approvals remain available through `/v1/approvals`.

Memory tools derive a workspace namespace from the canonical workspace and a
private owner from the session ID. They cannot change those scopes or initiate
replacement. Existing WebUI composition keeps its retained workspace/specialist
scope. The caller supplies standalone conversation context each turn, as before.

`live_replacement:false` in runtime information still describes **the runtime**.
Memory replacement is managed by the independent registry. Other tool/model/runtime
replacement remains incomplete. No new Python or Node backend is required.

`GET /v1/review-denials?session=CHAT` returns recent denied records and rationale.
`POST /v1/review-overrides` accepts `approval_id`, the complete original `authority`
and `expected_revision`. It returns `input_dispatched: false`: the next exact action
still needs independent review. No tool call is replayed by this endpoint.
The WebUI uses the equivalent `/api/review-overrides` endpoint and exposes denial
history through `/api/permissions?session_id=CHAT`.

Managed requirements, denial stops, delegated scopes and process sandbox setup are
documented in `kageha-policy/README.md` and `kageha-sandbox/README.md`. These policies
apply in standalone hosting as well as WebUI composition. Child runs must reference
a matching parent run before inheriting the parent's permission scope.

## Independent workspace and document worker

The default `read_file` adapter now comes from the public `kageha-workspace` library.
It keeps the workspace boundary and 64 KiB text limit. To additionally expose
`read_document`, build the independent worker and add this top-level setting:

```json
{"workspace_worker":"/absolute/path/to/kageha-workspace"}
```

The host selects that executable; tool arguments can supply only a path within the
configured workspace. The worker receives an already-captured byte snapshot on
stdin. It does not reopen the source path or use the Kageha application server.
Without this setting, the document capability is absent from discovery. Add
`read_document` to `approval_tools` if operator approval is required for these reads.
Both routes validate the file identity approved by the host before reading.

`tests/workspace_e2e.mjs` uses an empty backend PATH and proves standalone CLI/read
parity, document extraction, approvals, path containment, replacement fencing and
restart without a Python or Node backend.

### Catalog-selected model adapters

The standalone host can consume the same layered model configuration as WebUI,
without importing its database or depending on Python:

```json
{
  "catalog_files": ["/etc/kageha/models.yaml", "/etc/kageha/models-local.yaml"],
  "catalog_models": ["primary-model-id", "fallback-model-id"]
}
```

Paths and selected IDs are operator-owned. The ordered selection becomes the
fallback chain, followed by any explicit `providers` entries. Credentials resolve
from each provider's environment reference; native protocols without a Rust
adapter are rejected. The standalone host does not implicitly read the WebUI
credential database or dotenv files. Library consumers may provide their own
lookup, including `catalog::Environment`, without changing runtime consumers.

Optional `browser: {"state_dir":"/absolute/private/browser-state"}` composes the
independent typed browser adapter. Launch the Rust browser daemon separately with
standalone authority; the runtime does not launch a browser service or copy profile
cookies. See `../kageha-tools/README.md` for scope, consent, current tool capabilities
and the real Chromium `tests/browser_e2e.mjs` acceptance fixture. The existing WebUI
still owns profile selection, preview presentation and uploaded-file ownership.

## Current WebUI with a separate turn process

The client-port bridge runs the same turn engine in an independent Rust process.
Kageha supplies model, tool, policy and steering adapters over authenticated,
run-scoped requests; credentials and existing chat storage stay with the
application host. The daemon owns its own run/dispatch journal. It does not import
WebUI code or connect to the application database. Existing embedded deployments
remain supported; a configured bridge never silently falls back to embedded execution.

```sh
kageha-runtime-service bridge --state ./runtime-state \
  --token-file ./runtime.token --bind 127.0.0.1:8792
export KAGEHA_RUNTIME_SERVICE_URL=http://127.0.0.1:8792
export KAGEHA_RUNTIME_SERVICE_TOKEN_FILE="$PWD/runtime.token"
export KAGEHA_WEBUI_DIST="$PWD/frontend/dist"
kageha-loop serve ./workspace ./existing-application-state 0.0.0.0:8788
```

Use the existing application state directory and server authentication token to
retain chats, settings, attachments and browser authority. Back up that state
before upgrades. The token file must be private, at least 32 characters, and shared
only with trusted application adapters. Non-loopback bridge connections require
HTTPS. Authenticated application `/health` reports runtime mode, connection and
separate process identity. This is not a model-configured service: `bridge` receives
its model/tool implementation through the caller's ports.

The bridge protocol exposes authenticated `/v1/info`, run admission/status/cancel,
and `/v1/runs/{id}/commands` with per-command claim and reply endpoints. Use the
Rust `bridge_client::Client` to provide `ModelPort`/`ToolPort` implementations.
Claim intents are durable before dispatch. Reply publication may retry; the
adapter invocation itself may not. Completed replies are idempotent and conflicting
replies are rejected. On crash, unknown claimed operations remain interrupted;
committed completion receipts are restored without executing again. Host loss
expires the execution lease; cancellation remains cooperative at adapter boundaries.
This bridge is a trusted single-tenant adapter protocol, not an untrusted plugin sandbox.

The WebUI admits turns with `event_stream: "session"` and then subscribes once to
`/api/sessions/{id}/live/stream?after_sequence=N`. Snapshot, tool cards, approvals,
worker lifecycle and child activity share a durable ordered journal. Worker and
spawn-call status remain separate. A parent finishing does not close the worker
feed; reconnect and history reads never admit another execution. Legacy event
routes remain compatible. Streams have bounded connections/replay pages, and
slow/disconnected clients reconnect from their last processed sequence.

```sh
node crates/kageha-runtime-service/tests/bridge_recovery_e2e.mjs
node crates/kageha-loop/tests/standalone_runtime_ui_e2e.mjs
```

These use disposable model/tool fixtures and run Rust backend processes with an
empty executable PATH. They cover the retained WebUI contracts, concurrent workers,
cancellation, session replay/restart, and crashes between dispatch, receipt and
completion publication. They are correctness evidence, not live-provider latency
benchmarks. Research evidence and its limits are recorded in
`../kageha-runtime/research/subagent-ui-integration.md`.

## Independent specialist workers and workspace ports

The standalone executable now exposes `agent_spawn`, `agent_status`, `agent_cancel`
and `agent_wait_any` through the existing model/tool engine. Server configuration
selects specialist identities, instructions and tool allowlists:

```json
{
  "specialists": [
    {"id": "worker", "instructions": "Complete the delegated objective."},
    {"id": "reader", "instructions": "Read the requested files.",
     "allowed_tools": ["read_file", "read_document"]}
  ],
  "git_binary": "/usr/bin/git"
}
```

Merge these fields with a working provider configuration. If the catalog is omitted,
a `worker` specialist uses the configured tools under its parent's permissions.
Nested tool allowlists intersect; child configuration cannot widen the parent scope.
Worker catalog names are included in tool discovery. Only the owning parent can
inspect, wait for or cancel a worker through model tools.

`agent_spawn` accepts `isolation: "worktree"`, `keep_worktree` and `base_ref`. The
shared `kageha-workspace::worker::Manager` supplies the same journaled creation,
unchanged-checkout cleanup, cancellation-aware queue and recovery rules used by
Kageha's existing WebUI adapter. File and optional document tools use the checkout;
other configured integrations retain their own authority. Isolation is a workspace
boundary, not an OS process sandbox. Local Git isolation is rejected for a remote
execution composition and for repository-subdirectory roots.

Library hosts call `ExecutionHost::with_workers`. `Composition::with_workspace`
accepts a replaceable `WorkspacePort`; `LocalWorkspace` is the default Git adapter.
The port owns binding identity, validation, preparation, observation, tools and
settlement. The coordinator does not depend on a workspace database or filesystem
implementation. Conformance tests run the same engine with two virtual workspace
adapters and verify that a changed binding cannot dispatch after authorization.
Live workspace replacement remains a separate release requirement.

Run the fully Rust embedding example:

```sh
cargo run --locked -p kageha-runtime-service --example workers
```

It uses a disposable Git repository and local model/tool ports, with no HTTP host,
WebUI, Python or Node. Git is an explicit native dependency of the default adapter.

The authenticated service API adds:

- `GET /v1/workers/catalog` — configured specialist definitions.
- `GET /v1/runs/{id}/workspace` — current workspace and lifecycle receipts.
- `POST /v1/runs/{id}/workspace/reconcile` — observation-only receipt recovery.
- `POST /v1/runs/{id}/workspace/cleanup` with `{"confirmed":true}` — explicit
  unchanged-checkout cleanup after all attempts using it have stopped.
- `POST /v1/runs/{id}/resume` with
  `{"command_id":"stable-user-command","confirmed":true}` — continue a stopped
  run from its last confirmed checkpoint. The response contains the new run ID.

Continuation preserves the original workspace identity, specialist policy, tool
results and available active ancestry. Each stopped attempt has one idempotent
continuation reservation; another command cannot create a parallel duplicate.
A batch without a confirmed result checkpoint cannot resume. Confirmed mutations
are matched to their original adapter authority, so inventing a fresh call ID cannot
repeat them. A different profile or generation is not blanket-blocked by that guard.
Read tools may take new observations. Legacy receipts without authority metadata
are handled conservatively; missing checkpoints require an explicit new assignment.

Source workspace identity is pinned at admission. Configuration cannot silently
redirect existing runs to another source. The executable holds an exclusive state
lease; embedding hosts must provide their own single-writer supervision. Per-run
model-call/deadline budgets and depth/queue limits apply. A shared monetary budget
across the complete worker family still needs its retained-feature implementation.

`scripts/check_standalone_workers.sh` runs library/embedding and real-service
acceptance, including actual process loss, authority changes, unknown batches,
fresh-ID mutation attempts, document roots and the existing WebUI worker scenarios.
Remaining lifecycle events, full planner/persona/settings parity, managed replacement
and release-candidate deployment validation remain outside this scoped completion.

The authenticated low-level run API retains generic child-run submission with
caller-supplied messages. Named catalogs constrain model-driven delegation; generic
children still inherit their parent's approvals and tool restrictions. Registered
specialist identities retain their pinned policy even through low-level submission.
A workspace adapter that accepts isolation but returns a shared scope or no scoped
tools is rejected before model/tool execution.

### Lifecycle hooks without a WebUI

`hooks::Lifecycle` composes the independent `kageha-execution::hooks` executor
with the existing policy, model/tool and run-store ports. Library consumers use
`ExecutionHost::with_lifecycle`; no HTTP server or application database is required.
The standalone executable enables this composition directly. Optional `hook_files`
contains explicit absolute global configuration paths; the workspace's
`.kageha/hooks.json` is loaded last. Library consumers do not implicitly read HOME.

Supported events are `sessionStart`, `preToolUse`, `beforeShell` (bash/remote_exec)
and `postToolUse`. These are host operations, not advertised model tools.
Command/HTTP hooks require the normal exact `project_hook` policy decision, including
configuration, destination, payload digest and execution-root identity. Approval
waiting does not consume the hook's execution timeout. Commands run in the actual
checkout with a scrubbed environment; local shell hooks are unavailable in a remote
execution composition. HTTP uses pooled, bounded, cancellable Rust transport.

Configuration is captured in the run journal. Standalone dispatch fences changed
configuration before and after approval; continuation requires the same captured
configuration and reuses its checkout. Pre-hook denials stop the turn. A confirmed
base tool result is published before any post hook, so a post-hook failure cannot
erase it or cause an automatic replay. Existing pre-hook service continuations can
adopt an empty configuration; enabling hooks on such a continuation requires a new
assignment. Existing WebUI turns retain their immutable configuration until the next
turn, through the same executor's explicitly captured-snapshot API.

Authenticated operator APIs:

- `GET /v1/runs/{id}/hooks/receipts` inspects that run's receipts.
- `POST /v1/runs/{id}/hooks/acknowledge` accepts `receipt`, `target` and
  `confirmed: true` after the run stops. This acknowledges an unknown outcome;
  it neither verifies nor replays the historical operation.

`scripts/check_standalone_hooks.sh` exercises independent embedding, the real
standalone binary and existing WebUI flows with no Python backend. Other lifecycle
events, plugin hook formats and managed service replacement remain separate gates.

### Reviewed planning

Standalone hosting directly composes `planning::Coordinator` over the independent
`kageha-workflow::planning` store/evidence ports. Embedding hosts can provide it with
`ExecutionHost::with_components` and `Components`; existing constructors remain
compatible. A normal run is unchanged. Request planning through the existing SDK's
run input, for example:

```json
{"messages":[{"role":"user","content":"Inspect the result"}],
 "planning":{"required_outcomes":[{"id":"result","text":"Deliver evidence-backed result"}]},
 "max_model_calls":16}
```

Supply `planning.document` to review an already prepared plan instead of generating
one. Documents contain `objective`, `steps` (`id`, `title`, `depends_on`) and optional
`outcomes` (`id`, `text`, `required`). Caller-required outcomes are pinned independently
of model output. Planner calls consume the run's model-call budget. Generated
approval/completion claims are reset. The host waits for an exact `approve_plan`
decision before task execution; the approval displays the full plan and binds its
revision and digest. Changes while waiting require a fresh decision.

`GET /v1/runs/{id}/plan` returns the scoped plan and completion state. Before approval,
`POST` to the same route accepts `command_id`, `expected_revision` and `document`.
Required outcomes cannot be removed; exact retries are idempotent and stale writes
cannot overwrite newer state. Approved designs are immutable.

Approved planned runs expose `task_plan_read` and `task_plan_update` subject to the
specialist's inherited allowlist. Updates record local plan metadata and require
`expected_revision`; completion requires successful non-planning/non-delegation
receipt IDs from the assignment. `outcome_ids` identifies satisfied outcomes.
A premature final answer produces a bounded repair step through the ordinary turn
engine, consuming the same call/deadline budget. Repeated final claims without
progress stop; they cannot publish success with unfinished required outcomes.

Confirmed continuation reuses the approved plan and prior receipt facts. Unknown
batch outcomes remain blockers. `scripts/check_standalone_planning.sh` includes a
separate fixture that composes the ordinary runtime with a pausing `RunStore`, then
kills that process after plan commit and before receipt publication. Restart keeps
the plan fact, interrupts the run and never replays the unpublished call. No fault
flag or special replay path is added to the production launcher.

This component does not establish complete persona/settings parity, all historical
task-state formats, semantic outcome verification, family-wide monetary budgets,
managed service replacement or production installation readiness.

### Portable specialist profiles

The optional `agent_profiles` launcher setting accepts validated
`kageha-agents::Definition` documents (`id`, `name`, `system_prompt`, `tool_policy`).
Embedding hosts can supply any `kageha_agents::Port` through
`workers::Composition::new_with_profiles`. Legacy `specialists` still work, but
identities cannot overlap between catalogs.

Profiles pin their complete configuration at admission. Wildcard allow/deny rules
intersect with ancestor policy; risk overrides only raise approval requirements,
including planning tools. Direct registered-agent runs receive their instructions,
as do delegated children. Changing a profile while approval is pending prevents
execution under the obsolete authority. Unrelated agents remain usable.

Approval risk is separate from mutation identity. A reviewed read can provide a
fresh observation on continuation; a reviewed mutation still retains its original
replay fence. Confirmed receipts store both identities, with legacy receipt fallback.

The public worker catalog reports effective settings and activation failures.
Configure the standalone launcher with paths to existing component stores:

```json
{
  "skill_registry": "/data/skills/registry.sqlite",
  "profile_knowledge": {
    "path": "/data/knowledge/store.sqlite",
    "workspace": "workspace-A",
    "bases": ["runbooks", "references"],
    "selected": ["runbooks"]
  }
}
```

Profiles can pin declarative skills and restrict context to those pins. Skills
marked as disabled for model invocation are omitted. A pinned skill that needs
a helper runtime blocks admission. Knowledge excerpts come from lexical search
against the profile objective. The operator's `bases` list bounds access.
`selected` defaults to that list, and a root profile can add authorized bases
through `extra_kb_ids`. Delegation retains at most the parent's scope.

Nonempty `model_preferences.planner` and `model_preferences.executor` select
a configured catalog ID or a unique chat adapter model name. An explicit route
uses that adapter's configured transport and credential authority.
`model_preferences.fast_worker` selects the chat adapter used by automatic tool
review. It receives the review request when automatic review applies.
`model_preferences.embedding` selects a unique explicit provider with
`protocol: "openai_embeddings"` and an embedding endpoint. With profile knowledge
configured, that endpoint receives the objective and bounded excerpts from up
to 20 authorized lexical candidates. Cosine similarity reranks them to five
results. This path does not maintain a vector index. Missing, ambiguous or
unsupported choices block admission. The admitted
resource pin records skill versions, named model bindings and knowledge revisions.
Continuation checks those bindings before sending a model request. Profiles with
admitted skills can use `read_skill_resource` with a skill name and package-relative
path. The tool checks the admitted revision and package digest when reading UTF-8
content, and follows the run’s inherited tool policy.

For durable profiles, import an exported bundle with the independent
`kageha-agent-profiles` executable and replace inline `agent_profiles` with:

```json
{"agent_repository":{"path":"/data/profiles.sqlite","scope":"workspace-A"}}
```

The scope must match the repository. Inline and persistent profile configurations
cannot be combined; legacy `specialists` can still coexist with disjoint identities.
The runtime leases the owned store until shutdown. Restart retains definitions,
revision fingerprints and prior run fences. A second offline writer is rejected.
The existing WebUI database is never opened by this component. Exporting/importing
its settings is a separate, explicit migration operation.
