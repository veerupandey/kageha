# Standalone local browser client

This Rust library extracts Kageha’s existing Unix-socket protocol client without
importing its application, runtime, database or UI. The existing WebUI uses a
compatibility re-export; preview framing, profile control, epoch checks, private
state validation and connection rollover keep their previous wire format.

```rust,ignore
let mut browser = kageha_browser_client::Client::connect_to(
    "workspace-id", "agent", state_dir, false,
)?;
let selection = browser.selection()?;
```

`call_controlled` adds a caller-owned attempt ID, bounded deadline and cooperative
cancellation while waiting for a reply, including partial JSON headers. It never
resends a failed attempt. Failed streams close so a late reply cannot become a
subsequent operation’s result. A potentially sent mutation requires receipt
inspection; cancellation is not proof the browser did not execute it.

The transport does not grant application permission. Hosts supply consent, exact
approval, workspace scope and native control authority. `kageha-tools::browser`
provides an independently reusable typed tool adapter over this client.

This is the local client library, **not** the browser’s in-process engine library.
Engine extraction, public SDK completeness and managed live browser replacement
are separate work. HTTP/MCP transports remain owned by the browser daemon.
