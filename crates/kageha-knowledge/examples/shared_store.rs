//! Disposable standalone use: no Kageha server, runtime or application database.
use kageha_knowledge::{
    scope::{Config, Scope},
    store::Store,
    Lexical, Retriever,
};
fn main() -> std::io::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("component/knowledge.sqlite");
    let mut store = Store::open(&path)?;
    store.create("example-workspace", "runbooks", "hierarchical")?;
    store.put(
        "example-workspace",
        "runbooks",
        "recovery.md",
        b"# Recovery\nRollback uses a verified checkpoint.",
        "text/plain",
        None,
    )?;
    let scope = Scope::resolve(
        "example-workspace",
        "example-agent",
        &Config {
            inherit: true,
            extras: vec![],
        },
        &["runbooks".into()],
        |id| store.binding("example-workspace", id),
    )?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let control = kageha_contracts::execution::Control {
        cancelled: &cancel,
        deadline: None,
    };
    let hits = Lexical.search(
        "example-agent",
        &store.documents(&scope, &control)?,
        "rollback checkpoint",
        5,
        &control,
    )?;
    assert!(!hits.is_empty());
    println!("{}", serde_json::to_string(&hits)?);
    Ok(())
}
