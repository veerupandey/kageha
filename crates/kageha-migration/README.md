# Restartable historical conversation import

This library owns its migration journal and content-addressed file snapshots. It
has no WebUI, provider, credential or application-database dependency. A host
implements `HistorySink` with an atomic projection-and-digest receipt transaction.

The importer pins the source directory, rejects path-component symlinks and bounds
history/attachment reads. Raw history and metadata snapshots preserve provenance;
only user/assistant conversation records are projected. Deleted-turn tombstones
and retained user inputs are honored. Unsupported or malformed records produce an
error requiring review; they are never silently counted as successful migration.

Each session follows prepared -> host projection/verification -> applied. A lost
acknowledgement leaves prepared state; a retry reaches the host's idempotent receipt
instead of repeating the projection. Sessions already applied are verified. Changed
or missing previous sources, damaged owned blobs and conflicting chat ownership
are errors. Importing another directory cannot overwrite an already-owned chat ID.
Unknown journal versions are rejected rather than downgraded.
The WebUI adapter also rejects removed projections instead of recreating deleted
chats; its legacy startup importer fences these receipts independently of path aliases.

Attachments are copied to private, durable storage before projection. They remain
available after the source disappears. Snapshot locations are local store paths;
moving an entire existing state directory or migrating all other product stores is
not implemented by this library. Keep destination state outside agent workspaces.
Raw snapshots, including deleted raw records retained for provenance, are private
operator data and are never registered as chat attachments by the projection.

Use a quiescent, read-only copy of historical sessions. Source checks catch changes
during reads and between preparations; they do not coordinate the legacy writer or
turn a concurrently changing directory into a transactional filesystem snapshot.
The source is never modified, locked, repaired or deleted by this importer.

Tests cover idempotency, interruption/resumption, lost projection acknowledgement,
source changes/deletions, owned-copy integrity, source preservation, tombstones,
path escape, writer exclusion and journal-version rejection. The WebUI projection
and CLI are tested separately through the existing APIs.

## Additional document domains

`Importer::run_documents(Domain, DocumentSink)` uses the same prepared/applied
journal mechanism. Agent YAML, `.history/<id>/*.yaml`, deleted-agent revision
archives and `.default-knowledge.yaml` are supported. Source revision names and
raw records are retained. Agent identity validation lives in shared contracts,
not in the UI or a vendor adapter. Tool policy validation and raise-only risk
overrides use the independent policy component. Raised risk requires an exact
one-use operator decision in the native execution adapter; malformed policy
values are rejected.

Task-plan snapshots read `task_state.json`, retaining goals, stage dependencies,
constraints, budgets, answered questions and historical tool state in the private
snapshot. The host projects a review-only plan, not a resumed process. It never
imports worker leases, approvals or browser authority from that JSON. Source IDs
use exact directory bytes and a domain namespace; lossy path names cannot alias.

The WebUI projection keeps imported agent revisions immutable while allowing later
native edits/deletion/restore. Required outcomes remain visible in plan review;
replacing a reviewed document cannot silently remove them. Full worker, knowledge,
policy and budget migration is not established by these document imports.

## Private knowledge sources

`Domain::Knowledge` consumes an explicit copy of the old knowledge root containing
`agent-<id>/manifest.yaml`, `raw/` and optional `chunks.jsonl`. Agent definitions
must be imported first. The Rust launcher exposes `--legacy-agent-knowledge`.
Only manifest-listed originals become current sources; orphan raw files/chunks
remain excluded from retrieval. Original bytes and extraction provenance are
retained in private content-addressed blobs. Source paths in old chunks are
resolved only by basename inside that KB's raw directory; external files and
symlinks are never followed.

Imported chunk IDs/headings/parent IDs survive. Text-only sources without chunks
can rebuild a lexical index. Binary sources lacking extracted chunks remain
explicitly unindexed. Vendor embeddings and indexes are derived state and are
not imported. The adapter reports lexical search rather than claiming semantic
parity. Missing active source bytes, ambiguous filenames, changed sources,
conflicting destinations and modified owned blobs stop import.

Bounds are 512 sources and 512 chunks per source, 16 MiB per input file and 64 MiB
per private store. These do not establish the old 500 MiB upload limit, remote
sources, external KBs, media extraction or vendor retriever parity. State-directory
relocation and post-write rollback remain outstanding migration requirements.
