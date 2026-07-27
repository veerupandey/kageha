//! The same consumer operates independent memory and durable adapters.
use kageha_agents::repository::{Kind, MemoryRepository, Repository};
use serde_json::json;
fn exercise(repo: &dyn Repository) -> std::io::Result<()> {
    repo.write(Kind::Agent,"reviewer",0,Some(json!({"id":"reviewer","name":"Reviewer","system_prompt":"Verify evidence before completion"})),1.0)?;
    assert_eq!(
        repo.get("reviewer")?.unwrap().instructions(),
        "Verify evidence before completion"
    );
    repo.write(Kind::Agent, "reviewer", 1, None, 2.0)?;
    assert!(repo.get("reviewer")?.is_none());
    repo.restore(Kind::Agent, "reviewer", 2, 1, 3.0)?;
    let target = MemoryRepository::new("independent-import")?;
    assert!(target.import(&repo.export()?, 4096)?.complete());
    assert_eq!(target.entries()?, repo.entries()?);
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    exercise(&MemoryRepository::new("embedded")?)?;
    #[cfg(feature = "sqlite")]
    {
        let dir = tempfile::tempdir()?;
        exercise(&kageha_agents::repository::sqlite::SqliteRepository::open(
            &dir.path().join("profiles.sqlite"),
            "durable",
        )?)?;
    }
    println!("Independent profile repositories passed");
    Ok(())
}
