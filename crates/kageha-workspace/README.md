# Independent Rust workspace library

This package is a public Rust library with a thin `kageha-workspace` executable.
It has no dependency on Kageha's runtime, WebUI, server, database, model providers,
Python or Node. Another framework can supply its own ownership, approvals and
execution host and use the same implementation.

## Embed

```rust,no_run
use kageha_workspace::{ReadWorkspace, DocumentWorker};
use kageha_contracts::execution::Control;
use std::{path::Path, sync::atomic::AtomicBool};
# fn main() -> std::io::Result<()> {
let cancelled = AtomicBool::new(false);
let control = Control { cancelled: &cancelled, deadline: None };
let workspace = ReadWorkspace::new(Path::new("./project"))?;
let text = workspace.read_text("notes.txt", &control)?;
// Worker executable is selected by the host, never by tool arguments.
let worker = DocumentWorker::new(Path::new("./target/debug/kageha-workspace"))?;
let snapshot = workspace.snapshot("report.pdf", 16 * 1024 * 1024, &control)?;
let report = worker.extract(snapshot.bytes, "pdf", &control)?;
# Ok(()) }
```

`ReadWorkspace` implements the shared `ToolPort` for `read_file`. Optional
`DocumentTools` implements `read_document` using a host-selected worker. Both can
be composed through `kageha-tools::Composite` by any host. The runtime-service
compatibility export of `ReadWorkspace` remains available.

## Boundaries

- A directory descriptor pins the root. Internal symlinks are resolved within that
  root, then each component is opened relative to the descriptor without following
  replacement symlinks. Absolute paths must still resolve inside the configured root.
- Regular files only: directories, devices, FIFOs and socket files cannot become
  blocking read targets. Text reads keep the retained 64 KiB limit.
- Authorized dispatch checks file/root identity on the descriptor actually read.
  A file replaced after approval is rejected. File metadata is checked across a read.
- `file_snapshot` is for an already-authorized exact file. It does not infer chat
  ownership. The host can require an expected SHA-256; mismatched bytes are rejected.
- `DocumentWorker` receives the verified byte snapshot on stdin, avoiding a later
  path reopen. Input is capped at 16 MiB, extracted output at 1 MiB, and execution
  at 20 seconds (or a shorter caller deadline).
- Workers parse PDF, DOCX, PPTX, XLSX/XLSM, XML, HTML and UTF-8 text using the
  retained Rust parser implementation. Office entry counts and actual decompressed
  byte totals are bounded. Image-only/encrypted documents still need OCR/password
  support; the adapter does not advertise those features.
- Dedicated worker entry points apply CPU/address-space limits where supported.
  Managed requirements for an OS process sandbox are enforced, with no fallback
  when required support is missing. Worker environments do not inherit credentials.
- `process::run` handles simultaneous stdin/stdout with bounded nonblocking I/O,
  cancellation, deadlines and process-group cleanup. It does not launch a shell or
  create detached I/O threads. The unreaped leader pins group identity until
  descendants are terminated. Output diagnostics report byte counts, not payloads.

The process primitive accepts a host-configured `Command`; it is **not** an
agent-facing arbitrary-command execution tool or approval bypass. Long-lived MCP
connections use their separate protocol/session transport. Existing HTTP wrappers
still live in their host; this extraction does not claim all curl clients migrated.

## Run and test

```sh
cargo build --locked -p kageha-workspace
./target/debug/kageha-workspace read ./project notes.txt
./target/debug/kageha-workspace extract-document ./report.pdf
cargo test --locked -p kageha-workspace
```

`extract-document-stdin EXT` is the worker protocol. Backend execution does not
require a shell, Python, Node, Office or unzip. Tests may use native shell/zip
fixtures as external test drivers. Both Linux and macOS are the current targets.

The WebUI delegates ordinary file reads, chat-owned document extraction and
subprocess I/O here while retaining its ownership tables, uploads, routes and error
compatibility. The standalone runtime enables `read_document` only when
`workspace_worker` is configured. Artifact/worktree persistence, complete local/
remote execution adapters, remote workspace hosting and managed replacement remain.


`projects::Projects` is an independent owned catalog for project records, revision
history, chat assignments and tagged turns. Callers supply an owner namespace and
validate chat ownership before binding. Concurrent edits can require a revision;
project creation, tags and chat bindings survive restart. Ambiguous name resolution
requires an exact ID. This catalog does not imply Git worktree, project-brain parity. Historical project catalogs and chat tags have an explicit
portable import adapter; this does not establish full workspace migration parity. Run `scripts/check_projects.sh` for
component tests and existing WebUI API acceptance without Python.


`projects::import::Archive` and `Projects::import_archive` provide portable catalog
import independently of Kageha's server. Immutable source receipts commit with
project history, chat bindings and tags; retrying cannot undo later native changes.
The application adapter uses the common migration coordinator and checks chat
existence before the initial import. The copied source is never rewritten.


`brain::Brain` reads bounded trusted workspace instructions, scoped rules and
explicit recipes as a library. `Brain::render` selects glob rules for touched
paths; `Brain::command` returns recipe text without executing it. Hosts retain
snapshots in their own run/checkpoint protocols. Neither recipes nor instruction
files grant tool authority. Worktree mutation and hook execution are separate
retained capabilities and are not supplied by this read-only API.

## Git worktrees and lifecycle hooks

`worktrees::{Git, Worktrees}` provides repository inspection and journaled worktree
creation. The host chooses a trusted absolute Git executable and root; it owns the
journal path. Request IDs bind to the original root, label and base. A dispatched
operation with an unknown outcome fences further creation in that repository.
`reconcile` only inspects the expected path, branch, commit and clean status; it
never repeats checkout. Other repositories remain usable.

Git hooks, fsmonitor, submodule recursion and configured checkout filters are
disabled. Git must be installed explicitly (`KAGEHA_GIT_BIN` in the WebUI host,
default `/usr/bin/git`). Filter-dependent repositories and stronger protection against concurrent hostile filesystem replacement remain
outside this scoped implementation.

`hooks::{Set, Executor}` parses declarative hook configurations independently.
The host implements authorization, deadlines and durable execution receipts. Run
`cargo run -p kageha-workspace --example hooks` for an embedding example without
WebUI, application databases or a model provider. Static denial never executes a
command. Configuration, payloads, responses and combined context are bounded.

The existing WebUI snapshots hooks at turn admission and wires `sessionStart`,
`preToolUse`, `beforeShell` and `postToolUse`. Command hooks use `/bin/sh` with a
scrubbed environment; HTTP hooks use bounded authenticated-policy-controlled
transport. Approval wait does not consume the hook execution deadline. Unknown
outcomes stop further admission and remain fenced across restart; explicit operator
acknowledgment records uncertainty rather than success. Confirmed base-tool results
survive a later post-hook failure without repeating the base action.

`GET /api/project` reports actual configured hooks and whether each event is wired.
Other lifecycle events, plugin hook loading, full stderr diagnostics, automatic
worker settlement and service-only/channel compositions still need retained-feature parity.
Run `scripts/check_workspace_lifecycle.sh` for disposable component, crash/restart,
approval, existing WebUI API and recovery-panel acceptance. No live chats or user
repositories are mutated by this test lane.

Managed cleanup uses the original completed creation receipt, not an arbitrary
path. `Worktrees::cleanup` retains checkouts containing tracked, untracked, ignored
or committed work. It never forces removal. Checkout removal and branch deletion
have separate durable dispatch stages; branch deletion compares the original
commit. `reconcile_cleanup` performs observation only. After it proves checkout
removal, an explicit continuation may dispatch the branch step. Unknown steps
cannot be replayed under the same or a fresh request ID in that repository.

The WebUI lists pending cleanup receipts, offers **Remove unchanged** only for
managed worktrees, and exposes separate check/resume controls. Automatic specialist
worktree setup/settlement and cross-service workspace replacement remain to be
wired; operator cleanup does not establish those capabilities. Concurrent external
writers must be quiesced before cleanup; OS-level protection against a hostile
filesystem replacement or ignored-file writer during Git removal remains a gap.

## Specialist workspace policy

`worker::Options` is a vendor-neutral admission policy (`shared` or `worktree`,
`keep_worktree`, `base_ref`). Hosts can combine it with `Worktrees` in any framework.
The policy and Git library do not import the runtime, WebUI or application store.

Kageha's existing `/api/jobs` and `agent_spawn` accept these options. Shared remains
compatible with old requests; `isolation: "worktree"` requires a Git project and
pins a durable creation identity before a worker can contact its model. Missing
Git, an unknown creation receipt or a missing checkout fails without selecting a
shared root. Nested workers inherit isolation and cannot request the shared source.

File tools and lifecycle hooks use the worker checkout. Knowledge, credential and
approval namespaces retain the source scope. Git worktrees are filesystem working
contexts, not OS process sandboxes: integration tools retain their own policy and
approved hooks retain their host authority. Worker status/session-state APIs expose
the actual checkout, branch, creation/cleanup receipts and retained state.

`keep_worktree` defaults to true. Successful runs may opt into unchanged-checkout
cleanup. Cancelled, failed and interrupted attempts retain their checkout for safe
continuation; a continuation reuses its original creation identity. Changed output
is retained by the cleanup library. Repository Git mutations queue independently
of worker model/tool execution, with bounded, cancellable admission.

Run `scripts/check_worker_worktrees.sh` for the real-host acceptance lane. Standalone
runtime-service worker orchestration and hosted workspace replacement still need
full parity. Backend tests run without Python on the host PATH.

Worktree creation and worker admission require the configured source to be the
actual Git top-level project directory. A subtree source is rejected because a
whole-repository checkout would expand its file capability. Bare repositories do
not advertise an available working checkout.

`worker::Manager` now owns the reusable worker checkout lifecycle as a public
library. Both the WebUI and standalone runtime service delegate to it, including
its bounded admission queue and observation-only reconciliation. Consumers supply
source, journal and trusted Git executable paths; the component owns its journal.
The service's replaceable `WorkspacePort` can instead supply non-filesystem tools,
which is checked by an independent virtual-adapter conformance test.
