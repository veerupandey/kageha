//! Explicit, bounded operator probe. Does not print keys or model response text.
use kageha_contracts::execution::{Control, ModelPort, ModelRequest};
use kageha_models::{
    catalog::{Catalog, Environment},
    HttpModel,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: probe_model WORKSPACE ENV_FILE MODEL_ID".into());
    }
    let workspace = PathBuf::from(&args[0]);
    let env = Environment::load(&[PathBuf::from(&args[1])])?;
    let lookup = |name: &str| env.file_value(name).ok().flatten();
    let catalog = Catalog::load(&kageha_models::catalog::default_paths(&workspace), lookup)?;
    let result = catalog
        .provider(&args[2], lookup)
        .and_then(|p| HttpModel::new(vec![p]))
        .and_then(|model| {
            model.complete(
                ModelRequest {
                    messages: vec![json!({"role":"user","content":"Reply with OK."})],
                    tools: vec![],
                    role: Default::default(),
                },
                &Control {
                    cancelled: &AtomicBool::new(false),
                    deadline: Some(Instant::now() + Duration::from_secs(20)),
                },
            )
        });
    match result {
        Ok(_) => println!("{}", json!({"model":args[2],"passed":true})),
        Err(e) => println!(
            "{}",
            json!({"model":args[2],"passed":false,"error":e.to_string()})
        ),
    };
    Ok(())
}
