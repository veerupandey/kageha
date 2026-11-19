# Kageha SDKs

The [Agent SDK guide](AGENT.md) covers run/status, lifecycle events, steering, cancellation, approvals and in-process Rust embedding. Rust, TypeScript and optional Python clients use the independent runtime protocol.

## Optional memory clients

These clients address the independent Rust memory service. They are operator-level clients: access to portable export/import belongs to trusted composition, not to agent prompts. Application hosts derive their agents' namespaces and identities.

- Rust: `kageha-memory::http::HttpMemory` implements `kageha-contracts::MemoryPort`. See `crates/kageha-memory/examples/memory-client.rs`.
- TypeScript/JavaScript: `sdk/typescript/client.mjs` and its declarations provide `MemoryClient`. The live Kageha replacement test exercises this client.
- Python: `sdk/python` is an optional client-only package using the standard library. Nothing in the Rust backend imports or launches it.

Run the optional Python client tests separately from backend installation gates:

```sh
cargo build -p kageha-memory
PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -v
```

All three clients support explicit writer claims, scoped writer contexts and retirement. Once a backend is managed, a plain client cannot bypass its writer epoch. Claims require an explicit expected epoch and a stable request ID; do not adopt another framework's writer automatically.

These memory clients and the Agent SDK cover their documented protocols. Full browser/runtime/harness SDK parity and published packages remain release gates.
