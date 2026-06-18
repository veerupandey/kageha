# Kageha knowledge library

`Lexical` implements the public `Retriever` trait over portable `Document` and
`Chunk` records. Retrieval has no WebUI, application-database, model or vendor
client dependency. Hosts supply verified source hashes and explicit access scopes.
The retriever filters by owner and tombstones, honors cancellation/deadlines,
bounds work and returns stable chunk/source citations.

The optional `Store` API owns a separate SQLite database for workspace knowledge.
It retains source revisions, hashes and tombstones, uses transactional compare
and exchange for edits, and fences access using KB generations. Text sources use
fixed or Markdown-heading chunking. Metadata counts are derived caches, not another
source of truth. A source's original bytes remain available independently of its
index. The store never opens Kageha's application database.

`Scope::resolve` combines selected workspace bindings and configured extra KBs
only when inheritance is enabled. Private mode performs no shared lookup.
`Scope::delegate` can narrow inherited access; a worker definition cannot add a
shared base the parent did not attach. The WebUI pins the resolved scope into each
run, rather than re-resolving mutable settings during recovery. Store reads check
current generations and tombstones. Deleting and recreating a base with the same
name does not authorize a previously pinned run to read the new base.

Run independent disposable embedding examples:

```sh
cargo run -p kageha-knowledge --example retrieve
cargo run -p kageha-knowledge --example shared_store
```

The current implementation uses lexical BM25 ranking. It does not implement
semantic/graph/remote/media retrieval, vendor-index import, or service replacement
lifecycle conformance. The WebUI rejects unsupported binary/media ingestion;
`capabilities()` reports actual retrieval behavior. Private agent document storage
still uses a compatibility projection; historical shared KB import, portable
workspace relocation and complete store export/import/rollback remain release gaps.
