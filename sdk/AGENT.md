# Agent SDK

The SDK calls the standalone Rust runtime API, independently of WebUI. All three
clients expose the same operations and portable protocol-v1 records:

| Operation | Rust | TypeScript | Python |
|---|---|---|---|
| Submit a run | `run(&spec)` | `run(spec)` | `run(spec)` |
| Inspect result | `status(id)` | `status(id)` | `status(id)` |
| Observe lifecycle | `events(id, after)` | `events(id, {after, signal})` | `events(id, after=0)` |
| Steer an active run | `steer(id, &message)` | `steer(id, message)` | `steer(id, message)` |
| Cancel server execution | `cancel(id)` | `cancel(id)` | `cancel(id)` |
| List approvals | `approvals(filter)` | `approvals(filter)` | `approvals(session=..., ...)` |
| Inspect approval | `approval(id)` | `approval(id)` | `approval(id)` |
| Submit reviewed decision | `decide(id, &decision)` | `decide(id, decision)` | `decide(id, decision)` |

Rust is async over Tokio, TypeScript uses promises/async iteration, and Python is
synchronous with a context-managed event iterator. TypeScript includes declarations;
Python includes TypedDict records and `py.typed`. These packages are local prerelease
SDKs, not published registry releases. Rust requires a local Cargo path dependency;
JavaScript consumers can install `sdk/typescript` as a local npm dependency. Python
is optional client code, never a backend requirement.

## Start the service

Build and configure `kageha-runtime-service` using
[`../crates/kageha-runtime-service/README.md`](../crates/kageha-runtime-service/README.md).
It owns the runtime/policy journals and configured model/tool implementations. The
SDK does not start WebUI, import its database or select model credentials through
run payloads. Use HTTPS outside loopback and keep the operator token on a trusted
server/device; do not ship it to untrusted browser pages or put it in model prompts.
Redirects are disabled. Responses and individual events are bounded at 32 MiB.

## Rust client and in-process embedding

```toml
[dependencies]
kageha-agent-sdk = { path = "../kageha/crates/kageha-agent-sdk" }
```

`AgentClient::new(endpoint, token)` builds a reusable pooled client. `RunSpec`,
`RunRecord`, `RunEvent`, `Steering`, `ApprovalRecord`, `ApprovalDecision`, `Authority`
and `GrantScope` are re-exported from the shared Rust contracts. Stream with:

```rust
let mut events = client.events(&run.spec.id, 0).await?;
while let Some(event) = events.next().await? {
    println!("{} {:?}", event.sequence, event.record.state);
}
```

Dropping/closing `Events` releases its connection. `last_sequence()` reports only
the last delivered event, so it is suitable for explicit reconnection. The working
CLI example exposes every SDK method:

```sh
cargo run --locked -p kageha-agent-sdk --example client -- \
  http://127.0.0.1:8787 ./runtime.token info < /dev/null
```

For **in-process embedding without an HTTP service**, run:

```sh
cargo run --locked -p kageha-agent-sdk --example embed
```

[`embed.rs`](../crates/kageha-agent-sdk/examples/embed.rs) constructs a durable
`ExecutionHost` from a runtime store, `ModelPort` and workspace `ToolPort`. It
executes a real file-reading tool turn, prints lifecycle records and drains its
workers. The default deterministic demonstration model needs no account/network.
To use an actual provider instead, set `KAGEHA_EXAMPLE_ENDPOINT`,
`KAGEHA_EXAMPLE_MODEL` and, when required, `KAGEHA_EXAMPLE_KEY` (HTTPS required for
credentials). Replace model/tools/store ports to embed it in another framework.
The example uses disposable state; applications must provide persistent directories.
Mutation-capable tools additionally need an explicit policy and approval port,
such as `ExecutionHost::with_journal`; the client SDK never supplies consent.

## TypeScript

```ts
import { AgentClient, type RunSpec } from "@kageha/client";

const client = new AgentClient(endpoint, operatorToken);
const id = crypto.randomUUID();
const spec: RunSpec = {
  id, session: "my-chat", turn: id, request: id, agent: "main",
  objective: "Say hello", parent: null, parent_run: null, depth: 0,
  input: {
    messages: [{ role: "user", content: "Say hello" }],
    max_model_calls: 8, timeout_ms: 60000,
  },
};
const run = await client.run(spec);
let after = 0;
for await (const event of client.events(run.spec.id, { after })) {
  after = event.sequence; // Persist after processing the event.
  console.log(event.record.state, event.record.completion);
}
```

Every method accepts an optional `AbortSignal`; `events` takes it in its options.
Breaking iteration aborts the stream connection. JavaScript cursors are restricted
to safe integers; the SDK rejects larger values rather than silently rounding them.
The implementation requires modern Node with fetch/AbortSignal.any (Node 22+ tested).
It has no npm runtime dependencies.

## Python

```sh
python3 -m pip install ./sdk/python
```

```python
from uuid import uuid4
from kageha_client import AgentClient, RunSpec

client = AgentClient(endpoint, operator_token)
id = str(uuid4())
spec: RunSpec = {
    "id": id, "session": "my-chat", "turn": id, "request": id,
    "agent": "main", "objective": "Say hello", "depth": 0,
    "parent": None, "parent_run": None,
    "input": {"messages": [{"role": "user", "content": "Say hello"}],
              "max_model_calls": 8, "timeout_ms": 60000},
}
run = client.run(spec)
with client.events(run["spec"]["id"]) as events:
    for event in events:
        print(event["sequence"], event["record"]["state"])
```

Use `with` or explicitly call `events.close()` when stopping early. No background
thread is required. Python 3.10+ is supported using the standard library. Its
configurable timeout applies to socket I/O; the run's execution deadline is the
server-side `input.timeout_ms`.

## Approvals, steering and recovery

List pending requests for the exact session, display their operation and authority,
then submit the user's explicit decision. For example, after reviewing a record:

```ts
const pending = await client.approval(approvalId);
await client.decide(pending.id, {
  authority: pending.request.authority,
  expected_revision: pending.revision,
  approved: userApproved,
  scope: "once",
});
await client.steer(runId, { id: crypto.randomUUID(), body: revisedDirection });
await client.cancel(runId);
```

Python accepts the same dictionaries. Rust accepts the corresponding contract
structs. Approval pagination returns `next_cursor`; pass it as `after`. This cursor
orders created approvals, not later changes; inspect individual records for updates.
Decisions preserve exact authority/revision. Stale decisions fail; no SDK silently
refreshes and approves a different operation. Broader grant scopes must be explicit.

Run/session/turn/request and steering IDs are caller-owned. Reuse identical run
payloads/IDs for exact retry; changing the same ID's input is rejected. The caller
supplies conversation messages on each turn. This SDK does not automatically load
Kageha chat history or claim task completion from an HTTP acknowledgement.

Events contain **durable run lifecycle records**, not token deltas, browser previews
or the full tool trace. Their sequence is increasing but may contain gaps. Resume
explicitly with the last processed sequence. Truncated/malformed/out-of-order frames
raise errors instead of advancing the cursor. Authentication and transport errors
include structured status/code when available and do not expose request headers.
HTTP mutations, decisions and steering are never automatically retried. After a
connection loss/timeout, inspect status before deciding what to retry. A stream
ending does not itself prove task success: inspect `state` and `completion`.

Closing a local request or event stream does **not** cancel its server run. Use
`cancel` explicitly. Interrupted post-crash runs remain interrupted; clients do not
replay tools to manufacture success. This SDK does not imply live runtime replacement
or full feature parity with every WebUI route.

## Acceptance

```sh
scripts/check_agent_sdk.sh
# Optional client-only lane:
scripts/check_python_sdk.sh
```

This builds the SDK/examples and standalone runtime, type-checks TypeScript, tests
the Rust/TypeScript clients and runs the same flows against a real Rust service: exact retries,
conflicts, approval allow/deny/stale authority, isolation, event close/resume, steering,
cancellation, actual host death and a second conversation turn after restart. The
backend runs with an empty executable PATH. Node is the disposable model/test driver;
The separate Python lane uses Python only to test the optional client. The embedding example also
runs independently with no HTTP server. Backend-only component gates exercise the
Rust/TypeScript clients without requiring Python.
