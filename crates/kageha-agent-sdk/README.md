# Kageha Agent SDK for Rust

Reusable async client for the standalone Rust runtime: run/status, resumable SSE
lifecycle events, steering, cancellation and exact approval decisions. Depends on
shared contracts/runtime types and HTTP transport; no WebUI or host database.

See [the SDK guide](../../sdk/AGENT.md) for all three languages, records, recovery
semantics and runnable examples.

```sh
cargo run --locked -p kageha-agent-sdk --example embed
cargo test --locked -p kageha-agent-sdk --all-targets
```

`examples/embed.rs` embeds a durable runtime without a server; `examples/client.rs`
uses the service API without importing any host implementation. Hosting dependencies
are development-only, used by the embedding example. This is a local prerelease
crate (`publish = false`).
