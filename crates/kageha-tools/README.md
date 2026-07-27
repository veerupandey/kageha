# Framework-independent Rust tool adapters

`Composite` joins tool ports and rejects ambiguous names. Discovery, execution
context, authority metadata, authorized dispatch and scheduling hints are delegated
to the adapter that owns each tool. The package does not import the application,
UI, a vendor client or another component's database connection.

`memory::MemoryTools` implements retained `memory_put`, `memory_get`,
`memory_search` and `memory_delete` operations over the public `LogicalMemoryPort`.
`kageha-services::Registry` supplies that port, keeping calls behind `memory.default`
while implementations change. The adapter holds no SQLite or HTTP vendor client.
Baseline capability validation rejects incomplete implementations before activation.

A trusted host selects either fixed `MemoryAccess` (the existing WebUI's workspace
and specialist identity) or session-private access (standalone runtime). Session
owners are stable SHA-256-derived IDs, exposed by `session_actor`. Tool arguments
cannot select another owner, namespace, backend, ACL, export/import operation or
replacement destination. Existing WebUI namespaces, owner IDs and mutation request
ID derivation are retained, preserving old data and deduplication receipts.

The registry serializes logical writes within each private scope. Reads and
independent scopes remain usable while another scope is waiting. Cancellation or
process death turns unresolved writes into durable uncertainty. Further tool
mutations in that scope stop; reads remain available. Only an explicit operator
reconciliation of the original idempotent request can resolve that uncertainty.
Agent tools cannot clear it or automatically replay the request.

Approvals pin the logical binding generation. `execute_authorized_in` carries that
approved authority into dispatch, and the registry atomically checks the generation
when admitting the call. A concurrent cutover therefore rejects obsolete authority
before writing to the replacement. A caller must obtain fresh authority to retry;
an old approved operation is never silently redirected. Known data/conflict errors
stay distinct from ambiguous write failures.

Synchronous execution has bounded cancellation/deadline handling and a shared Tokio
executor. `MemoryTools` can be embedded by another framework without the Kageha
runtime, server, application database, Node or Python. Its outputs use the same
`Routed` receipts and `ToolReply` contract as the existing SDK/service protocols.

```sh
cargo test --locked -p kageha-tools -p kageha-services
cargo build --locked -p kageha-runtime-service -p kageha-memory
node crates/kageha-runtime-service/tests/memory_e2e.mjs
```

The last command uses Node only as a test driver/model fixture. It keeps one run
alive through a SQLite-to-HTTP replacement and checks mutation approvals, subsequent
reads/writes/search/deletion, private scope isolation, provenance and restart.
This extraction does not complete memory knowledge/peer semantics, semantic/graph
retrieval, legacy memory import or managed replacement of other components.

## Browser adapter and shared native projection

`browser::BrowserTools` composes the standalone browser daemon through the public
`kageha-browser-client` library. It exposes managed tab listing/selection, HTTP(S)
navigation, full snapshots, pending/exact receipts and observed click/fill/key/
select/check operations. Calls use `browser.workspace` authority and exclusive
scheduling. Configuration names only an absolute private daemon state directory:

```json
{"browser":{"state_dir":"/absolute/private/browser-state"}}
```

Add that field to the standalone runtime configuration. Its normal approval APIs,
permission profiles, cancellation and effect journal continue to apply. Inputs
cannot override workspace/client identity, native grants, service epoch, profile
endpoints, script code or file paths. Independent runtime session IDs map to stable
`runtime-<SHA256(session)>` browser workspaces, avoiding accidental adoption of a
WebUI chat’s browser scope.

Approval pins the service epoch, selected binding/browser/tab/URL, caller scope,
configuration root and exact arguments. The numerical epoch marker fits JavaScript
integers; the full epoch is also hashed and retained in the authority resource.
It is not a monotonic managed replacement generation. Selection changes reject
pending authority; transport ambiguity becomes a typed uncertain stop rather than
an ordinary error the agent can route around. Returned attempt IDs support exact
receipt inspection. Opening a tab selects it explicitly. Empty private profiles can
start a private browser; signed-in profiles must first be connected by the operator.
The adapter never forces takeover of another client’s control lease.

`observed_action` is the common native argument projection used by both this adapter
and Kageha’s existing WebUI tool host. The WebUI keeps its chat-owned uploads,
credential handling, profile picker, preview acknowledgment, input refresh and
approval UI. The shared projection preserves frame identity and the native key
operation’s requirement to omit a target argument.

The standalone adapter does not advertise scripts, uploads, credentials, profile
switching, preview display acknowledgment or reconciliation yet. Their existing
WebUI paths remain working; moving those ownership-sensitive host contracts is
required before claiming full standalone browser parity. Neither a snapshot nor a
verified input receipt proves task completion or that a UI painted the page.

```sh
cargo test --locked -p kageha-browser-client -p kageha-tools
cargo build --locked -p kageha-loop -p kageha-runtime-service
KAGEHA_BROWSER_BIN="$PWD/target/debug/kageha-browser" \
  node crates/kageha-runtime-service/tests/browser_e2e.mjs
KAGEHA_LOOP_BIN="$PWD/target/debug/kageha-loop" \
  KAGEHA_BROWSER_BIN="$PWD/target/debug/kageha-browser" \
  node crates/kageha-loop/tests/webui_browser_e2e.mjs
```

The first integration fixture drives real Chromium through multiple independent
runtime turns with no Python in the backend PATH. It proves consent, actual fill,
receipt lookup, denial and tab isolation. Transport tests additionally cover changed
selection/arguments, lost mutation response, partial reply cancellation/deadline,
model transport injection and strict keyboard/frame projection.
