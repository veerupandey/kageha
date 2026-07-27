# Kageha agent definitions

Portable specialist settings, validation, effective configuration and configuration
fingerprints. This library depends on contracts and policy; it does not depend on
WebUI, application persistence, model providers or the runtime host.

`Definition::parse` retains unknown metadata while validating the supported document
shape. `Catalog` is an immutable implementation of `Port`. Hosts can supply another
port without changing runtime consumers. Adapter results are checked for identity
and duplicate definitions. Run the independent example with:

```sh
cargo run -p kageha-agents --example profiles
```

The standalone runtime accepts `agent_profiles` in its existing JSON configuration
and composes the port through `workers::Composition::new_with_profiles`. Existing
`specialists` configuration remains compatible. Instructions and complete profile
fingerprints are pinned at admission. Wildcard policy restrictions and risk
escalations intersect with ancestor authority; children cannot widen access or
lower approval requirements. Configuration changes fence affected active runs.

Stored settings are not execution capabilities. Standalone activation currently
supports instructions and tool policy. Skill selection, provider preferences and
nondefault knowledge selection are retained in documents but rejected before run
admission until the host has the corresponding execution ports. The public worker
catalog reports those unsupported settings explicitly. The WebUI delegates validation and profile/team CRUD/history persistence to this
library through its application adapter.

`Port` is a configuration interface, not a managed live-replacement implementation.
The owned repository is described below. Full settings activation and managed
replacement remain migration work. `scripts/check_specialist_profiles.sh` covers independent
adapters, standalone execution, approval/replay semantics and existing WebUI flows.

## Owned revisions and portable import

`repository::Repository` adds optimistic writes, durable deletions, append-only
restoration, history export and checkpointed import to the read-only profile port.
`MemoryRepository` and the optional `sqlite::SqliteRepository` implement the same
consumer interface. Enable `sqlite` for the dedicated, scope-bound, private journal
and standalone operator executable. A writer lease rejects another process using
the store. Reopening with a different scope or an unrelated application database
fails. The SQLite schema is versioned independently of application storage.

Every write supplies the expected head revision (`0` for creation). Deletion adds
a tombstone. Restoring an old revision appends a new head; it never rewrites history.
Exports include agents, teams, revisions, timestamps and original revision aliases.
Imports commit one record and its checkpoint together, reject changed source
fingerprints and never overwrite an existing identity. Resumption skips committed
records; repeating a finished import cannot erase later edits or resurrect deletion.
The source is untouched. This is cold data migration, not managed live replacement
or incremental catch-up after source edits.

The existing WebUI exposes an authenticated, read-only compatibility export at
`GET /api/agents/_portable/export`. The export reads the owned repository. At first start, the application adapter
imports legacy settings/history without removing their source tables. Historical
deletions retain an unknown (`null`) deletion time. Conflicting source records
block migration rather than discarding data.
The agents library itself has no dependency on application tables or WebUI types.

```sh
cargo run -p kageha-agents --features sqlite --example repository
cargo build -p kageha-agents --features sqlite --bin kageha-agent-profiles
kageha-agent-profiles /data/profiles.sqlite workspace-A import /data/webui-export.json
kageha-agent-profiles /data/profiles.sqlite workspace-A export
kageha-agent-profiles /data/profiles.sqlite workspace-A delete agent reviewer 4
kageha-agent-profiles /data/profiles.sqlite workspace-A restore agent reviewer 5 2
```

`write agent ID EXPECTED FILE` accepts a complete definition. Operator writes are
offline while a service owns the store; embeddings can use the same live repository
instance, where runtime configuration fingerprints fence changed authority. No
profile import grants tool approval or activates unsupported skills/model settings.

`scripts/check_agent_repository.sh` exercises both adapters, actual importer process
death, restart/resumption, concurrent revision conflicts, source preservation,
WebUI export and persistent standalone execution without a Python backend.

## WebUI store cutover

The WebUI composition uses `components/agents/profiles.sqlite` beneath its state
directory. The application database retains only the frozen legacy profile tables
and a stable repository identity/cutover reference. All profile/team creation,
updates, deletion, history and restoration use the owned component, as do agent
reads for channel execution. Existing `/api/agents` and `/api/teams` shapes and
built-in protections are preserved. Optional `expected_revision` fences concurrent
writes; clients that omit it retain serialized compatibility behavior.

Bootstrap is restartable and preserves imported revision prefixes and aliases.
Legacy import retries remain idempotent after owned edits or deletion. New legacy
agent imports after cutover are rejected; use portable repository import for new
profiles. Changed frozen source data, missing owned files/checkpoints and missing
imported history block startup with a recovery error. They do not trigger stale
reconstruction. Back up the application state and component directories together;
restoring an older component backup is not a managed rollback after newer writes.
The persisted identity survives moving the state directory. Service readiness is
reported after successful component initialization.

`scripts/check_agent_store_cutover.sh` tests the same WebUI, historical migration,
provider/channel integration and owned storage without Python. Managed live service
replacement, full standalone settings activation and final deployment qualification
remain separate release gates.

The portable export response also includes `repository.scope`, so an authorized
operator can address the owned store without inspecting application tables. Stop
the service before using the offline operator CLI with that scope. Importing new,
disjoint portable identities leaves the original bootstrap checkpoint intact.
