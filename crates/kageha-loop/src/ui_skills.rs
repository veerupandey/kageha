//! Thin compatibility adapter over the independently owned declarative registry.
use crate::{
    server,
    ui_store::{self as store, db_error, ApiResult},
    webui::{self, Database},
};
use kageha_skills::{
    registry::{Record, Registry},
    Package,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use tiny_http::{Method, Request};
pub(crate) fn handles(path: &str) -> bool {
    matches!(
        path,
        "/api/extensions/skills"
            | "/api/skills/install"
            | "/api/knowledge/skill"
            | "/api/knowledge"
    )
}
/// The Host's skill registry, with legacy skills adopted.
pub(crate) fn owned(db: &Database) -> ApiResult<Registry> {
    let (path, legacy) = {
        let db = db.lock().map_err(db_error)?;
        let path = Path::new(
            db.path()
                .ok_or((500, "Persistent skill state required".into()))?,
        )
        .to_owned();
        let legacy = store::list(&db, "skill").map_err(db_error)?;
        (path, legacy)
    };
    let mut registry = Registry::open(
        &path
            .parent()
            .unwrap()
            .join("components/skills/registry.sqlite"),
    )
    .map_err(db_error)?;
    let source = format!("native-ui:{}", path.display());
    for value in legacy {
        let name = store::text(&value, "name", 64)?;
        let content = value["content"]
            .as_str()
            .ok_or((409, "Legacy skill instructions are missing".into()))?;
        let package = Package::plain(name, content).map_err(|e| (409, e.to_string()))?;
        if package.skill.name != name {
            return Err((
                409,
                "Legacy skill identity conflicts with its manifest".into(),
            ));
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&value).map_err(db_error)?)
        );
        registry
            .adopt_with_snapshot(
                &source,
                &digest,
                &package,
                value["pinned"] == true,
                value["archived"] == true,
                &value,
            )
            .map_err(|e| (409, e.to_string()))?;
    }
    Ok(registry)
}
fn row(registry: &Registry, record: Record) -> ApiResult<Value> {
    let text = registry_instruction(registry, &record)?;
    Ok(
        json!({"name":record.skill.name,"description":record.skill.description,"content":text,"revision":record.revision,"content_sha256":record.skill.content_sha256,"package_sha256":record.package_sha256,"origin":record.source,"source":record.source,"plugin":"","plugin_enabled":true,"editable":true,"can_pin":true,"pinned":record.pinned,"archived":record.archived,"peer_shareable":record.required_runtimes.is_empty()&&record.skill.peer_shareable(),"required_runtimes":record.required_runtimes,"allowed_tools":record.skill.allowed_tools,"triggers":record.skill.triggers,"paths":record.skill.paths,"disable_model_invocation":record.skill.disable_model_invocation,"degraded":record.degraded,"restored_from":record.restored_from}),
    )
}
fn registry_instruction(registry: &Registry, record: &Record) -> ApiResult<String> {
    // Archived skills are still editable/listable; their package snapshot is in
    // the version journal. Ordinary tool resource reads remain active-only.
    registry.instructions(&record.skill.name).map_err(db_error)
}
fn in_scope(persona: &Value, name: &str) -> bool {
    persona["id"] == "main" || persona["skills_restricted_to_pinned"] != true
        || persona["pinned_skills"].as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == name))
}

/// Helper runtimes the `shell` tool provides through the Toolchain_Cache. A skill that
/// needs another runtime stays unavailable.
const SHELL_RUNTIMES: &[&str] = &["python", "node", "bash"];

/// Whether the turn can run the helper scripts of `record`: it needs none, or `shell`
/// is in the turn's tool list (`shell`) and provides every runtime it needs.
pub(crate) fn runtimes_available(record: &Record, shell: bool) -> bool {
    record.required_runtimes.iter()
        .all(|runtime| shell && SHELL_RUNTIMES.contains(&runtime.as_str()))
}

/// Whether `record` is offered to the model in the catalog and discovery: active, model
/// invocation allowed, runnable in this turn, in the agent's scope, and not degraded
/// (Requirement 16.5). Degraded skills stay listed for the user.
fn suggested(record: &Record, persona: &Value, shell: bool) -> bool {
    !record.archived && !record.skill.disable_model_invocation && !record.degraded
        && runtimes_available(record, shell) && in_scope(persona, &record.skill.name)
}

/// The skills offered to the model (discovery, learned context): name and description.
/// `shell` is whether the turn lists the `shell` tool.
pub(crate) fn model_catalog_for(db: &Database, persona: &Value, shell: bool) -> ApiResult<Vec<(String, String)>> {
    Ok(owned(db)?.list().map_err(db_error)?.into_iter()
        .filter(|record| suggested(record, persona, shell))
        .map(|record| (record.skill.name, record.skill.description.chars().take(300).collect()))
        .take(64)
        .collect())
}

/// A skill read by the model through `skill_read`.
pub(crate) struct LoadedSkill {
    pub text: String,
    pub dependencies: Vec<String>,
    /// The registry revision that was read.
    pub revision: u64,
}

/// The instructions and file list of `name` for `skill_read`. A degraded skill can still
/// be read by name; its active revision is the restored passing one.
pub(crate) fn load_for_model(db: &Database, persona: &Value, name: &str, shell: bool) -> std::io::Result<LoadedSkill> {
    let registry = owned(db).map_err(|(_, error)| std::io::Error::other(error))?;
    let record = registry.get(name)?
        .filter(|record| !record.deleted && !record.archived
            && !record.skill.disable_model_invocation && runtimes_available(record, shell)
            && in_scope(persona, name))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "Skill is not available to this agent"))?;
    let dependencies = record.skill.metadata["tool_dependencies"].as_array().into_iter().flatten()
        .filter_map(Value::as_str).take(16).map(str::to_owned).collect();
    let mut text = format!("Skill {} (resources: skills/{}/):\n{}",
        record.skill.name, record.skill.name, record.skill.body);
    // Resource files are read with read_file at their skills/<name>/<path> handle.
    let files: Vec<_> = registry.resource_paths(name)?.into_iter()
        .filter(|path| path != "SKILL.md")
        .map(|path| format!("skills/{}/{path}", record.skill.name)).collect();
    if !files.is_empty() {
        text.push_str(&format!("\n\nSkill files (read with read_file): {}", files.join(", ")));
    }
    Ok(LoadedSkill { text, dependencies, revision: record.revision })
}

/// The skill block of a turn's context: pinned skills in full and a metadata catalog of
/// suggested skills. `shell` is whether the turn lists the `shell` tool.
pub(crate) fn context(db: &Database, persona: &Value, shell: bool) -> ApiResult<String> {
    let registry = owned(db)?;
    let records = registry.list().map_err(db_error)?;
    let mut text = String::new();
    let mut catalog = Vec::new();
    for record in records.into_iter().filter(|r| {
        !r.archived
            && !r.skill.disable_model_invocation
            && in_scope(persona, &r.skill.name)
    }) {
        let pinned = record.pinned || persona["pinned_skills"].as_array()
            .is_some_and(|ids| ids.iter().any(|v| v == &record.skill.name));
        if !pinned {
            if suggested(&record, persona, shell) {
                catalog.push(json!({
                    "description": record.skill.description.chars().take(300).collect::<String>(),
                    "name": record.skill.name,
                    "path": format!("skills/{}/SKILL.md", record.skill.name),
                }));
            }
            continue;
        }
        let body = if runtimes_available(&record, shell) {
            record.skill.body.clone()
        } else {
            format!("This skill is unavailable in the Rust backend until these helper runtimes are replaced: {:?}. Do not execute its helper scripts.",record.required_runtimes)
        };
        let header = format!(
            "\n\nSkill {} (resources: skills/{}/):\n",
            record.skill.name, record.skill.name
        );
        if text.len() + header.len() + body.len() > 65536 {
            continue;
        }
        text.push_str(&header);
        text.push_str(&body);
    }
    if !catalog.is_empty() {
        text.push_str("\n\nAvailable skills (metadata only). Before answering a task that matches a skill description, call skill_read with its name and follow the loaded instructions. If skill_read is unavailable, use read_file with the exact SKILL.md path below. Skip skill loading for unrelated tasks.\n");
        let mut used = 0;
        for entry in catalog {
            let row = entry.to_string();
            if used + row.len() > 4000 { break; }
            text.push_str(&row);
            text.push('\n');
            used += row.len() + 1;
        }
        text.push_str("Use discover_capabilities to find additional eligible skills.");
    }
    Ok(text)
}
/// A skill file for `read_file` at `skills/<name>/<path>`. `shell` is whether the turn
/// lists the `shell` tool, which runs helper scripts.
pub(crate) fn resource(db: &Database, persona: &Value, path: &str, shell: bool) -> std::io::Result<String> {
    let rest = path
        .strip_prefix("skills/")
        .ok_or_else(|| std::io::Error::other("Invalid skill resource handle"))?;
    let (name, relative) = rest
        .split_once('/')
        .ok_or_else(|| std::io::Error::other("Skill resource path required"))?;
    let registry = owned(db).map_err(|(_, e)| std::io::Error::other(e))?;
    let record = registry
        .get(name)?
        .ok_or_else(|| std::io::Error::other("Skill is missing"))?;
    if !in_scope(persona, name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Skill is outside this agent's configured scope",
        ));
    }
    if !runtimes_available(&record, shell) {
        return Err(std::io::Error::other(
            "Skill needs helper runtimes unavailable in this turn",
        ));
    }
    let bytes = registry.resource(name, relative)?;
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::other("Skill resource is binary; use its typed artifact adapter")
    })
}
pub(crate) fn handle(mut request: Request, db: &Database) -> std::io::Result<()> {
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    let result = (|| -> ApiResult<Value> {
        let mut registry = owned(db)?;
        if method == Method::Get {
            let skills = registry
                .list()
                .map_err(db_error)?
                .into_iter()
                .map(|r| row(&registry, r))
                .collect::<ApiResult<Vec<_>>>()?;
            return Ok(json!({"enabled":true,"skills":skills,"memories":[]}));
        }
        if method != Method::Post {
            return Err((405, "POST required".into()));
        }
        let payload = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
        if path == "/api/skills/install" {
            let source = PathBuf::from(store::text(&payload, "source", 4096)?);
            if !source.is_absolute() {
                return Err((400,"Use an absolute local package path; remote installation needs its retained source adapter".into()));
            }
            let root = if source.is_dir() {
                source.clone()
            } else {
                if source.file_name().is_none_or(|n| n != "SKILL.md") {
                    return Err((400, "Install a skill directory or its SKILL.md".into()));
                }
                source
                    .parent()
                    .ok_or((400, "Skill root required".into()))?
                    .to_owned()
            };
            let package = Package::read(
                &root,
                &kageha_contracts::execution::Control {
                    cancelled: &AtomicBool::new(false),
                    deadline: None,
                },
            )
            .map_err(|e| (400, e.to_string()))?;
            let saved = registry
                .install(&package, &source.to_string_lossy(), None)
                .map_err(|e| (409, e.to_string()))?;
            return Ok(
                json!({"installed":[saved.skill.name],"skipped":[],"required_runtimes":saved.required_runtimes,"package_sha256":saved.package_sha256}),
            );
        }
        let name = store::text(&payload, "name", 64)?;
        if !kageha_skills::valid_name(name) {
            return Err((400, "Invalid skill name".into()));
        }
        let expected = match payload.get("expected_revision") {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or((400, "expected_revision must be an integer".into()))?,
            ),
        };
        match payload["action"].as_str().unwrap_or("save") {
            "save" => {
                let content = store::text(&payload, "content", 65536)?;
                let package = Package::plain(name, content);
                if registry
                    .get(name)
                    .map_err(db_error)?
                    .is_some_and(|r| !r.deleted)
                {
                    registry
                        .edit(name, content, expected)
                        .map_err(|e| (409, e.to_string()))?;
                } else {
                    let package = package.map_err(|e| (400, e.to_string()))?;
                    if package.skill.name != name {
                        return Err((400, "Skill identity must match the requested name".into()));
                    }
                    registry
                        .install(&package, "operator", expected)
                        .map_err(|e| (409, e.to_string()))?;
                }
            }
            action @ ("pin" | "unpin" | "archive" | "restore" | "delete") => {
                registry
                    .change(name, action, expected)
                    .map_err(|e| (409, e.to_string()))?;
            }
            _ => {
                return Err((
                    409,
                    "Peer sharing requires its retained authenticated adapter".into(),
                ))
            }
        }
        let skills = registry
            .list()
            .map_err(db_error)?
            .into_iter()
            .map(|r| row(&registry, r))
            .collect::<ApiResult<Vec<_>>>()?;
        Ok(json!({"ok":true,"skills":skills}))
    })();
    match result {
        Ok(v) => server::response(request, 200, v),
        Err((code, error)) => server::response(request, code, json!({"error":error})),
    };
    Ok(())
}
