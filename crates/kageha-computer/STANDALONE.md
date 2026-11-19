# Standalone computer-use runtime

This component is being extracted from Kageha. It owns computer execution, not an agent's reasoning loop. A host can use its Rust library, Rust child-process client, protocol-2 JSON lines, or native MCP stdio. No Python is required by these paths. The existing protocol-1 adapter remains available for compatibility; its host-side Python authorization has **not** been silently replaced.

The control authority is Rust. macOS Accessibility, event delivery and ScreenCaptureKit use a statically linked Swift adapter. **Swift is the intended macOS implementation, not a migration blocker.** This migration removes Python orchestration and runtime dependencies; it does not require replacing platform-native languages. The Rust Kageha host now has an opt-in adapter using this component's Rust client. Complete UI parity and production reliability still require the release checks below.

## Boundaries

- `src/control.rs`: versioned scope, process epochs, exclusive desktop leases, fresh observations, host authorization, durable receipts and image binding.
- `src/lib.rs`: `DesktopBackend` adapter trait and existing macOS bridge adapter. Custom test/remote adapters can implement the trait without a Kageha dependency.
- `src/client.rs`: owned child process with fixed caller identity and pinned epoch. Transport errors preserve uncertainty; requests are never automatically replayed.
- `src/mcp.rs`: MCP stdio over the same authority, with JSON outcome text and observation image blocks.
- `src/legacy.rs`, `src/plan.rs`, `native/Platform.swift`: existing protocol-1/native behavior, preserved separately.

A controller serializes one physical desktop. Native controllers take a private per-user desktop lock for their whole lifetime, as well as a separate journal owner lock. Different journals cannot create simultaneous native protocol-2 owners. The locks do not coordinate older protocol-1 helpers or arbitrary external automation: do not run them alongside the new controller.

Scope labels are routing identities, **not authentication**. Launch one trusted stdio owner, or authenticate callers in the embedding host before fixing their scope. Different labels cannot isolate untrusted tenants sharing one logged-in OS desktop. A future remote API must preserve the same controller and add authenticated routing, encrypted transport, bounded queues and operational isolation.

## Launch

Build from the workspace:

```sh
cargo build -p kageha-computer
cargo test -p kageha-computer
```

The crate also builds when copied into a separate repository: its manifest names its own dependencies and it has no Kageha crate dependency. macOS 14+ and Xcode command-line tools are required for the current native adapter.

Create a private journal directory, then start one owned process:

```sh
mkdir -p "$HOME/.local/share/kageha-computer"
chmod 700 "$HOME/.local/share/kageha-computer"
./target/debug/kageha-computer control \
  --journal "$HOME/.local/share/kageha-computer/actions.sqlite"
```

This launch is read-only. The operator can explicitly add `--allow-input` for a trusted agent process. A remote model cannot grant this permission by calling a tool. Rust embedders should use `Controller::set_authorizer` to authorize exact immutable resolved arguments; denial occurs before journal admission or OS dispatch. The callback receives scope, method and resolved native arguments, including the exact process/window. Application bundle identity, approval persistence and app allowlists remain the embedding host's responsibility until a native identity grant contract is implemented.

MCP launch:

```sh
./target/debug/kageha-computer control \
  --journal "$HOME/.local/share/kageha-computer/actions.sqlite" \
  --mcp --workspace my-workspace --client my-agent
```

Configure that executable and argument list as an MCP stdio server in an agent framework. It supports MCP `2024-11-05` lifecycle and tools, not HTTP/Streamable HTTP. Initialize, send `notifications/initialized`, discover `computer_info`, then call `computer_call` with the advertised epoch. Input still needs the explicit host opt-in or embedding policy. The generic call tool exposes the operation catalog; per-operation typed tool schemas remain a release gate.

## Protocol 2

JSON-lines mode uses an outer transport identity and an inner durable action identity:

```json
{"id":1,"method":"control","args":{"protocol":2,"workspace_id":"my-workspace","client_id":"my-agent","request_id":"connect","operation":"info","args":{}}}
```

The result advertises the service epoch, actions, operations and limits. Subsequent calls include that epoch. Each request supplies a safe, bounded `request_id`. The transport `id` is only for matching a pipe response; the inner `request_id` protects an admitted native input from replay.

1. `permissions`: check Accessibility and Screen Recording without a prompt. Inventory uses `list_apps` and `list_windows`.
2. `acquire`: request `ttl_ms` in `1..60000`; one live desktop lease at a time. Use `renew` with the exact lease or explicitly `release` it.
3. `observe`: supply `lease_id`, exact `pid`, exact `window_id`. AX is always requested; screenshots default off. Optional `include_screenshot`, `max_elements` in `1..2000`, `image_format` of `jpeg` or `png`, and `max_image_dimension` in `1..4096` (default 1920). Reduce dimension or use JPEG when a capture exceeds the transport limit.
4. `execute`: supply `lease_id`, `observation_id`, and one `action` with `method` and object `args`. The controller injects the bound process/window/session; attempts to override those fields are refused.
5. Observe again after every admitted action. Observations expire after 30 seconds and after takeover, release, reacquisition, replacement or input. This is a maximum age, not proof that pixels have stayed unchanged: the current native coordinate guard rechecks geometry, not visual content or human input.
6. On a lost response, query `receipt` with the **original** action `request_id`. Do not issue the same input with a new ID merely because a transport failed.

Example action arguments:

```json
{"lease_id":"returned-lease","observation_id":"returned-observation","action":{"method":"set_value","args":{"element_token":"token-from-this-observation","value":"example"}}}
```

Supported actions are click, native text insertion, key, native value, scroll, drag, secondary AX action, selection, explicit window focus and observed-element focus. Coordinates are screenshot-local, not desktop-global; native dispatch requires captured pixels and unchanged window geometry. The native bridge rechecks exact key focus and refuses DOM field writes. Screen-wide input, clipboard, held-key APIs, Linux/Windows production adapters and remote desktops are not advertised.

Rust client use:

```rust,no_run
use kageha_computer::client::ComputerClient;
use serde_json::json;
use std::path::Path;

let mut client = ComputerClient::launch(
    Path::new("/absolute/path/kageha-computer"),
    Path::new("/private/owned/directory/actions.sqlite"),
    "workspace", "agent",
)?;
let permissions = client.call("permissions", json!({}), "permissions-1")?;
// The host can explicitly choose launch_with_input instead, after authorizing
// this process. A framework reasoning loop remains independent of this client.
# Ok::<(), Box<dyn std::error::Error>>(())
```

The current native adapter must run on its process main thread; it is intentionally not Send/Sync. Use the Rust child-process client from framework worker threads. The synchronous client has no hard I/O deadline. Isolate it in an owned worker if the host requires a cancellable wait, then recover the original receipt. Killing a worker does not roll back an OS operation. An explicit new client creates a new epoch; there is no automatic reconnect or authority renewal.

## Receipts and privacy

The SQLite journal records an intent digest and reservation **before** native dispatch, with synchronous durability enabled. It stores the resulting receipt separately. Action arguments/text are not copied into the journal; native result/error metadata is retained in the private file. Backend implementations must avoid echoing secrets in their results.

- Controller validation, policy and stale-observation failures report `not_dispatched` and create no action receipt. A native adapter preflight refusal after durable admission produces a recoverable `not_dispatched` receipt. The Swift adapter marks only its validation/read-only preflight boundary this way; errors after entering the action remain unknown.
- A native event acknowledgment reports `dispatched`, never task success.
- Independent native readback can report `verified`; it does not establish application submission, document saving or another server-side commit.
- An adapter error after entering native dispatch, interrupted reservation, or failed completion write reports `unknown`. Duplicate IDs recover that record rather than dispatch again.
- A reused ID with different admitted arguments is rejected. Completed/unknown receipts survive process restarts and remain queryable after epoch changes.
- Capacity is 4096 admitted actions. Capacity exhaustion refuses new input without evicting old deduplication records. Production archival/compaction is not implemented. A reservation that expires during durable admission is retained as a not-dispatched receipt when its cancellation can be persisted.

Journals and lock files require an owned private directory and regular files; leaf symlinks, hard-linked files and broad permissions are refused. Images are written to a private capture directory beside the journal and bound to exact observation tokens. MCP reads only controller-issued captures, checks PNG/JPEG magic, and limits image bytes to 4 MiB. At most eight successful captures from one live controller are retained. Crash leftovers and cross-restart capture retention still need a production cleanup policy.

`Controller::take_control` is a host-only method: it revokes lease and observation. It cannot undo or cancel an OS action already entered. Physical human-input detection and screen-lock observation are not implemented yet; an explicit host takeover is the currently enforced contract.

## Evidence and release gates

The [Codex/Antigravity evidence](research/competitor-evidence.md) and [UI-TARS/UFO source audit](research/framework-audit.md) record pinned identities, selected sources, observed facts and unknowns. Static code inspection is not runtime parity or performance evidence. No proprietary implementation was copied.

Required before the Python-free production cutover:

1. Complete app identity grants, desktop preview/manual-control parity and full end-to-end approval/takeover tests through the Rust Kageha host. The initial Rust host adapter provides native tools, per-chat ownership, approval-bound input and receipt recovery.
2. Harden and retain the Swift/public Apple API adapter, including stable signing and macOS permission identity. Keep OS permissions separate from host authorization. Other operating systems may use their appropriate native languages behind the same Rust contract.
3. Implement typed permission/locked-screen/intervention outcomes, app-instance identity, deadlines/queue admission and native event interruption cleanup.
4. Add real disposable-fixture tests for AX values, radio/checkbox state, selections, focus/sheets, Unicode, scrolling, drag, screenshot scales and monitor changes. Run permission, crash, cancellation and takeover fault injection on macOS first; extend the matrix when additional OS adapters are introduced.
5. Add full/diff observation baselines, AX notifications and bounded capture streaming after measuring equal geometry and codec. Do not loosen observation binding merely to hide stale-state errors.
6. Add native clipboard ownership/restoration and remaining action capability groups only with readback, secret handling and held-input cleanup tests.
7. Publish per-operation JSON schemas/typed agent adapters and a bounded authenticated network API. MCP stdio and the Rust library are available now; a remote computer HTTP endpoint is not.
8. Add journal archival, private artifact retention, metrics, long-running soak/performance tests and extraction CI. Test upgrades/rollback without reusing old authority or repeating uncertain input.

These gates are outstanding work, not claims of completeness. Rust owns the shared control path; Swift owns the macOS API adapter. Existing Kageha sessions and user drafts are preserved.

## Rust Kageha host configuration

The host discovers the installed `~/.local/share/kageha/computer/Kageha Computer.app` helper and creates an owned mode-0700 state directory under `~/.kageha/runtime/computer-rust`. Override these with `KAGEHA_COMPUTER_BIN` and `KAGEHA_COMPUTER_STATE_DIR`. The host uses the Rust client, not Python. Input starts disabled unless `KAGEHA_COMPUTER_ALLOW_INPUT=1` or the operator chooses **Enable agent control**; model-initiated input still requires the native approval policy.

The host registers `computer_info`, `computer_permissions`, `computer_apps`, `computer_windows`, `computer_activity`, `computer_inspect`, `computer_observe`, `computer_execute`, `computer_receipt`, and `computer_release`. Only the observing chat owns the desktop lease. Approval binds the original process/window/epoch/observation and action digest; changed or expired authority sends no input. Durable input request IDs bind chat, turn and provider call. Unknown outcomes remain errors with a recoverable request ID.

`GET /api/sessions/{chat}/computer` exposes ownership/status. `POST .../computer/lease` supports user takeover/return; starting an agent turn does not return control automatically. `POST .../computer/access` explicitly enables or revokes host input authorization, and `POST .../computer/permission` requests macOS permission from an operator gesture. Neither is an agent tool. Revocation closes the old helper connection and invalidates old input authority.

`observe` also returns an owner-bound `preview_id`. The public `preview` operation and `GET .../computer/preview` capture that exact window without replacing the actionable observation. Preview grants last 60 seconds; polling does not renew them. A new observation grants another preview. Input release/takeover leaves this short-lived read grant intact. Images are inline JPEG/PNG, bounded to 4 MiB; polling files are deleted after reading. The panel displays these frames and offers takeover, and supports manual click, right-click, drag, scroll, text entry and key shortcuts after explicit takeover and input opt-in. Manual requests carry a recent `frame_id` and a unique `request_id`; the host resolves the exact window from its own frame record. It retains at most four issued frames, valid for 15 seconds. Frames are consumed by a manual action. A fresh screenshot must match the displayed dimensions, and the native adapter rechecks geometry and focus. Returning to Agent or revoking access invalidates queued manual input.

The packaged helper launches its native worker through LaunchServices and a private socket so macOS attributes permission checks to Kageha Computer. Unpackaged binaries have their launcher's permission identity. Ad-hoc signature changes may require reapproval; stable distribution signing remains necessary for production updates. The host dependency disables `native-macos`, so consuming its transport client does not link Swift into the agent loop. The separately built helper enables `native-macos` by default.

## Benchmark-driven development

The native [benchmark suite](bench/README.md) grades independent app state and fault recovery. It found and closed a preflight outcome-classification gap: the matched suite improved from 30/42 to 42/42, with another 28/28 held-out layout trials. These are scripted runtime measurements, not a full agent benchmark score.

### Explicit field focus

`focus_element` takes only an observed `element_token`. The exact window must already be focused, the control must still belong to that window, and its native focus attribute must be settable. Use `focus_window`, re-observe, `focus_element`, then re-observe before selecting or typing. Focus changes are explicit authorized actions; selection errors do not trigger automatic focus/key/pixel fallbacks. Disabled observed controls are rejected before any indexed native mutation.

The expanded v3 frozen benchmark recorded 258 passing trials, two intermittent native AX failures during invalid-save clicks, and 30 blocked capture/pixel trials. Both native failures remained unknown, with no save confirmed and no write replay. Screen Recording access is unavailable to the benchmark helper, so screenshot coverage is not claimed. See the benchmark guide for preserved failing runs, workflow corrections and raw evidence.

The subsequent action-timeout fix separates 100 ms AX traversal reads from bounded one-second native mutations. The 30-scenario follow-up passed 270 runnable trials with zero failures and 30 Screen Recording permission blocks. Unknown outcomes are still never automatically retried. Exact comparison/provenance: `bench/results/2026-11-07-action-timeout-summary.json`.

### Manual operator input

Authenticated operator endpoint: `POST /api/sessions/{chat}/computer/action`, for example `{"action":"click","x":400,"y":200,"frame_id":"issued-by-preview","request_id":"new-uuid"}`. Coordinates are pixels of the issued image. Clients reject clicks outside the contained image and scale CSS coordinates into those pixels. The request cannot choose another PID/window. Reusing an ID with identical input returns the recorded result; changing it conflicts. A reserved request with an unrecorded result remains unknown after restart and is never replayed.

Panel mouse and key actions explicitly request `delivery_mode:"foreground"`. Focus must already match the exact window (the panel has an explicit Focus button). Mouse points must be unobstructed in the live window stack; guards run before and during gestures. This mode uses public macOS HID event posting and moves the Mac pointer. It is not a silent fallback after a failed background action. Existing default PID/AX delivery remains separate; a dispatch acknowledgement never proves that an application accepted an event. Foreground focus is read through the system accessibility API rather than cached NSWorkspace notifications. Human input and events from other automation processes pause the controller through a listen-only event tap. Atomic exclusion against another app stealing focus between an OS check and dispatch remains a production limitation; an OS call already entered cannot be cancelled retroactively.

The UI keeps typed text when input is refused, preserves input errors across polling, suppresses concurrent manual dispatch, and uses LAN-compatible cryptographic request IDs. Permission changes remain operator-only. Stable signing/notarization is not completed: this machine has no valid code-signing identity. Local ad-hoc updates can require reapproval of Kageha Computer only.

After an idle preview grant expires, the operator can choose **Refresh view** (`POST .../computer/refresh`). This read-only gesture inspects only the chat's previously selected window without acquiring input authority, grants a new 60-second preview, and invalidates old input frames. Polling itself does not renew the read grant. Foreground synthetic events use a private CGEventSource with bounded press/release intervals; the optional native monitor uses public listen-only event APIs, with explicit Input Monitoring consent. No private Apple API is used.


### Human intervention and restart recovery

Input-enabled native leases require a healthy listen-only macOS event monitor. The runtime retains a revision counter, a random per-process nonce, availability and held-input booleans; it does not retain typed characters, key codes, or pointer positions. Events tagged with the helper's own process ID do not increment the revision. External mouse movement, clicks, keys, scrolls, and monitoring coverage gaps invalidate existing input authority. The monitor also checks the current Input Monitoring grant and public macOS Secure Input status; it refuses input when either prevents monitoring. A durable pause survives helper restart. On acquisition/resume, pre-existing held input is rejected. During a lease the event revision is authoritative: hardware state briefly includes the helper's queued synthetic key-down. Keyboard chord key-up clears synthetic modifier flags.

An input-enabled lease records an `armed` marker before granting authority. Restart while armed returns `control.reason: helper_restarted`. No model operation clears this hold. An embedding operator may call `Controller::resume_control()` or launch the client with `ClientOptions { resume_control: true, ..Default::default() }` after an explicit user gesture. The equivalent executable flag is `--resume-control`. Resuming grants neither a lease nor an observation; agents must observe again. Never derive this host option from model arguments.

`inspect` reads one exact `pid`/`window_id` without a lease and returns a **read-only** `inspection_id`, plus a bounded preview grant. It works while paused. Its tokens cannot authorize mutations. `receipt` continues to recover verified, dispatched, refused, or unknown outcomes across epochs; unknown outcomes must never be replayed. The Rust SDK uses bounded Unix pipe I/O (default 10 seconds, configurable up to 120 seconds). A transport deadline closes the owned connection and reports uncertainty without retrying.

Kageha's authenticated take/return controls restart the owned connection explicitly, preserve the selected window, clear old frames, and show a pause reason. **Return control to agent** and **Resume manual control** are operator gestures. Refresh view remains read-only. The helper is separate from Kageha and the same controller/client/MCP contract works with another trusted framework.

Acceptance evidence: `bench/results/2026-11-07-intervention-recovery/summary.json`. Native tests mutate only owned disposable fixtures. External-process input injection tests interruption handling; they are not presented as a physical-device benchmark. Public-site, OS-wide atomic input exclusion, and unattended soak coverage remain separate release work.
