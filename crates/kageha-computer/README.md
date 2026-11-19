# Kageha native computer backend

The new framework-independent protocol-2 controller, Rust client and native MCP
stdio adapter are described in [STANDALONE.md](STANDALONE.md). They add host-only
authorization, desktop ownership and durable action receipts without Python.
The description below covers the existing protocol-1 compatibility path.

This is the sole macOS execution backend for Kageha's existing `computer_*`
tools. Rust owns a persistent JSON-lines service, protocol validation, frame
limits, snapshot generations, and batch admission. A statically linked Swift
bridge calls public Accessibility, CoreGraphics and ScreenCaptureKit APIs. It
never launches a child process for an individual desktop operation.

Rust is the default and only native executor. The previous socket/CLI and Python
input fallbacks have been removed. This backend has verified AX observation and
field-value batches on a disposable AppKit test window. Screenshot performance
parity with Codex has not been established: the new helper needs its own Screen
Recording authorization. No private SkyLight API or copied vendor source is used.

## Build and install

Requires macOS 14+, Xcode command-line tools with Swift, and stable Rust/Cargo.

```sh
./scripts/build_rust_computer.sh
./scripts/build_rust_computer.sh --check-signing
# With a Developer ID Application certificate and private key in Keychain:
./scripts/build_rust_computer.sh --install
```

The installer writes a user-local app at
`~/.local/share/kageha/computer/Kageha Computer.app` and a launcher symlink at
`~/.local/bin/kageha-computer`. It does not request permissions or modify unrelated installed applications. Installation now requires a valid Developer ID Application identity, or an explicit `--ad-hoc` choice for local development. Missing certificates fail before building or replacing the installed helper. A unique valid Developer ID identity is selected automatically; set `KAGEHA_COMPUTER_SIGN_IDENTITY` to a fingerprint when more than one exists, and optionally pin `KAGEHA_COMPUTER_TEAM_ID`.

Updates are staged, signed and verified before an atomic macOS app-directory exchange. The old app is retained for rollback. Both apps must satisfy each other's designated requirements; incompatible identity changes stop the update. Only an intentional `--allow-identity-change` migration can override that check, and a Developer ID app cannot be downgraded to ad-hoc. Compatible designated requirements are how macOS recognizes an update for privacy-resource access; this is not a guarantee that OS policy will never require renewed consent. See [Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).

For local development without an Apple Developer account:

```sh
./scripts/build_rust_computer.sh --install --ad-hoc --allow-identity-change
```

Changed ad-hoc builds can require renewed Accessibility/Screen Recording grants. The flag makes that change deliberate; it does not bypass macOS consent. The currently working helper is preserved when an update fails.

For a Developer ID release, store notarization credentials interactively in Keychain and use their profile name (never put a password in the repository):

```sh
xcrun notarytool store-credentials kageha-notary
export KAGEHA_COMPUTER_NOTARY_PROFILE=kageha-notary
./scripts/build_rust_computer.sh --package "/absolute/output/Kageha Computer.app" --notarize
# First migration from the local ad-hoc installation may need --allow-identity-change:
./scripts/build_rust_computer.sh --install --notarize --allow-identity-change
```

The release path enables hardened runtime and a secure timestamp, checks Apple's Accepted result, staples and validates the ticket, then runs `syspolicy_check distribution` before promotion. Actual Developer ID signing/notarization requires an enrolled Apple Developer account and its certificate; a self-signed or ad-hoc signature is not a substitute. [Apple's notarization guidance](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

The standalone `scripts/package_macos.sh` inside this component accepts an absolute built binary and output app path, so another framework can use the packaging flow without Kageha's agent loop. `KAGEHA_COMPUTER_VERSION` controls the numeric app version (default 0.1.0). A crashed installer may leave a private staging directory and an install lock; inspect its PID before removing a stale lock. No update automatically deletes an earlier rollback app.

For an uninstalled build:

```sh
export KAGEHA_RUST_COMPUTER_BIN="$PWD/target/release/kageha-computer"
```

An explicit missing binary fails with an error. There is no alternative native
backend or automatic action replay. The old `KAGEHA_COMPUTER_BACKEND` variable
no longer selects a backend. Obtain a fresh observation after reconnecting.

Run `computer_doctor` through Kageha to check the selected helper and its
Accessibility/Screen Recording status. Permission probes never prompt. Grant
permissions through macOS System Settings when you choose to enable them; an
existing grant to another application does not transfer to this app.

Clef/Jev settings are unchanged. Missing/empty decision configuration adds no
model call. Explicit active/shadow/off policy remains independent of the backend.

## Operations and limits

Supported protocol operations are advertised by `hello`: running app inventory,
window inventory, AX/window observation, AX/pixel click, direct native text
insertion, key, value, scroll, drag, secondary AX actions, app launch by bundle ID,
text selection, explicit window focus, `prepare_batch` and `execute_batch`. Unavailable functionality fails explicitly. Installed but
stopped apps are not enumerated; launch accepts an installed app name or bundle identifier.
Global input, clipboard operations,
and desktop-wide screenshots are not implemented in this first version.

A batch uses one exact window and at most 24 steps. Rust normalizes public tool
steps and resolves their observed refs in `prepare_batch` before Python applies
the existing approval policy. Rust validates the complete
plan; the platform bridge validates all target handles before dispatch. The helper
stops after the first failed or uncertain text/key/value effect and reports
partial outcomes. A batch is not transactional. Kageha still performs its existing
approval checks before invoking the helper. `computer_batch(verify=True)` returns
a fresh final observation via the existing tool contract.

Every helper instance uses a random nonce in its snapshot tokens. A new AX
observation supersedes earlier tokens for the same session/window. Mutation
consumes its observation; deterministic batch steps share their admitted snapshot
until the batch stops. Pixel input requires an actual screenshot and unchanged
window geometry. Restarted helpers refuse stale refs and unobserved coordinates.

Keys require the exact window to be focused. `computer_focus` explicitly activates
the observed window and verifies its live AX focus, then returns a fresh AX
observation. It does not type or retry earlier keys. A modal sheet blocks focus
to its underlying parent. Focus can change after verification, so key dispatch
checks it again.
Generic event dispatch is reported as unverified. Uncertain writes/keys are never
automatically replayed with another delivery mode. Web-content AX writes are
refused; use Kageha's browser backend for DOM fields. Model-facing observations use JPEG at quality 0.9 by default; pass
`image_format="png"` for lossless capture. Geometry and codec are returned for
benchmark comparisons. Live previews preserve the captured codec. This backend
does not include AX-notification caching or a continuous screenshot stream.

## Validation

```sh
cargo test --manifest-path crates/kageha-computer/Cargo.toml
PYTHONPATH=src .venv/bin/pytest -q tests/test_rust_computer_driver.py
PYTHONPATH=src .venv/bin/python scripts/smoke_rust_computer.py \
  --binary crates/kageha-computer/target/release/kageha-computer
PYTHONPATH=src .venv/bin/python scripts/benchmark_native_computer.py \
  --target Finder --samples 20 --no-screenshots
```

The opt-in smoke test launches a disposable local AppKit window, fills only its
two fields, verifies fresh AX readback, checks stale-token refusal, then terminates
that fixture. It does not type into other apps or grant any permissions.
The read-only benchmark requires an already-running app and excludes decision
models. Compare equal geometry, codec and tree scope before claiming speed parity.

## Protocol

The installed app is launched through macOS LaunchServices and serves one
private Unix socket per client. This gives the helper its own permission identity
instead of inheriting the launcher application's TCC attribution. The client
creates a private temporary directory; the socket is mode 0600, accepts one
connection, and exits after that connection closes. Unconnected startup expires
after eight seconds. There is no shared desktop daemon or input replay.

Unpackaged development helpers use `kageha-computer serve --stdio`. Standard
output contains only UTF-8 JSON lines. Requests and responses are limited to 8 MiB; requests require a numeric
`id`, a supported `method`, and object `args`.

```json
{"id":1,"method":"hello","args":{}}
{"id":1,"result":{"protocol":1,"backend":"kageha-rust","capabilities":["..."]}}
```

Errors use `{"id":1,"error":{"code":"...","message":"..."}}`. The Python
client serializes calls and validates IDs/version. Timeout or cancellation after
dispatch closes only the client-owned connection (or terminates its direct
subprocess); no mutation is replayed. A launched app finishes a bounded in-flight
native operation before exiting on EOF. A queued call's timeout cannot close
another in-flight request.

## Investigation evidence

The repository's `reports/codex-computer-control-2026-11-02/native-re` directory
contains artifact hashes, selected symbols, bounded disassembly, and findings.
Static symbols show separate AX/screenshot operations and both public SCK and
private SkyLight-related code in Codex. They do not establish its live Finder
capture path or timing constants. Kageha's public-API implementation is an original
design informed by that boundary, not a recovered copy of Codex's native code.

Apple references: [SCScreenshotManager](https://developer.apple.com/documentation/screencapturekit/scscreenshotmanager)
and [Accessibility APIs](https://developer.apple.com/documentation/applicationservices).

## Native benchmark loop

[bench/README.md](bench/README.md) describes the Rust/Swift execution-based
benchmark, independent app-state evaluators, strict recovery gates and measured
before/after results. This is separate from full model-driven MacArena/OSWorld
scores and does not establish complete production coverage.


The protocol-2 runtime now pauses on external input and monitoring loss, journals the pause across restart, and supports lease-free read-only `inspect` during recovery. Explicit operator resume never replays input. See [intervention and recovery](STANDALONE.md#human-intervention-and-restart-recovery) for the host-only API and macOS permission requirements.
