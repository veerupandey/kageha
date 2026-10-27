//! Read-only provider discovery using an explicitly selected source. Never prints keys.
use kageha_contracts::execution::Control;
use kageha_models::catalog::{Catalog, Environment};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, sync::atomic::AtomicBool};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: audit_catalog WORKSPACE ENV_FILE".into());
    }
    let workspace = PathBuf::from(&args[0]);
    let env = Environment::load(&[PathBuf::from(&args[1])])?;
    let catalog = Catalog::load(&kageha_models::catalog::default_paths(&workspace), |name| {
        env.file_value(name).ok().flatten()
    })?;
    let mut groups: BTreeMap<(String, String, String), Vec<serde_json::Value>> = BTreeMap::new();
    for row in catalog.models {
        groups
            .entry((
                row["provider"].as_str().unwrap_or("").into(),
                row["base_url"].as_str().unwrap_or("").into(),
                row["protocol"].as_str().unwrap_or("").into(),
            ))
            .or_default()
            .push(row);
    }
    let results: Vec<_> = std::thread::scope(|scope| {
        let tasks:Vec<_>=groups.into_iter().map(|((provider,base,protocol),rows)|{let env=&env;scope.spawn(move||{
            if !kageha_models::catalog::supported(&protocol){return json!({"provider":provider,"protocol":protocol,"status":"unsupported","configured":rows.len()});}
            let key_name=rows[0]["api_key_env"].as_str().unwrap_or("");
            let key=zeroize::Zeroizing::new(kageha_models::catalog::credential(key_name,|name|env.file_value(name).ok().flatten()));
            if !key_name.is_empty()&&key.is_empty(){return json!({"provider":provider,"status":"missing_credential","configured":rows.len()});}
            let cancelled=AtomicBool::new(false);
            match kageha_models::discover_models(&base,&key,&protocol,&Control{cancelled:&cancelled,deadline:None}){
                Ok(available)=>{let ids:std::collections::BTreeSet<_>=available.iter().filter_map(|r|r["id"].as_str()).collect();let absent:Vec<_>=rows.iter().filter(|r|!ids.iter().any(|id|id.strip_prefix("models/").unwrap_or(id).eq_ignore_ascii_case(r["model"].as_str().unwrap_or("")))).map(|r|r["id"].clone()).collect();json!({"provider":provider,"protocol":protocol,"status":"discovery_passed","configured":rows.len(),"discovered":ids.len(),"advertised_models":ids,"not_advertised":absent})},
                Err(e)=>json!({"provider":provider,"protocol":protocol,"status":"discovery_failed","configured":rows.len(),"error":e.to_string()})
            }
        })}).collect();
        tasks
            .into_iter()
            .map(|t| t.join().unwrap_or(json!({"status":"worker_failed"})))
            .collect()
    });
    println!("{}", json!({"read_only":true,"providers":results}));
    Ok(())
}
