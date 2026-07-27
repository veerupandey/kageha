# Independent SSH execution component

`kageha-execution` is a Rust library implementing the shared `ToolPort`. It has no
dependency on Kageha's runtime, WebUI or application database. The WebUI and
standalone runtime compose this same implementation; another framework can do so
without starting Kageha.

## Embed

Construct `Tools::workspace(config, upload_root, Arc<Journal>)`, or use
`Tools::new(config, Arc<dyn UploadSource>, journal)` to supply scoped attachments.
The host supplies authenticated `ToolContext`, its policy/approval ports,
cancellation and deadlines. Discover the configured tools through `ToolPort`:

- `remote_exec`: execute an authorized shell command on the configured account.
- `remote_upload`: snapshot local bytes, bind approval to their hash and remote
  destination, then verify the transferred hash before publishing success.
- `read_file`: bounded remote workspace text reads.
- `remote_receipts`: inspect this session's receipt summaries without remote input.

Model arguments cannot override the SSH destination, credentials, journal or
session scope. A changed configuration, command, timeout, destination or upload
hash invalidates the previous binding. `execute_authorized_in` validates the exact
binding immediately before dispatch. The host must enforce policy before calling
it; this comparison is not an approval service. The lower-level `Remote` transport
likewise expects a trusted caller to supply authorization and receipts.

Each component instance has a dedicated SQLite journal and exclusive writer lock.
It persists intent before mutation, caches exact completed calls, and fences the
SSH account when dispatch may have occurred without a known result. Account
identity includes host, user and port; changing labels, workdir or credentials
cannot bypass the fence. Host aliases are not resolved into a universal account
identity. Hosts must not configure aliases as independent ways to bypass recovery.

Cancellation stops the local SSH process group; it cannot prove a remote command
stopped. SSH exit 255, lost transport, process death and failed receipt publication
therefore leave an unknown outcome. Restart never replays such input. An ordinary
remote nonzero exit is recorded as a known failed command, which may still have
made partial changes. Reads and independent targets remain usable.

## Operator recovery

`http::router(journal, token)` provides authenticated operator routes:

| Route | Behavior |
|---|---|
| `GET /v1/execution/receipts?session=CHAT` | Bounded receipt summaries; omit session for operator-wide inspection |
| `POST /v1/execution/receipts/ID/acknowledge` | Explicit decision allowing future distinct work, without replay |

The acknowledgement body is
`{"target":"EXACT_RECEIPT_TARGET","allow_continuation_without_replay":true}`.
Active commands cannot be acknowledged. The historical result remains unknown;
this decision neither confirms success nor reruns the original call. There is no
agent tool to clear uncertainty. Deleting a chat erases cached output and fences
its scope, while retaining unknown target blockers. Late completion cannot restore
the deleted output.

Kageha's existing authenticated WebUI server exposes equivalent operator routes
at `/api/remote/receipts?session_id=CHAT` and
`/api/remote/receipts/ID/acknowledge`. These are recovery APIs, not a newly added
frontend workflow. Existing host selection, connection tests, approvals and upload
ownership remain in the compatibility host.

## Dependencies and limits

OpenSSH is the declared transport dependency. It runs with explicit arguments,
without a local shell, ambient SSH configuration or connection multiplexing.
Only PATH, HOME, LANG and SSH_AUTH_SOCK are inherited. Optional managed process
sandbox requirements must be satisfied explicitly; there is no fallback.

Standalone configuration defaults to strict host-key checking. Provision
`known_hosts` independently. The legacy WebUI retains its existing accept-new
policy in its private known-host file; changed keys remain rejected.

The remote account needs a POSIX shell, `realpath`, `head`, `mktemp`, `cat`, `mv`
and either `sha256sum` or `shasum`. Uploads are limited to 16 MiB, text reads to
64 KiB, output to 1 MiB, commands to 32 KiB at the tool boundary, and execution
timeouts to 120 seconds. The shared runner enforces caller cancellation and any
earlier deadline.

Relative remote file paths reject traversal and validate resolved containment.
These portable shell checks are **not race-proof against a concurrently hostile
remote filesystem**. Local upload snapshots use the separate descriptor-based
workspace implementation. Arbitrary approved commands run with the remote
account's authority; its OS permissions provide confinement.

This slice does not implement browser/desktop tunnels, an independent execution
daemon, legacy receipt import, managed live execution replacement or a device
enrollment protocol. The current adapter binding generation is 1.

## Verification

```sh
cargo test --locked -p kageha-execution
node crates/kageha-runtime-service/tests/execution_e2e.mjs
KAGEHA_LOOP_BIN="$PWD/target/debug/kageha-loop" node crates/kageha-loop/tests/native_adapters_e2e.mjs
```

The standalone test uses a disposable SSH protocol fixture and actual host process
death after dispatch. Node is only the test driver/model fixture, not a backend.
The WebUI suite optionally uses loopback OpenSSH when its configured container
image is available. No live user account is mutated by the default fixtures.

## Lifecycle hook execution

`hooks` is a reusable approved command/HTTP executor over the same owned journal.
It consumes `kageha-workspace::hooks::{Hook, Set, Executor}` contracts; it does not
import the runtime, policy database or UI. A host supplies a `ToolContext`, actual
workspace root, control and an authorization callback. `hooks::context` defines the
exact approval payload. `hooks::execute` verifies configuration before/after the
decision; `hooks::execute_captured` is for a host that already owns an immutable
per-turn snapshot. That distinction preserves existing WebUI snapshot semantics.

The executor preserves existing hook receipt IDs. It caches confirmed output,
reserves intent before command/HTTP dispatch, and leaves failed or interrupted
dispatch uncertain. Unknown outcomes cannot be cleared by an agent. Shell commands
use `/bin/sh` with a scrubbed environment and bounded process-group I/O; HTTP uses
the pooled Rust transport with deadlines, cancellation, response limits and no
redirect following. These trusted approved commands are not an OS sandbox.
