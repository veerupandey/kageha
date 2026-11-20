# Rust workspace

Kageha runs as a Rust service with a React web client. The release launcher starts the backend, locates packaged model and frontend assets, and keeps installation files separate from chat state. The personal messaging gateways are optional Node.js services.

| Area | Main components |
| --- | --- |
| Chat and tools | `kageha-loop` serves the Web UI and chat API. It connects the model adapters, tools, approvals, and event streams. |
| Execution and recovery | `kageha-contracts` defines shared interfaces. `kageha-runtime` drives turns, `kageha-runtime-store` persists state, and `kageha-workflow` tracks work that can be resumed and verified. |
| Browser and desktop | `kageha-browser` drives Chromium. `kageha-browser-decisions` can propose bounded actions for the native executor to validate. `kageha-computer` provides optional native macOS control. |
| Workspaces | `kageha-workspace` handles shell sessions, files, and artifacts. `kageha-sandbox` confines local processes. The host also supports Docker workstations and SSH destinations. |
| Access and integrations | `kageha-policy` classifies tool access. Separate components handle credentials, model adapters, MCP, and service connectors. |
| Clients and compatibility | The [SDKs](../sdk/README.md) expose client interfaces. The [Rust launcher](../crates/kageha-harness/README.md) also documents offline import of supported legacy data. |

The backend owns the chat and run state. Reopening the Web UI reattaches to the same run. Actions that may have already run use receipts for recovery. Follow-ups, worker jobs, approvals, and file artifacts keep their own persisted state.

## Maintainer checks

From the repository root, run the Rust component suite and frontend checks.

```sh
scripts/check_rust_components.sh
npm --prefix frontend ci
npm --prefix frontend run lint
npm --prefix frontend test
npm --prefix frontend run build
```

`cargo run --locked -p xtask -- boundaries` checks dependency direction. `cargo run --locked -p xtask -- cutover-audit` checks the Python-free backend source boundary. The [Linux deployment guide](LINUX_DEPLOYMENT.md#qualify-a-release) describes how to qualify the installable bundles.
