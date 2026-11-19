# Rust composition launcher

`kageha-harness` exposes the launcher as a library and a thin `kageha` executable.
It launches the existing Rust host directly, preserving signal delivery and worker
executable identity. It neither imports nor launches Python or Node.

```sh
kageha doctor --resources /path/to/resources --bin-dir /path/to/bin
kageha up --workspace /path/to/workspace --state-dir /path/to/private-state \
  --resources /path/to/resources --bin-dir /path/to/bin --bind 127.0.0.1:8788
kageha migrate --legacy-sessions /path/to/read-only-session-copy \
  --state-dir /path/to/private-rust-state --bin-dir /path/to/bin
kageha migrate --legacy-agents /path/to/read-only-agent-copy \
  --state-dir /path/to/private-rust-state --bin-dir /path/to/bin
kageha migrate --legacy-task-plans /path/to/read-only-session-copy \
  --state-dir /path/to/private-rust-state --bin-dir /path/to/bin
```

The default installed resource location is `bin/../share/kageha`, containing
`models.yaml` and `webui/index.html`. A resource override is explicit and must
resolve within its root. The host receives concrete resource paths; no checkout
or current-working-directory search is used by the launcher.

Supply a private token with `--token-file` or `KAGEHA_SERVER_TOKEN`. Otherwise a
private `server.token` is created once inside the private state directory. Tokens
are never printed. The launcher requires explicit workspace/state paths and does
not adopt the Python edition's stores. Browser/computer executables are reported
as optional installed components; their presence does not prove adapter readiness.

`doctor` checks files and executable availability without starting a service.
It is not the retained-feature release gate. `migrate` accepts exactly one legacy
domain per invocation: conversations/attachments, agents/revisions, or task plans.
Import conversations before their plans. It needs no installed WebUI,
provider configuration, credential helper, Python or Node. The destination must be
offline: it uses the same writer lock as the WebUI host. Repeat invocation verifies
existing records and resumes prepared sessions without duplicating the projection.
It reports its domain explicitly; credentials, worker execution journals, projects, memory,
settings, audits and other product-store migrations remain separate release work.
No rollback command is exposed yet. The original source remains the rollback input;
post-import writes require reconciliation before any later rollback.

Agent imports preserve retained IDs (including dots and up to 80 ASCII characters),
historical snapshot IDs, model/skill/tool/knowledge configuration, and the Default
agent's knowledge overlay. Existing destination identities cannot be overwritten.
Repeat import does not undo later edits, deletion or an explicit revision restore.
Non-empty risk overrides remain blocked until their retained native policy adapter
exists; imports never silently weaken them. Knowledge file/index transfer is separate.

Task plans retain source revisions, stage dependencies/statuses, required outcomes,
constraints and historical budgets. Their raw state is archived, not executed.
The projected plan is unapproved and requires review; old observations, tool evidence,
browser bindings and approvals are not carried into fresh authority. No worker runs
are admitted by importing a plan. A new turn receives bounded historical continuity
context and prior answers without the stale browser blocker. These answers are
historical facts, not renewed approval grants. Live budget continuation and restoration
of interrupted workers remain separate migration work.

`scripts/check_rust_installation.sh` tests relocated binaries, static hosting,
resource containment and persistent chat identity in disposable directories with
an empty backend PATH. This is a core-composition test, not the full production
bundle/platform certification.
