# Runtime journal adapter

`kageha-runtime` owns lifecycle decisions through `RunStore` and `RunExecutor` ports.
This library supplies a separate SQLite journal. It does not open application tables,
import WebUI types, or require a browser, model vendor, or Python installation.

```sh
cargo test -p kageha-runtime-store
```

`tests/supervision.rs` is an executable embedding example: supply an executor,
open `SqliteRunStore`, construct `Supervisor`, stage a portable `RunSpec`, then
activate it. Staging cannot execute inputs. Activation follows the consumer's
committed admission/outbox record. Start recovery before accepting new work.

The supervisor bounds admission and execution independently at each worker depth,
serializes runs in one session, and gives nested workers separate capacity. Cancel
is durable before cooperative signaling; it propagates through `parent_run` links
without cancelling independent work. Steering has stable message IDs and retains
its history after checkpoint delivery.

Each claimed run has an incarnation-scoped lease. Confirmed results are journaled
before compatibility publication. Publication retries never execute inputs. A
restart inspects adapter-owned committed evidence; admitted runs without that
evidence become interrupted, rather than being replayed. Late obsolete receipts
are retained for inspection and cannot replace the active result.

Effect dispatch is recorded before invoking a consumer callback. A confirmed exact
receipt can be returned idempotently; an uncertain dispatch cannot be replayed.
Session erasure purges payloads and keeps tombstones to fence reimports and replay.
Only one writer process may open the journal. Use a persistent volume for its
SQLite file and its sibling lock file.

Kageha's adapter is `kageha-loop/src/runtime_host.rs`. Existing chat/job APIs remain
compatibility projections, not this library's storage format. The end-to-end
fixture kills that harness after its adapter commits a model result and before the
runtime publishes completion:

```sh
cargo build -p kageha-loop
KAGEHA_LOOP_BIN="$PWD/target/debug/kageha-loop" \
  node crates/kageha-loop/tests/runtime_supervision_e2e.mjs
```

Node (with `node:sqlite`) is the test client only; the backend fixture PATH contains
no Node or Python. `kageha-runtime-service` now supplies independent HTTP hosting using public model/tool
execution ports. Managed live replacement, broader remote domain-tool hosting and
full SDK lifecycle coverage remain future work. This package alone does not
establish full M3 or release parity.
