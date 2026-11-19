mod cutover;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};
fn walk(root: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if !matches!(
                entry.file_name().to_str(),
                Some("node_modules" | "__pycache__" | "target" | "dist" | ".git")
            ) {
                walk(&path, files)?;
            }
        } else if path
            .extension()
            .is_some_and(|e| matches!(e.to_str(), Some("py" | "rs" | "tsx" | "ts" | "toml")))
        {
            files.push(path);
        }
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let cmd = std::env::args().nth(1).unwrap_or_default();
    if cmd == "cutover-audit" {
        let audit = cutover::audit(root)?;
        println!("{}", serde_json::to_string_pretty(&audit)?);
        if audit["python_free_backend_ready"] != true {
            return Err("Python-free structural cutover is incomplete".into());
        }
        return Ok(());
    }
    if cmd == "inventory" {
        let mut files = Vec::new();
        walk(&root.join("crates"), &mut files)?;
        files.sort();
        let mut inventory = Vec::new();
        for file in files {
            let bytes = std::fs::read(&file)?;
            let source = String::from_utf8_lossy(&bytes);
            let entries = source
                .lines()
                .enumerate()
                .filter(|(_, line)| {
                    line.contains("/api/")
                        || line.contains("@app.command")
                        || line.contains("@tool")
                        || line.contains("pub trait ")
                })
                .map(|(i, line)| json!({"line":i+1,"declaration":line.trim()}))
                .collect::<Vec<_>>();
            if !entries.is_empty() {
                inventory.push(json!({"path":file.strip_prefix(root)?.to_string_lossy(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"entries":entries,"verification":"unclassified"}));
            }
        }
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"schema_version":1,"status":"inventory_only_not_parity_evidence","sources":inventory})
            )?
        );
        return Ok(());
    }
    if cmd == "boundaries" {
        // Check declared dependency direction as well as source imports. Do not
        // allow a new transport/vendor client to slip into the turn kernel.
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let output = std::process::Command::new(cargo)
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(root)
            .output()?;
        if !output.status.success() {
            return Err("Cargo metadata failed".into());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        for package in metadata["packages"].as_array().ok_or("missing packages")? {
            let name = package["name"].as_str().unwrap_or("");
            if matches!(
                name,
                "kageha-contracts"
                    | "kageha-runtime"
                    | "kageha-services"
                    | "kageha-memory"
                    | "kageha-runtime-store"
                    | "kageha-policy"
                    | "kageha-agents"
                    | "kageha-models"
                    | "kageha-credentials"
                    | "kageha-mcp"
                    | "kageha-tools"
                    | "kageha-browser-client"
                    | "kageha-workspace"
                    | "kageha-execution"
                    | "kageha-integrations"
                    | "kageha-migration"
                    | "kageha-knowledge"
                    | "kageha-skills"
                    | "kageha-resources"
                    | "kageha-agent-sdk"
                    | "kageha-runtime-service"
            ) {
                for dependency in package["dependencies"]
                    .as_array()
                    .ok_or("missing dependencies")?
                {
                    if dependency["kind"] == "dev" {
                        continue;
                    }
                    let dep = dependency["name"].as_str().unwrap_or("");
                    if name == "kageha-agents"
                        && !matches!(dep, "kageha-contracts" | "kageha-policy" | "serde" | "serde_json" | "sha2" | "rusqlite" | "fs2" | "libc")
                    {
                        return Err(format!("agent definitions must not depend on runtime, application or vendor clients: {dep}").into());
                    }
                    if name == "kageha-models"
                        && !matches!(
                            dep,
                            "kageha-contracts"
                                | "serde"
                                | "serde_json"
                                | "serde_yaml"
                                | "dotenvy"
                                | "zeroize"
                                | "sha2"
                                | "tempfile"
                                | "tokio"
                                | "reqwest"
                        )
                    {
                        return Err(format!("model adapter must depend on contracts, not runtime/application consumers: {dep}").into());
                    }
                    if matches!(dep, "kageha-loop" | "kageha-ui")
                        || matches!(name, "kageha-contracts" | "kageha-runtime")
                            && !(matches!(
                                dep,
                                "serde"
                                    | "serde_json"
                                    | "async-trait"
                                    | "thiserror"
                                    | "zeroize"
                                    | "kageha-contracts"
                            ) || (name == "kageha-runtime" && dep == "sha2"))
                    {
                        return Err(format!("forbidden dependency: {name} -> {dep}").into());
                    }
                }
            }
        }
        for name in [
            "kageha-contracts",
            "kageha-memory",
            "kageha-services",
            "kageha-runtime",
            "kageha-runtime-store",
            "kageha-policy",
            "kageha-agents",
            "kageha-models",
            "kageha-mcp",
            "kageha-tools",
            "kageha-browser-client",
            "kageha-workspace",
            "kageha-execution",
            "kageha-integrations",
            "kageha-migration",
            "kageha-resources",
            "kageha-agent-sdk",
            "kageha-runtime-service",
        ] {
            let mut files = Vec::new();
            walk(&root.join("crates").join(name).join("src"), &mut files)?;
            for file in files {
                let s = std::fs::read_to_string(&file)?;
                if s.contains("crate::webui") || s.contains("kageha_loop") || s.contains("ui_store")
                {
                    return Err(
                        format!("forbidden application dependency: {}", file.display()).into(),
                    );
                }
            }
        }
        println!("Component dependency boundaries pass");
        return Ok(());
    }
    Err("usage: cargo run -p xtask -- inventory | boundaries | cutover-audit".into())
}
