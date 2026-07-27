//! No host, application database, model provider or WebUI required.
use kageha_skills::{registry::Registry, Package};
fn main() -> std::io::Result<()> {
    let root = std::env::temp_dir().join(format!("kageha-skills-embed-{}", std::process::id()));
    let result = (|| {
        let mut registry = Registry::open(&root.join("skills/registry.sqlite"))?;
        let skill = Package::plain(
            "source_reader",
            "Read the supplied evidence before answering.",
        )?;
        let first = registry.install(&skill, "embedded-example", Some(0))?;
        registry.change("source_reader", "pin", Some(first.revision))?;
        assert_eq!(
            registry.resource("source_reader", "SKILL.md")?,
            skill.files["SKILL.md"]
        );
        println!("Independent Rust skill registry passed");
        Ok(())
    })();
    std::fs::remove_dir_all(root)?;
    result
}
