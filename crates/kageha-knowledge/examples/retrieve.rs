use kageha_knowledge::{Chunk, Document, Lexical, Retriever};
fn main() -> std::io::Result<()> {
    let document = Document {
        id: "runbook".into(),
        owner: "agent-one".into(),
        source: "runbook.md".into(),
        sha256: "fixture".into(),
        tombstone: false,
        chunks: vec![Chunk {
            id: "step-1".into(),
            text: "Rollback uses the last verified checkpoint".into(),
            heading: "Recovery".into(),
            parent_id: None,
        }],
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let hits = Lexical.search(
        "agent-one",
        &[document],
        "rollback checkpoint",
        5,
        &kageha_contracts::execution::Control {
            cancelled: &cancel,
            deadline: None,
        },
    )?;
    assert_eq!(hits[0].document_id, "runbook");
    println!("{}", serde_json::to_string(&hits)?);
    Ok(())
}
