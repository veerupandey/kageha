# kageha-skills

Independent Rust library for declarative skill manifests, bounded resource packages,
and a component-owned SQLite lifecycle journal. It does not depend on the runtime,
WebUI or application database. Skills are instructions, never an authority grant.

```rust,no_run
use kageha_skills::{Package, registry::Registry};
let mut registry = Registry::open(std::path::Path::new("state/skills/registry.sqlite"))?;
let skill = Package::plain("source_reader", "Read evidence before answering.")?;
let record = registry.install(&skill, "operator", Some(0))?;
registry.change("source_reader", "pin", Some(record.revision))?;
# Ok::<(), std::io::Error>(())
```

`Package::read` copies the entire package through a pinned workspace reader, rejects
symlinks, bounds traversal/files/bytes and validates declared required resources.
`Registry::edit` retains supporting resources and uses revision checks. Immutable
versions, hashes and tombstones survive restart. Legacy adoption is repeatable and
cannot overwrite a native identity or undo subsequent edits/deletions.

Run `cargo run -p kageha-skills --example embed` independently, or
`bash scripts/check_skills.sh` for library and Python-free WebUI acceptance.

The compatibility adapter retains existing skill endpoints and adopts old
application records without changing their source tables. Installed resources are
addressed as `skills/NAME/path`. Restricted agents can only read their configured
skills; archived/deleted skills cannot supply tool resources. Disabled implicit
invocation is honored. Python helper requirements are reported as unavailable,
not executed through a hidden interpreter.

Remaining retained behavior includes remote/catalog installation and updates,
bundled helper ports, path/trigger discovery, explicit invocation plumbing,
authenticated peer sharing, learning/verification workflows, service hosting,
and coordinated live adapter replacement. This library alone does not establish
full skills parity or complete the Rust migration.
