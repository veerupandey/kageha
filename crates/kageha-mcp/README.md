# Independent Rust MCP adapter

`Pool` provides scoped, persistent stdio and Streamable HTTP connections. `McpTools`
implements the public `ToolPort`, including discovery, binding metadata and explicit
session/turn execution contexts. It imports contracts, not Kageha's runtime, WebUI
or database. `kageha-tools::Composite` combines this adapter with other tool ports;
ambiguous tool names are rejected before dispatch.

Each component owns its journal. A stable session/turn/call ID is persisted before
sending `tools/call`. Confirmed replies, including explicit MCP errors, are cached.
A transport failure after admission leaves an uncertain receipt. It blocks new
calls only for that session/server and survives restart or connection release.
`mcp_receipts` inspects the current session without allowing a caller-selected scope.
No automatic replay or generic “clear uncertainty” operation exists: opaque remote
mutations require domain-specific reconciliation. A cached receipt is evidence of
the MCP reply, not proof of a business transaction.

Connections are serialized per configuration/session; different scopes can run
independently. Call deadlines are bounded to 12 seconds and cancellation is checked
during connection, writes and reads. Requests/replies are capped at 1 MiB. The pool
holds at most 128 connections, with explicit idle-session release. Release rejects
active connections and fences already-waiting callers; receipts remain durable.
The WebUI compatibility endpoint is `POST /api/sessions/{chat}/mcp/release`; active chats/workers cannot release their connections.
Erase deletes recorded results and tombstones the session to reject future calls.
The journal permits one writer and rejects foreign schemas and symlink paths.

Configuration changes alter binding fingerprints. The standalone host rechecks the
binding after approval; callers cannot change endpoints or credentials through tool
arguments. Discovery is validated and pinned when the adapter is constructed.
Tools are mutations by default; only operator-configured `read_only_tools` bypass
mutation approval. Server-supplied descriptions/annotations never grant authority.
HTTP uses pooled Reqwest, validates JSON-RPC IDs/session identity, supports fragmented
SSE and refuses redirects. Custom headers require HTTPS; ambient CA bundle settings
are supported. Stdio launches argv directly, bounds nonblocking I/O and terminates
its process group on disconnect. No Python, curl, shell or Node is needed by this
adapter. The configured server may have its own explicitly installed runtime.

Existing WebUI `/api/mcp` configuration, tool IDs and secret storage remain through
`ui_mcp`, which delegates transport and receipt ownership here. Legacy local MCP
servers retain inherited environment compatibility; the independent host uses
explicit environment references and strips Kageha control-plane credentials.

## Disposable server and checks

```sh
cargo test --locked -p kageha-mcp -p kageha-tools
cargo build --locked -p kageha-mcp --example fixture
cargo build --locked -p kageha-runtime-service
node crates/kageha-runtime-service/tests/mcp_e2e.mjs
```

Node in the last command is the external test driver and model fixture. The backend
and stdio fixture run with an empty executable PATH. Tests cover persistent state,
exact cached calls, errors, deadlines, restart uncertainty, approval, HTTP/SSE,
context isolation, and unrelated work continuing after an ambiguous call.

This is a fixed-generation adapter extraction, not managed live replacement or full
MCP protocol parity. Resources, prompts, sampling, elicitation, pagination and
server-initiated requests are not exposed; unsupported traffic is bounded. Browser
MCP calls do not automatically provide the typed right-panel/profile integration.
