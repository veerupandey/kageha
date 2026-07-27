//! Any framework can supply profile data through the same port.
use kageha_agents::{Catalog, Definition, Port};
use serde_json::json;
struct Alternative(Definition);
impl Port for Alternative {
    fn get(&self, id: &str) -> std::io::Result<Option<Definition>> {
        Ok((id == self.0.id()).then(|| self.0.clone()))
    }
    fn list(&self) -> std::io::Result<Vec<Definition>> {
        Ok(vec![self.0.clone()])
    }
}
fn use_profiles(port: &dyn Port) -> std::io::Result<String> {
    let definitions = kageha_agents::list(port)?;
    let d = kageha_agents::get(port, definitions[0].id())?.unwrap();
    d.require(&Default::default())?;
    assert!(kageha_policy::specialist::evaluate(&[d.policy().clone()], "read_file")?.allowed);
    assert!(!kageha_policy::specialist::evaluate(&[d.policy().clone()], "read_secret")?.allowed);
    Ok(d.fingerprint())
}
fn main() -> std::io::Result<()> {
    let definition = Definition::parse(
        json!({"id":"research.ai","name":"Research","system_prompt":"Respect the assignment","tool_policy":{"allow":["read_*"],"deny":["read_secret"]}}),
    )?;
    assert_eq!(
        use_profiles(&Catalog::new(vec![definition.clone()])?)?,
        use_profiles(&Alternative(definition))?
    );
    println!("Independent profile adapters passed");
    Ok(())
}
