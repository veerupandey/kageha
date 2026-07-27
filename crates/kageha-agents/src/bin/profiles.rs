//! Offline operator commands. The same repository/ports are used by embeddings
//! and the runtime. No WebUI, Python or running application DB is required.
use kageha_agents::repository::{sqlite::SqliteRepository, Bundle, Kind, Repository};
use std::{
    io::{self, Read},
    path::Path,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        return Err("usage: kageha-agent-profiles DB SCOPE export|list|import|write|delete|restore SOURCE_OR_KIND [ID EXPECTED FILE_OR_REVISION]".into());
    }
    let repo = SqliteRepository::open(Path::new(&a[1]), &a[2])?;
    let read = |p: &str| -> io::Result<Vec<u8>> {
        let mut b = vec![];
        std::fs::File::open(p)?
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut b)?;
        if b.len() > 64 * 1024 * 1024 {
            return Err(io::Error::other("Import exceeds limit"));
        }
        Ok(b)
    };
    let v = match a[3].as_str() {
        "export" => serde_json::to_value(repo.export()?)?,
        "list" => serde_json::to_value(repo.entries()?)?,
        "import" => {
            let raw: serde_json::Value =
                serde_json::from_slice(&read(a.get(4).ok_or("Bundle file required")?)?)?;
            let b: Bundle = serde_json::from_value(raw.get("bundle").unwrap_or(&raw).clone())?;
            let s = repo.import(&b, 4096)?;
            serde_json::to_value(s)?
        }
        op @ ("write" | "delete" | "restore") => {
            let k = match a.get(4).ok_or("Kind required")?.as_str() {
                "agent" => Kind::Agent,
                "team" => Kind::Team,
                _ => return Err("Invalid kind".into()),
            };
            let id = a.get(5).ok_or("ID required")?;
            let n = a.get(6).ok_or("Expected revision required")?.parse()?;
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs_f64();
            serde_json::to_value(if op == "restore" {
                repo.restore(k, id, n, a.get(7).ok_or("Revision required")?.parse()?, t)?
            } else {
                repo.write(
                    k,
                    id,
                    n,
                    if op == "delete" {
                        None
                    } else {
                        Some(serde_json::from_slice(&read(
                            a.get(7).ok_or("Document file required")?,
                        )?)?)
                    },
                    t,
                )?
            })?
        }
        _ => return Err("Unknown profile operation".into()),
    };
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}
