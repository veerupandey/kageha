//! Preview and install supported parts of an official plugin without executing it.
//! The approved preview is a commit-pinned snapshot. Unsupported formats are reported.
use crate::{native_io, ui_store, webui::Database};
use kageha_contracts::execution::Control;
use kageha_skills::Package;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, io, sync::{atomic::AtomicBool, Mutex}, time::{Duration,Instant}};

const PREVIEW: &str = "plugin-preview";
const INSTALLED: &str = "plugin-installed";
const MCP: &str = "config:mcp:servers";
static INSTALL_LOCK: Mutex<()> = Mutex::new(());
const MAX_PREVIEWS: u64 = 8;
const PREVIEW_TTL_SECONDS: f64 = 24.0 * 60.0 * 60.0;
fn err(text: &str) -> io::Error { io::Error::other(text) }
fn digest(value: &Value) -> String { format!("{:x}", Sha256::digest(value.to_string().as_bytes())) }
fn name_ok(name: &str) -> bool { !name.is_empty() && name.len() <= 50 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') }
fn sha_ok(s: &str) -> bool { s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) }
fn relative(path: &str) -> bool { !path.is_empty() && path.len() <= 1024 && path.split('/').count() <= 16 && path.split('/').all(|p| !p.is_empty() && p != "." && p != ".." && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'|b'.'))) }
/// Merge reviewed hook documents in declaration order, retaining group order.
fn merge_hook_documents(documents: &[Value]) -> io::Result<Value> {
    let mut events=serde_json::Map::new();
    let mut groups=0usize;
    for document in documents {
        let object=document.as_object().ok_or_else(||err("Invalid plugin hook document"))?;
        let source=if let Some(hooks)=object.get("hooks") {
            if object.len()!=1 {return Err(err("Unsupported plugin hook document option"));}
            hooks.as_object().ok_or_else(||err("Invalid plugin hook event map"))?
        } else {object};
        for (event,value) in source {
            let rows=value.as_array().ok_or_else(||err("Invalid plugin hook event groups"))?;
            groups=groups.checked_add(rows.len()).ok_or_else(||err("Too many plugin hook groups"))?;
            if groups>256 || event.len()>80 || event.chars().any(char::is_control) {return Err(err("Too many plugin hook groups"));}
            let merged=events.entry(event.clone()).or_insert_with(||json!([]));
            merged.as_array_mut().ok_or_else(||err("Invalid plugin hook event groups"))?.extend(rows.iter().cloned());
        }
    }
    if events.len()>64 {return Err(err("Too many plugin hook events"));}
    let result=json!({"hooks":events});
    if serde_json::to_vec(&result)?.len()>256*1024 {return Err(err("Plugin hook declarations exceed limits"));}
    Ok(result)
}
fn account_download(total: &mut usize, expected: u64, actual: usize) -> io::Result<()> {
    *total=total.checked_add(actual).ok_or_else(||err("Plugin resource limit exceeded"))?;
    if actual as u64 != expected || *total > 8*1024*1024 { return Err(err("Plugin source size changed or exceeds limit")); }
    Ok(())
}
fn fetch(url: &str, cancel: &AtomicBool, limit: usize) -> io::Result<Vec<u8>> {
    let response = native_io::http(url,"GET",&[("User-Agent".into(),"kageha-plugin-preview".into())],vec![],cancel,Duration::from_secs(15),limit)?;
    if response.status != 200 { return Err(err("Official plugin source unavailable")); }
    Ok(response.bytes)
}
fn get_json(url: &str, cancel: &AtomicBool, limit: usize) -> io::Result<Value> {
    serde_json::from_slice(&fetch(url,cancel,limit)?).map_err(|_|err("Invalid official plugin metadata"))
}
fn raw(commit: &str, path: &str, cancel: &AtomicBool, limit: usize) -> io::Result<Vec<u8>> {
    if !sha_ok(commit) || !relative(path) { return Err(err("Invalid plugin source path")); }
    fetch(&format!("https://raw.githubusercontent.com/openai/plugins/{commit}/{path}"),cancel,limit)
}
fn metadata(db: &Database, name: &str) -> io::Result<Option<Value>> {
    let db = db.lock().map_err(|_|err("Plugin state unavailable"))?;
    ui_store::get(&db, INSTALLED, name).map_err(io::Error::other)
}
/// Bound persistent snapshots while retaining any incomplete install for recovery.
/// Must run under the host DB lock, including the subsequent preview insertion.
fn reserve_preview(conn: &rusqlite::Connection) -> io::Result<()> {
    conn.execute("DELETE FROM ui_documents AS p WHERE p.kind=?1 AND p.updated<?2 AND NOT EXISTS (SELECT 1 FROM ui_documents AS i WHERE i.kind=?3 AND json_extract(i.body,'$.status')='installing' AND json_extract(i.body,'$.preview_id')=p.id)",
        rusqlite::params![PREVIEW,ui_store::now()-PREVIEW_TTL_SECONDS,INSTALLED]).map_err(io::Error::other)?;
    conn.execute("DELETE FROM ui_document_history WHERE kind=?1 AND id NOT IN (SELECT id FROM ui_documents WHERE kind=?1)",[PREVIEW]).map_err(io::Error::other)?;
    let count:u64=conn.query_row("SELECT count(*) FROM ui_documents WHERE kind=?",[PREVIEW],|row|row.get(0)).map_err(io::Error::other)?;
    if count >= MAX_PREVIEWS { return Err(err("Plugin preview capacity reached; finish or wait for existing previews to expire")); }
    Ok(())
}
/// Reviewable immutable summary. Private snapshots stay inside the host database.
pub(crate) fn summary(db: &Database, chat: &str, id: &str) -> io::Result<Value> {
    let db = db.lock().map_err(|_|err("Plugin state unavailable"))?;
    let row = ui_store::get(&db,PREVIEW,id).map_err(io::Error::other)?.ok_or_else(||err("Plugin preview not found"))?;
    if row["owner"] != chat || row["summary"]["preview_id"] != id { return Err(err("Plugin preview belongs to another chat")); }
    Ok(row["summary"].clone())
}
/// Official package snapshot is fetched by immutable commit, with no repository code run.
pub(crate) fn preview(db: &Database, chat: &str, name: &str, cancel: &AtomicBool) -> io::Result<Value> {
    if !name_ok(name) || chat.is_empty() || chat.len()>256 { return Err(err("Invalid plugin identity")); }
    let deadline=Instant::now()+Duration::from_secs(90);
    let (status, catalog) = crate::ui_plugin_catalog::configured(true,cancel);
    if status != "available" || !catalog.iter().any(|p| p.name == name && p.source.as_deref().is_some_and(|s| s == format!("https://github.com/openai/plugins/tree/main/plugins/{name}"))) {
        return Err(err("Plugin is not in the official catalog"));
    }
    let commit = get_json("https://api.github.com/repos/openai/plugins/commits/main",cancel,1024*1024)?["sha"].as_str().filter(|s|sha_ok(s)).ok_or_else(||err("Invalid official commit"))?.to_owned();
    let prefix = format!("plugins/{name}/");
    let manifest_path = format!("{prefix}.codex-plugin/plugin.json");
    let manifest: Value = serde_json::from_slice(&raw(&commit,&manifest_path,cancel,128*1024)?).map_err(|_|err("Invalid plugin manifest"))?;
    if manifest["name"] != name { return Err(err("Plugin manifest identity mismatch")); }
    let mut unsupported = Vec::<String>::new();
    for field in ["agents","commands"] {
        if !manifest[field].is_null() && manifest[field] != format!("./{field}") && manifest[field] != format!("./{field}/") {
            unsupported.push(format!("Nonstandard {field} declaration is not supported"));
        }
    }
    let mut apps = Vec::new();
    let mut app_manifest = Value::Null;
    if !manifest["apps"].is_null() {
        if manifest["apps"] != "./.app.json" {
            unsupported.push("Nonstandard app manifest is not supported".into());
        } else {
            app_manifest = serde_json::from_slice(&raw(&commit,&format!("{prefix}.app.json"),cancel,128*1024)?)
                .map_err(|_|err("Invalid plugin app manifest"))?;
            let (recognized, other) = crate::ui_plugin_apps::parse(name, &app_manifest)?;
            apps = recognized;
            unsupported.extend(other);
        }
    }
    // Agent and command directories use the standard plugin layout even when a
    // manifest omits explicit component fields. Inspect one immutable tree for
    // both those components and skill resources.
    let tree = get_json(&format!("https://api.github.com/repos/openai/plugins/git/trees/{commit}?recursive=1"),cancel,8*1024*1024)?;
    if tree["truncated"] != false { return Err(err("Plugin source tree is incomplete")); }
    let entries = tree["tree"].as_array().filter(|v|v.len()<=50_000).ok_or_else(||err("Invalid plugin source tree"))?;
    // Conventional hook files are inspected from the same immutable tree. No
    // commands run during preview or installation.
    let declaration=&manifest["hooks"];
    let default_declaration=declaration.is_null() || declaration.as_object().is_some_and(|v|v.is_empty());
    let items:Vec<Value>=if default_declaration {
        if entries.iter().any(|e|e["path"]==format!("{prefix}hooks/hooks.json")) {vec![json!("./hooks/hooks.json")]}
        else if entries.iter().any(|e|e["path"]==format!("{prefix}hooks.json")) {vec![json!("./hooks.json")]}
        else {vec![]}
    } else if let Some(items)=declaration.as_array() {
        if items.len()>16 {return Err(err("Too many plugin hook declarations"));}
        items.clone()
    } else {vec![declaration.clone()]};
    let mut documents=Vec::new(); let mut hook_bytes=0usize;
    for item in items {
        if let Some(path)=item.as_str() {
            let path=path.strip_prefix("./").filter(|path|relative(path) && path.ends_with(".json")).ok_or_else(||err("Unsafe plugin hook path"))?;
            let full=format!("{prefix}{path}");
            if let Some(entry)=entries.iter().find(|entry|entry["path"]==full) {
                if entry["type"]!="blob" || entry["mode"]=="120000" {return Err(err("Plugin contains unsafe hooks"));}
                let size=entry["size"].as_u64().ok_or_else(||err("Missing plugin hook size"))?;
                if size>128*1024 {return Err(err("Plugin hooks exceed limits"));}
                let bytes=raw(&commit,&full,cancel,128*1024)?;account_download(&mut hook_bytes,size,bytes.len())?;
                if hook_bytes>256*1024 {return Err(err("Plugin hook declarations exceed limits"));}
                documents.push(serde_json::from_slice(&bytes).map_err(|_|err("Invalid plugin hooks JSON"))?);
            } else {unsupported.push(format!("Declared plugin hook file {path} is missing"));}
        } else if item.is_object() {documents.push(item);}
        else {unsupported.push("Unsupported plugin hook declaration".into());}
    }
    let mut hook_manifest=if documents.is_empty(){Value::Null}else{merge_hook_documents(&documents)?};
    filter_hook_events(&mut hook_manifest,&mut unsupported);
    let mut hook_scripts=BTreeMap::<String,Vec<u8>>::new();
    if !hook_manifest.is_null() {
        let needs_resources=hook_manifest["hooks"].as_object().into_iter().flat_map(|events|events.values())
            .flat_map(|groups|groups.as_array().into_iter().flatten())
            .any(|group| group["command"].as_str().is_some_and(|c|c.contains("PLUGIN_ROOT") || c.starts_with("./scripts/")) ||
                group["hooks"].as_array().into_iter().flatten().any(|h|h["command"].as_str().is_some_and(|c|c.contains("PLUGIN_ROOT") || c.starts_with("./scripts/"))));
        let loaded=(|| -> io::Result<()> {
            if !needs_resources {return Ok(());}
            let mut total=0usize;
            for entry in entries {
                let Some(path)=entry["path"].as_str().and_then(|p|p.strip_prefix(&prefix)) else {continue};
                let in_resource_dir=["hooks/","scripts/","resources/","templates/","prompts/"].iter().any(|p|path.starts_with(p));
                let text_file=[".sh",".py",".js",".mjs",".cjs",".json",".md",".txt",".yaml",".yml",".toml"].iter().any(|ext|path.ends_with(ext));
                if !(in_resource_dir && text_file || path=="package.json") || entry["type"]=="tree" {continue;}
                if entry["type"]!="blob" || entry["mode"]=="120000" || !relative(path) {return Err(err("Unsafe plugin hook resource"));}
                if hook_scripts.len()>=64 {return Err(err("Too many plugin hook resources"));}
                let size=entry["size"].as_u64().ok_or_else(||err("Missing plugin hook resource size"))?;
                if size>65536 {return Err(err("Plugin hook resource exceeds limits"));}
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {return Err(io::Error::new(io::ErrorKind::Interrupted,"Plugin preview cancelled"));}
                if Instant::now()>=deadline {return Err(io::Error::new(io::ErrorKind::TimedOut,"Plugin preview timed out"));}
                let bytes=raw(&commit,&format!("{prefix}{path}"),cancel,65536)?;
                account_download(&mut total,size,bytes.len())?;
                if total>256*1024 {return Err(err("Plugin hook resources exceed limits"));}
                hook_scripts.insert(format!("./{path}"),bytes);
            }
            Ok(())
        })();
        if let Err(e)=loaded {unsupported.push(format!("Unsupported plugin hooks: {e}"));hook_manifest=Value::Null;hook_scripts.clear();}
    }
    let hook_config=if hook_manifest.is_null() {None} else {
        match crate::ui_plugin_components::hook_definition_with_scripts(name,&commit,&hook_manifest,&hook_scripts) {
            Ok(config) if !config["hooks"].as_array().is_some_and(Vec::is_empty) => Some(config),
            Ok(_) => None,
            Err(e) => {unsupported.push(format!("Unsupported plugin hooks: {e}")); hook_manifest=Value::Null; None}
        }
    };
    let hooks=hook_config.as_ref().map(hook_summary).unwrap_or_else(||json!([]));
    let hooks_sha256=hook_config.as_ref().map(|v|v["sha256"].clone()).unwrap_or(Value::Null);
    let mut skills = Vec::new();
    let mut packages = Vec::new();
    if !manifest["skills"].is_null() {
        if manifest["skills"] != "./skills/" && manifest["skills"] != "./skills" { unsupported.push("Nonstandard skill paths are not supported".into()); }
        else {
            let skill_prefix = format!("{prefix}skills/");
            let mut groups: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
            let mut total = 0u64;
            let mut count = 0usize;
            for entry in entries {
                let Some(path) = entry["path"].as_str().and_then(|p|p.strip_prefix(&skill_prefix)) else { continue };
                if entry["type"] == "tree" { continue; }
                if entry["type"] != "blob" || entry["mode"] == "120000" || !relative(path) { return Err(err("Plugin contains an unsafe skill resource")); }
                let (dir,sub) = path.split_once('/').ok_or_else(||err("Skill resource has no package directory"))?;
                if !relative(dir) || !relative(sub) { return Err(err("Invalid skill resource")); }
                let size=entry["size"].as_u64().ok_or_else(||err("Missing skill resource size"))?;
                total = total.saturating_add(size);
                count+=1;
                if total>8*1024*1024 || count>256 { return Err(err("Plugin skill resources exceed limits")); }
                groups.entry(dir.into()).or_default().push((sub.into(),size));
            }
            if groups.len()>32 { return Err(err("Too many plugin skills")); }
            let mut downloaded=0usize;
            for (dir, paths) in groups {
                if paths.len()>256 { return Err(err("Too many skill resources")); }
                let mut files=BTreeMap::new();
                for (path, size) in paths {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {return Err(io::Error::new(io::ErrorKind::Interrupted,"Plugin preview cancelled"));}
                    if Instant::now()>=deadline {return Err(io::Error::new(io::ErrorKind::TimedOut,"Plugin preview timed out"));}
                    let full=format!("{skill_prefix}{dir}/{path}");
                    let data=raw(&commit,&full,cancel,2*1024*1024)?;
                    account_download(&mut downloaded,size,data.len())?;
                    files.insert(path,data);
                }
                let package=Package::from_files(&dir,files)?;
                let data: BTreeMap<_,_> = package.files.iter().map(|(p,b)|(p.clone(),data_encoding::BASE64.encode(b))).collect();
                let resources: Vec<_> = package.files.iter().map(|(path,bytes)|json!({
                    "path":path,"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))
                })).collect();
                skills.push(json!({"name":package.skill.name,"description":package.skill.description,
                    "sha256":package.sha256,"resources":resources}));
                packages.push(json!({"name":package.skill.name,"sha256":package.sha256,"files":data}));
            }
        }
    }
    let mut commands = Vec::new();
    let mut agents = Vec::new();
    let mut agent_snapshots = Vec::new();
    let mut component_bytes = packages.iter().flat_map(|p|p["files"].as_object().into_iter().flat_map(|v|v.values()))
        .filter_map(Value::as_str).map(|v|v.len().saturating_mul(3)/4).sum::<usize>();
    let mut command_files = BTreeMap::new();
    let mut agent_files = BTreeMap::new();
    for (directory, output) in [("commands", &mut command_files), ("agents", &mut agent_files)] {
        let component_prefix=format!("{prefix}{directory}/");
        for item in entries {
            let Some(path)=item["path"].as_str().and_then(|p|p.strip_prefix(&component_prefix)) else {continue};
            if item["type"]=="tree" { continue; }
            if path.contains('/') || !path.ends_with(".md") { unsupported.push(format!("Non-Markdown or nested {directory} resource {path} is not supported")); continue; }
            if item["type"]!="blob" || item["mode"]=="120000" || !relative(path) { return Err(err("Plugin contains an unsafe component")); }
            if output.len()>=32 { return Err(err("Too many plugin components")); }
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {return Err(io::Error::new(io::ErrorKind::Interrupted,"Plugin preview cancelled"));}
            if Instant::now()>=deadline {return Err(io::Error::new(io::ErrorKind::TimedOut,"Plugin preview timed out"));}
            let size=item["size"].as_u64().ok_or_else(||err("Missing component size"))?;
            let bytes=raw(&commit,&format!("{component_prefix}{path}"),cancel,64*1024)?;
            account_download(&mut component_bytes,size,bytes.len())?;
            output.insert(path.to_owned(),bytes);
        }
    }
    let common: BTreeMap<String,Vec<u8>> = command_files.iter().filter(|(name,_)|name.starts_with('_'))
        .map(|(name,bytes)|(format!("references/{name}"),bytes.clone())).collect();
    for (path, bytes) in command_files.iter().filter(|(name,_)|!name.starts_with('_')) {
        let stem=path.strip_suffix(".md").unwrap();
        let package=crate::ui_plugin_components::command_package(name,stem,bytes,&common)?;
        let data: BTreeMap<_,_>=package.files.iter().map(|(p,b)|(p.clone(),data_encoding::BASE64.encode(b))).collect();
        commands.push(json!({"name":format!("/{}",package.skill.name),"skill":package.skill.name,"description":package.skill.description,"sha256":package.sha256}));
        skills.push(json!({"name":package.skill.name,"description":package.skill.description,"sha256":package.sha256,"resources":[]}));
        packages.push(json!({"name":package.skill.name,"sha256":package.sha256,"files":data}));
    }
    for (path, bytes) in agent_files.iter().filter(|(name,_)|!name.starts_with('_')) {
        let stem=path.strip_suffix(".md").unwrap();
        let mut definition=crate::ui_plugin_components::agent_definition(name,stem,bytes)?;
        definition["metadata"]["source_commit"]=json!(commit);
        let sha=digest(&definition);
        agents.push(json!({"id":definition["id"],"name":definition["name"],"description":definition["description"],"sha256":sha}));
        agent_snapshots.push(json!({"stem":stem,"content":data_encoding::BASE64.encode(bytes),"sha256":sha}));
    }
    let mut mcp = Vec::new();
    let mut configs = Vec::new();
    if let Some(path) = manifest["mcpServers"].as_str() {
        if path != "./.mcp.json" { unsupported.push("Nonstandard MCP configuration paths are not supported".into()); }
        else {
            let data: Value=serde_json::from_slice(&raw(&commit,&format!("{prefix}.mcp.json"),cancel,128*1024)?).map_err(|_|err("Invalid plugin MCP configuration"))?;
            let servers=data["mcpServers"].as_object().ok_or_else(||err("Invalid plugin MCP server map"))?;
            if servers.len()>16 { return Err(err("Too many plugin MCP servers")); }
            for (server,config) in servers {
                if !crate::server::identifier(server) { return Err(err("Invalid plugin MCP server name")); }
                let id=format!("plugin-{name}-{server}");
                if id.len()>64 { return Err(err("Plugin MCP name is too long")); }
                // Stage supported HTTP OAuth metadata without copying a client secret
                // into the preview; the pinned source is reread during installation.
                let fields=config.as_object().ok_or_else(||err("Invalid plugin MCP server"))?;
                let transport=config["type"].as_str().unwrap_or(if config["command"].is_string(){"stdio"}else{""});
                let (staged,mut details)=if transport=="http" {
                    if fields.keys().any(|k| !matches!(k.as_str(),"type"|"url"|"oauth")) {
                        unsupported.push(format!("MCP server {server} requires unsupported authentication or transport")); continue;
                    }
                    let endpoint=config["url"].as_str().ok_or_else(||err("Missing MCP endpoint"))?;
                    let url=url::Url::parse(endpoint).map_err(|_|err("Invalid MCP endpoint"))?;
                    if url.scheme()!="https" || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() { return Err(err("Unsafe MCP endpoint")); }
                    let mut staged=json!({"id":id,"name":id,"transport":"http","url":url.as_str(),"enabled":false,"connected":false,"tools":[],"origin":"plugin","plugin":name,"editable":true,"runtime_available":true,"read_only_tools":[]});
                    let mut details=json!({"name":id,"transport":"http","endpoint":url.as_str(),"enabled":false});
                    if !config["oauth"].is_null() {
                        let public=match crate::ui_mcp_oauth::public_declaration(&config["oauth"]) {
                            Ok(value)=>value,
                            Err(_)=>{unsupported.push(format!("MCP server {server} requires unsupported OAuth configuration"));continue;}
                        };
                        staged["oauth"]=public.clone();staged["oauth_source_server"]=json!(server);
                        details["oauth"]=public;details["setup_required"]=json!(true);
                    }
                    (staged,details)
                } else if transport=="stdio" {
                    if fields.keys().any(|k| !matches!(k.as_str(),"type"|"command"|"args")) {
                        unsupported.push(format!("MCP server {server} requires unsupported environment, credentials or options")); continue;
                    }
                    let command=config["command"].as_str().filter(|c|!c.is_empty() && c.len()<=128 && c.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'|b'.')) && !matches!(*c,"sh"|"bash"|"zsh"|"fish"|"dash"))
                        .ok_or_else(||err("Unsafe plugin MCP command"))?;
                    let args=match config.get("args") {
                        None | Some(Value::Null) => vec![],
                        Some(Value::Array(args)) if args.len()<=32 => args.clone(),
                        _ => return Err(err("Invalid plugin MCP arguments")),
                    };
                    if args.iter().any(|v|v.as_str().is_none_or(|arg|arg.len()>1024 || arg.chars().any(char::is_control) || arg.contains("${") || arg.contains('`'))) {return Err(err("Unsafe plugin MCP arguments"));}
                    // A disabled entry cannot spawn a process. When explicitly activated,
                    // the existing MCP sandbox applies with no inherited environment.
                    (json!({"id":id,"name":id,"transport":"stdio","command":command,"args":args,"sandbox":{"read_roots":[],"write_roots":[],"hidden_roots":[],"network":true},"enabled":false,"connected":false,"tools":[],"origin":"plugin","plugin":name,"editable":true,"runtime_available":true,"read_only_tools":[]}),
                        json!({"name":id,"transport":"stdio","command":command,"args":args,"enabled":false,"setup_required":true}))
                } else { unsupported.push(format!("MCP server {server} requires unsupported authentication or transport")); continue; };
                kageha_mcp::Config::from_legacy(&staged)?;
                details["config_sha256"]=json!(digest(&staged));
                mcp.push(details);
                configs.push(staged);
            }
        }
    } else if !manifest["mcpServers"].is_null() { unsupported.push("Inline MCP configuration is not supported".into()); }
    if packages.is_empty() && agent_snapshots.is_empty() && mcp.is_empty() && apps.is_empty() && hook_config.is_none() && unsupported.is_empty() { unsupported.push("No supported components found".into()); }
    let package_sha256=digest(&json!({"name":name,"commit":commit,"skills":skills,"commands":commands,"agents":agents,"mcp":mcp,"apps":apps,"hooks":hooks,"hooks_sha256":hooks_sha256,"unsupported":unsupported}));
    let id=crate::browser_client::unique_id();
    let result=json!({"preview_id":id,"name":name,"source":"openai/plugins","commit":commit,"package_sha256":package_sha256,"skills":skills,"commands":commands,"agents":agents,"mcp":mcp,"apps":apps,"hooks":hooks,"hooks_sha256":hooks_sha256,"hooks_enabled":false,"unsupported":unsupported,"installable":!packages.is_empty() || !configs.is_empty() || !apps.is_empty() || !agent_snapshots.is_empty() || hook_config.is_some()});
    let encoded_scripts:BTreeMap<_,_>=hook_scripts.iter().map(|(path,bytes)|(path.clone(),data_encoding::BASE64.encode(bytes))).collect();
    let row=json!({"owner":chat,"summary":result,"packages":packages,"agent_snapshots":agent_snapshots,"configs":configs,"app_manifest":app_manifest,"hook_manifest":hook_manifest,"hook_scripts":encoded_scripts,"status":"preview"});
    let mut db=db.lock().map_err(|_|err("Plugin state unavailable"))?;
    let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(io::Error::other)?;
    reserve_preview(&tx)?;
    ui_store::put(&tx,PREVIEW,&id,row).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(result)
}
/// Install the exact approved summary, with collision checks and no MCP activation.
pub(crate) fn install(db: &Database, chat: &str, id: &str, approved_summary: &Value, cancel: &AtomicBool) -> io::Result<Value> {
    let _guard=INSTALL_LOCK.lock().map_err(|_|err("Plugin installation unavailable"))?;
    let row={let conn=db.lock().map_err(|_|err("Plugin state unavailable"))?; ui_store::get(&conn,PREVIEW,id).map_err(io::Error::other)?.ok_or_else(||err("Plugin preview not found"))?};
    if row["owner"] != chat || row["summary"] != *approved_summary || row["summary"]["preview_id"] != id || !matches!(row["status"].as_str(),Some("preview"|"installing"|"installed")) { return Err(err("Plugin preview changed or is not approved")); }
    if row["summary"]["installable"] != true { return Err(err("Plugin has no supported components to install")); }
    let name=row["summary"]["name"].as_str().ok_or_else(||err("Invalid preview"))?;
    if !name_ok(name) { return Err(err("Invalid plugin identity")); }
    let apps = if row["app_manifest"].is_null() { vec![] } else { crate::ui_plugin_apps::parse(name, &row["app_manifest"])?.0 };
    if !row["summary"]["apps"].is_null() && row["summary"]["apps"] != json!(apps) { return Err(err("Plugin app snapshot changed")); }
    if row["summary"]["apps"].is_null() && !apps.is_empty() { return Err(err("Plugin app snapshot changed")); }
    let hook_scripts=row["hook_scripts"].as_object().into_iter().flat_map(|v|v.iter()).map(|(path,value)| {
        let encoded=value.as_str().ok_or_else(||err("Invalid plugin hook script"))?;
        let bytes=data_encoding::BASE64.decode(encoded.as_bytes()).map_err(|_|err("Invalid plugin hook encoding"))?;
        if !path.starts_with("./") || !relative(&path[2..]) || bytes.len()>65536 {return Err(err("Invalid plugin hook script"));}
        Ok((path.clone(),bytes))
    }).collect::<io::Result<BTreeMap<_,_>>>()?;
    if hook_scripts.len()>64 {return Err(err("Too many plugin hook resources"));}
    let hook_config=if row["hook_manifest"].is_null() {None} else {
        let commit=row["summary"]["commit"].as_str().ok_or_else(||err("Invalid plugin commit"))?;
        let config=crate::ui_plugin_components::hook_definition_with_scripts(name,commit,&row["hook_manifest"],&hook_scripts)?;
        if config["hooks"].as_array().is_some_and(Vec::is_empty) {None} else {Some(config)}
    };
    if let Some(config)=&hook_config {
        if row["summary"]["hooks"]!=hook_summary(config) || row["summary"]["hooks_sha256"]!=config["sha256"] || row["summary"]["hooks_enabled"]!=false {return Err(err("Plugin hook snapshot changed"));}
    } else if row["summary"]["hooks"].as_array().is_some_and(|h|!h.is_empty()) || !row["summary"]["hooks_sha256"].is_null() {return Err(err("Plugin hook snapshot changed"));}
    let existing = metadata(db,name)?;
    if let Some(existing) = &existing {
        if existing["preview_id"] != id || existing["owner"] != chat || existing["package_sha256"] != row["summary"]["package_sha256"] {
            return Err(err("Plugin name conflicts with an existing installation"));
        }
        if existing["status"] == "installed" { return Ok(json!({"ok":true,"plugin":existing,"replayed":true})); }
        if existing["status"] != "installing" { return Err(err("Plugin installation cannot be resumed")); }
    }
    let ctrl=Control{cancelled:cancel,deadline:None}; ctrl.check()?;
    let mut decoded=Vec::new();
    for item in row["packages"].as_array().ok_or_else(||err("Invalid preview package"))? {
        let skill_name=item["name"].as_str().ok_or_else(||err("Invalid skill name"))?;
        let files=item["files"].as_object().ok_or_else(||err("Invalid skill resources"))?.iter().map(|(path,bytes)| {
            let bytes=bytes.as_str().ok_or_else(||err("Invalid skill resource"))?;
            Ok((path.clone(),data_encoding::BASE64.decode(bytes.as_bytes()).map_err(|_|err("Invalid skill encoding"))?))
        }).collect::<io::Result<BTreeMap<_,_>>>()?;
        let package=Package::from_files(skill_name,files)?;
        if package.skill.name != skill_name || item["sha256"] != package.sha256 || !row["summary"]["skills"].as_array().is_some_and(|s|s.iter().any(|x|x["name"]==skill_name&&x["sha256"]==package.sha256)) { return Err(err("Plugin skill snapshot changed")); }
        decoded.push(package);
    }
    let mut agent_definitions=Vec::new();
    for snapshot in row["agent_snapshots"].as_array().into_iter().flatten() {
        let stem=snapshot["stem"].as_str().ok_or_else(||err("Invalid plugin agent"))?;
        let content=data_encoding::BASE64.decode(snapshot["content"].as_str().ok_or_else(||err("Invalid plugin agent"))?.as_bytes()).map_err(|_|err("Invalid plugin agent encoding"))?;
        let mut definition=crate::ui_plugin_components::agent_definition(name,stem,&content)?;
        definition["metadata"]["source_commit"]=row["summary"]["commit"].clone();
        if snapshot["sha256"] != digest(&definition) || !row["summary"]["agents"].as_array().is_some_and(|rows|rows.iter().any(|v|v["id"]==definition["id"] && v["sha256"]==snapshot["sha256"])) {return Err(err("Plugin agent snapshot changed"));}
        definition["metadata"]["preview_id"]=json!(id);
        agent_definitions.push(definition);
    }
    if row["summary"]["agents"].as_array().map_or(0,Vec::len)!=agent_definitions.len() { return Err(err("Plugin agent set changed")); }
    for command in row["summary"]["commands"].as_array().into_iter().flatten() {
        if !decoded.iter().any(|p|command["skill"]==p.skill.name && command["sha256"]==p.sha256 && command["name"]==format!("/{}",p.skill.name)) {return Err(err("Plugin command snapshot changed"));}
    }
    let configs=row["configs"].as_array().ok_or_else(||err("Invalid preview configs"))?;
    for config in configs { kageha_mcp::Config::from_legacy(config)?; if config["enabled"] != false || config["plugin"] != name || !row["summary"]["mcp"].as_array().is_some_and(|m|m.iter().any(|x|x["name"]==config["name"]&&x["transport"]==config["transport"]&&x["config_sha256"]==digest(config))) { return Err(err("Plugin MCP snapshot changed")); } }
    if row["summary"]["skills"].as_array().map(Vec::len)!=Some(decoded.len()) || row["summary"]["mcp"].as_array().map(Vec::len)!=Some(configs.len()) { return Err(err("Plugin component set changed")); }
    // OAuth client secrets are never persisted in the preview. Retrieve them
    // only from the approved immutable commit immediately before installation.
    let mut oauth_secrets: Vec<(String,zeroize::Zeroizing<String>)> = Vec::new();
    if configs.iter().any(|config| config["oauth"]["client_secret_required"] == true) {
        let commit=row["summary"]["commit"].as_str().ok_or_else(||err("Invalid plugin commit"))?;
        let bytes=zeroize::Zeroizing::new(raw(commit,&format!("plugins/{name}/.mcp.json"),cancel,128*1024)?);
        let original=crate::ui_credentials::SecretJson::new(serde_json::from_slice(&bytes).map_err(|_|err("Invalid pinned MCP configuration"))?);
        for config in configs.iter().filter(|config| config["oauth"]["client_secret_required"] == true) {
            let source_name=config["oauth_source_server"].as_str().filter(|s|crate::server::identifier(s)).ok_or_else(||err("Invalid OAuth source identity"))?;
            let server=&original["mcpServers"][source_name];
            let expected_id=format!("plugin-{name}-{source_name}");
            let source_url=url::Url::parse(server["url"].as_str().unwrap_or("")).map_err(|_|err("Invalid pinned MCP endpoint"))?;
            let public=crate::ui_mcp_oauth::public_declaration(&server["oauth"]).map_err(|_|err("Invalid pinned OAuth declaration"))?;
            if config["name"]!=expected_id || config["url"]!=source_url.as_str() || config["oauth"]!=public {return Err(err("Pinned OAuth configuration changed"));}
            let secret=server["oauth"]["client_secret"].as_str().filter(|s|!s.is_empty()).ok_or_else(||err("Pinned OAuth client secret unavailable"))?;
            oauth_secrets.push((expected_id,zeroize::Zeroizing::new(secret.to_owned())));
        }
    }
    let source=format!("plugin:{name}@{}#{id}",row["summary"]["commit"].as_str().unwrap_or(""));
    let mut registry=crate::ui_skills::owned(db).map_err(|(_,e)|err(&e))?;
    let mut unique=BTreeSet::new();
    for p in &decoded {
        if !unique.insert(p.skill.name.clone()) { return Err(err("Duplicate plugin skill")); }
        if let Some(current)=registry.get(&p.skill.name)? {
            if existing.is_none() || current.deleted || current.source!=source || current.package_sha256!=p.sha256 {
                return Err(err("A plugin skill conflicts with an existing skill"));
            }
        }
    }
    let mut agent_ids=BTreeSet::new();
    for definition in &agent_definitions {
        let agent_id=definition["id"].as_str().ok_or_else(||err("Invalid plugin agent identity"))?;
        if !agent_ids.insert(agent_id.to_owned()) {return Err(err("Duplicate plugin agent"));}
        if let Some(entry)=db.profiles.entry(kageha_agents::repository::Kind::Agent,agent_id)? {
            let mut current=entry.head().value.clone().ok_or_else(||err("A plugin agent conflicts with a deleted profile"))?;
            if let Some(object)=current.as_object_mut() {object.remove("revision");}
            if existing.is_none() || current != *definition {return Err(err("A plugin agent conflicts with an existing profile"));}
        }
    }
    { let conn=db.lock().map_err(|_|err("Plugin state unavailable"))?; for c in configs {
        let n=c["name"].as_str().ok_or_else(||err("Invalid MCP name"))?;
        if let Some(mut current)=ui_store::get(&conn,MCP,n).map_err(io::Error::other)? {
            if let Some(map)=current.as_object_mut(){map.remove("revision");}
            if existing.is_none() || current != *c { return Err(err("A plugin MCP server conflicts with an existing server")); }
        }
    } }
    // Publish the intent before the first independent registry write. If the process
    // stops, the same approved preview can safely resume matching source+hash entries.
    if existing.is_none() {
        let mut conn=db.lock().map_err(|_|err("Plugin state unavailable"))?;
        let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(io::Error::other)?;
        if ui_store::get(&tx,INSTALLED,name).map_err(io::Error::other)?.is_some() {return Err(err("Plugin name changed concurrently"));}
        let pending=json!({"name":name,"status":"installing","preview_id":id,"owner":chat,"source":row["summary"]["source"],"commit":row["summary"]["commit"],"package_sha256":row["summary"]["package_sha256"],"skills":[],"mcp":[],"apps":apps,"unsupported":row["summary"]["unsupported"]});
        ui_store::put(&tx,INSTALLED,name,pending).map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
    }
    let mut installed=Vec::new();
    for package in &decoded {
        ctrl.check().map_err(|_|err("Plugin installation is incomplete; retry the same approved preview"))?;
        let record=match registry.get(&package.skill.name)? {
            Some(current) if !current.deleted && current.source==source && current.package_sha256==package.sha256 => current,
            Some(_) => return Err(err("Plugin installation is incomplete; a skill changed concurrently")),
            None => registry.install(package,&source,Some(0)).map_err(|_|err("Plugin installation is incomplete; retry the same approved preview"))?,
        };
        installed.push(json!({"name":record.skill.name,"revision":record.revision,"sha256":record.package_sha256}));
    }
    let mut installed_agents=Vec::new();
    for definition in &agent_definitions {
        ctrl.check().map_err(|_|err("Plugin installation is incomplete; retry the same approved preview"))?;
        let agent_id=definition["id"].as_str().unwrap();
        let revision=match db.profiles.entry(kageha_agents::repository::Kind::Agent,agent_id)? {
            Some(entry) => {
                let mut current=entry.head().value.clone().ok_or_else(||err("Plugin agent was removed"))?;
                if let Some(object)=current.as_object_mut() {object.remove("revision");}
                if current != *definition {return Err(err("Plugin agent changed concurrently"));}
                entry.head().number
            }
            None => db.profiles.write(kageha_agents::repository::Kind::Agent,agent_id,0,Some(definition.clone()),ui_store::now())?.head().number,
        };
        installed_agents.push(json!({"id":agent_id,"name":definition["name"],"description":definition["description"],"revision":revision}));
    }
    ctrl.check().map_err(|_|err("Plugin installation is incomplete; retry the same approved preview"))?;
    let conn=db.lock().map_err(|_|err("Plugin state unavailable"))?;
    let tx=conn.unchecked_transaction().map_err(io::Error::other)?;
    for (name,secret) in &oauth_secrets {
        let credential=format!("mcp:{name}:oauth-client");
        if crate::ui_credentials::managed(&tx,&credential).map_err(|(_,e)|err(&e))? {
            let stored=crate::ui_settings::secret(&tx,&credential).map_err(|(_,e)|err(&e))?;
            if stored != secret.as_str() {return Err(err("MCP OAuth client credential conflicts with existing configuration"));}
        } else {
            crate::ui_settings::save_secret(&tx,&credential,secret.as_str()).map_err(|(_,e)|err(&e))?;
        }
    }
    for config in configs {
        let n=config["name"].as_str().unwrap();
        if let Some(mut current)=ui_store::get(&tx,MCP,n).map_err(io::Error::other)? {
            if let Some(map)=current.as_object_mut(){map.remove("revision");}
            if current != *config {return Err(err("Plugin installation is incomplete; MCP server changed concurrently"));}
        } else {ui_store::put(&tx,MCP,n,config.clone()).map_err(io::Error::other)?;}
    }
    if let Some(config)=&hook_config {
        if let Some(current)=ui_store::get(&tx,"plugin-hooks",name).map_err(io::Error::other)? {
            if current != *config {return Err(err("Plugin hook configuration conflicts with existing configuration"));}
        } else {ui_store::put(&tx,"plugin-hooks",name,config.clone()).map_err(io::Error::other)?;}
    }
    let result=json!({"name":name,"status":"installed","preview_id":id,"owner":chat,"source":row["summary"]["source"],"commit":row["summary"]["commit"],"package_sha256":row["summary"]["package_sha256"],"skills":installed,"commands":row["summary"]["commands"],"agents":installed_agents,"mcp":row["summary"]["mcp"],"apps":apps,"hooks":row["summary"]["hooks"],"hooks_sha256":row["summary"]["hooks_sha256"],"hooks_enabled":false,"unsupported":row["summary"]["unsupported"]});
    ui_store::put(&tx,INSTALLED,name,result.clone()).map_err(io::Error::other)?;
    let mut done=row; done["status"]=json!("installed"); ui_store::put(&tx,PREVIEW,id,done).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(json!({"ok":true,"plugin":result}))
}
pub(crate) fn list(db: &Database) -> io::Result<Value> {
    let conn=db.lock().map_err(|_|err("Plugin state unavailable"))?;
    let mut plugins=ui_store::list(&conn,INSTALLED).map_err(io::Error::other)?;
    for plugin in &mut plugins { current_mcp(&conn,plugin)?; current_hooks(&conn,plugin)?; current_apps(&conn,plugin)?; }
    Ok(json!({"plugins":plugins}))
}
fn current_apps(conn: &rusqlite::Connection, plugin: &mut Value) -> io::Result<()> {
    let snapshot=plugin.clone();
    for app in plugin["apps"].as_array_mut().into_iter().flatten() {
        if app["integration"].is_null() {
            app["bridge"]=crate::ui_plugin_apps::bridge_status(conn,&snapshot,app)?;
        }
    }
    Ok(())
}
/// Keep supported event groups and report unsupported ones individually.
fn filter_hook_events(manifest:&mut Value, unsupported:&mut Vec<String>) {
    if let Some(events)=manifest.get_mut("hooks").and_then(Value::as_object_mut) {
        let unsupported_events:Vec<String>=events.keys().filter(|name| {
            !kageha_workspace::hooks::normalize(name).is_some_and(|event|matches!(event,"sessionStart"|"preToolUse"|"postToolUse"|"beforeShell"|"stop"|"sessionEnd"))
        }).cloned().collect();
        for event in unsupported_events {
            events.remove(&event);
            unsupported.push(format!("Plugin hook event {event} is not dispatched by this runtime"));
        }
        for (event,groups) in events.iter_mut() {
            let limit=if kageha_workspace::hooks::normalize(event)==Some("stop") {900.0} else {120.0};
            if let Some(groups)=groups.as_array_mut() {
                groups.retain_mut(|group| {
                    if let Some(handlers)=group.get_mut("hooks").and_then(Value::as_array_mut) {
                        handlers.retain(|handler| {
                            let timeout=handler.get("timeout").or_else(||handler.get("timeout_sec")).and_then(Value::as_f64);
                            if timeout.is_some_and(|timeout|!timeout.is_finite() || timeout<=0.0 || timeout>limit) {
                                unsupported.push(format!("Plugin hook {event} timeout exceeds supported limits"));false
                            } else {true}
                        });
                        !handlers.is_empty()
                    } else {
                        let timeout=group.get("timeout").or_else(||group.get("timeout_sec")).and_then(Value::as_f64);
                        if timeout.is_some_and(|timeout|!timeout.is_finite() || timeout<=0.0 || timeout>limit) {
                            unsupported.push(format!("Plugin hook {event} timeout exceeds supported limits"));false
                        } else {true}
                    }
                });
            }
        }
    }
}
fn hook_summary(config:&Value) -> Value {
    json!(config["hooks"].as_array().into_iter().flatten().map(|h|json!({
        "event":h["event"],"matcher":h["matcher"],"matcher_type":if h["plugin_matcher_regex"].is_string(){"regex"}else{"glob"},"command":h["command"],"http":h["http"],"timeout_s":h["timeout_s"],"status_message":h["status_message"],"resources":h["plugin_files"]
    })).collect::<Vec<_>>())
}
fn current_hooks(conn:&rusqlite::Connection, plugin:&mut Value) -> io::Result<()> {
    let name=plugin["name"].as_str().unwrap_or("").to_owned();
    if let Some(config)=ui_store::get(conn,"plugin-hooks",&name).map_err(io::Error::other)? {
        let hooks:Vec<kageha_workspace::hooks::Hook>=serde_json::from_value(config["hooks"].clone()).map_err(|_|err("Invalid installed plugin hooks"))?;
        let sha=format!("{:x}",Sha256::digest(serde_json::to_vec(&hooks)?));
        plugin["hooks_enabled"]=json!(config["enabled"]==true && config["sha256"]==sha && config["sha256"]==plugin["hooks_sha256"] && config["commit"]==plugin["commit"]);
    }
    Ok(())
}
/// Attach saved configuration state without claiming a live connection check.
fn current_mcp(conn: &rusqlite::Connection, plugin: &mut Value) -> io::Result<()> {
    let owner=plugin["name"].as_str().unwrap_or("").to_owned();
    for item in plugin["mcp"].as_array_mut().into_iter().flatten() {
        let name=item["name"].as_str().ok_or_else(||err("Invalid installed MCP name"))?.to_owned();
        match ui_store::get(conn,MCP,&name).map_err(io::Error::other)? {
            None => { item["configuration_status"]=json!("removed"); item["enabled"]=Value::Null; item["recorded_connected"]=Value::Null; }
            Some(config) if config["plugin"] != owner => { item["configuration_status"]=json!("replaced"); item["enabled"]=Value::Null; item["recorded_connected"]=Value::Null; }
            Some(config) => { item["configuration_status"]=json!("configured"); item["enabled"]=json!(config["enabled"]==true); item["recorded_connected"]=json!(config["connected"]==true); }
        }
    }
    Ok(())
}
/// Read a preview's installation state without exposing staged package bytes.
pub(crate) fn status(db: &Database, chat: &str, id: &str) -> io::Result<Value> {
    let summary=summary(db,chat,id)?;
    let name=summary["name"].as_str().ok_or_else(||err("Invalid preview"))?;
    let record=metadata(db,name)?;
    if let Some(mut record)=record.filter(|v|v["preview_id"]==id && v["owner"]==chat) {
        {let conn=db.lock().map_err(|_|err("Plugin state unavailable"))?; current_mcp(&conn,&mut record)?; current_hooks(&conn,&mut record)?; current_apps(&conn,&mut record)?;}
        if record["status"]=="installing" {
            let registry=crate::ui_skills::owned(db).map_err(|(_,e)|err(&e))?;
            let source=format!("plugin:{name}@{}#{id}",summary["commit"].as_str().unwrap_or(""));
            let mut recovered=Vec::new();
            for skill in summary["skills"].as_array().into_iter().flatten() {
                let Some(skill_name)=skill["name"].as_str() else {continue};
                if let Some(saved)=registry.get(skill_name)?.filter(|v|!v.deleted && v.source==source && skill["sha256"]==v.package_sha256) {
                    recovered.push(json!({"name":saved.skill.name,"revision":saved.revision,"sha256":saved.package_sha256}));
                }
            }
            record["skills"]=json!(recovered);
        }
        Ok(json!({"preview":summary,"installation":record}))
    } else { Ok(json!({"preview":summary,"installation":null})) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_oauth_client_stages_without_a_client_secret_or_network_refetch() {
        let fixture=crate::audited_session_tests::Fixture::new();let db=&fixture.db;
        let oauth=crate::ui_mcp_oauth::public_declaration(&json!({"client_id":"fixture-public",
            "callback_url":"http://127.0.0.1:12799/callback"})).unwrap();
        let config=json!({"id":"plugin-fixture-public","name":"plugin-fixture-public","transport":"http",
            "url":"https://api.githubcopilot.com/mcp/","enabled":false,"connected":false,"tools":[],
            "origin":"plugin","plugin":"fixture","editable":true,"runtime_available":true,"read_only_tools":[],
            "oauth":oauth,"oauth_source_server":"public"});
        let summary=json!({"preview_id":"public-client-preview","name":"fixture","source":"openai/plugins",
            "commit":"0123456789012345678901234567890123456789","package_sha256":"digest","skills":[],"commands":[],"agents":[],
            "mcp":[{"name":"plugin-fixture-public","transport":"http","endpoint":config["url"],"enabled":false,
                "oauth":oauth,"config_sha256":digest(&config)}],"apps":[],"unsupported":[],"installable":true});
        let staged=json!({"owner":"chat","summary":summary,"packages":[],"agent_snapshots":[],"configs":[config],"app_manifest":null,"status":"preview"});
        ui_store::put(&db.lock().unwrap(),PREVIEW,"public-client-preview",staged).unwrap();
        let result=install(db,"chat","public-client-preview",&summary,&AtomicBool::new(false)).unwrap();
        assert_eq!(result["plugin"]["status"],"installed");
        let connection=db.lock().unwrap();
        let saved=ui_store::get(&connection,MCP,"plugin-fixture-public").unwrap().unwrap();
        assert_eq!(saved["enabled"],false);assert_eq!(saved["oauth"]["client_secret_required"],false);
        assert!(!crate::ui_credentials::managed(&connection,"mcp:plugin-fixture-public:oauth-client").unwrap());
    }
    #[test]
    fn plugin_command_and_specialist_are_installed_and_invoked() {
        let fixture=crate::audited_session_tests::Fixture::new(); let db=&fixture.db;
        let command=crate::ui_plugin_components::command_package("fixture","review",b"Review $ARGUMENTS against the source.",&BTreeMap::new()).unwrap();
        let agent_bytes=b"---\ndescription: Review evidence\n---\nReview carefully.";
        let commit="0123456789012345678901234567890123456789";
        let mut agent=crate::ui_plugin_components::agent_definition("fixture","reviewer",agent_bytes).unwrap();
        agent["metadata"]["source_commit"]=json!(commit);
        let summary=json!({"preview_id":"component-test","name":"fixture","source":"openai/plugins","commit":commit,"package_sha256":"fixture-digest",
            "skills":[{"name":command.skill.name,"sha256":command.sha256}],
            "commands":[{"name":"/plugin-fixture-review","skill":command.skill.name,"description":command.skill.description,"sha256":command.sha256}],
            "agents":[{"id":agent["id"],"name":agent["name"],"description":agent["description"],"sha256":digest(&agent)}],
            "mcp":[],"apps":[],"unsupported":[],"installable":true});
        let files:BTreeMap<_,_>=command.files.iter().map(|(path,body)|(path.clone(),data_encoding::BASE64.encode(body))).collect();
        let row=json!({"owner":"chat","summary":summary,"packages":[{"name":command.skill.name,"sha256":command.sha256,"files":files}],
            "agent_snapshots":[{"stem":"reviewer","content":data_encoding::BASE64.encode(agent_bytes),"sha256":digest(&agent)}],
            "configs":[],"app_manifest":null,"status":"preview"});
        ui_store::put(&db.lock().unwrap(),PREVIEW,"component-test",row).unwrap();
        let installed=install(db,"chat","component-test",&summary,&AtomicBool::new(false)).unwrap();
        assert_eq!(installed["plugin"]["agents"][0]["id"],"plugin.fixture.reviewer");
        let profile=crate::ui_agents::agent(db,"plugin.fixture.reviewer").unwrap();
        assert_eq!(profile["system_prompt"],"Review carefully.");
        assert_eq!(profile["tool_policy"],json!({"allow":[],"deny":[]}));
        use crate::native_chat::ToolHost;
        let host=fixture.tools(json!({"id":"main","name":"Main","tool_policy":{"allow":[],"deny":[]}}));
        let started=host.invoke(&json!({"id":"spawn-plugin-agent","name":"agent_spawn","arguments":{
            "agent_id":"plugin.fixture.reviewer","objective":"Inspect the fixture","title":"Review fixture"}})).unwrap();
        let started:Value=serde_json::from_str(&started).unwrap();
        assert_eq!(started["spec"]["persona_id"],"plugin.fixture.reviewer");
        let commands=crate::ui_plugin_components::slash_commands(db).unwrap();
        assert!(commands.iter().any(|c|c["label"]=="/plugin-fixture-review"));
        let context=crate::ui_plugin_components::command_context(db,&json!({"id":"main"}),&[json!({"role":"user","content":"/plugin-fixture-review page"})],true,&host).unwrap().unwrap();
        assert_eq!(context[0]["tool_calls"][0]["name"],"skill_read");
        assert_eq!(context[1]["role"],"tool");
        assert!(context[1]["content"].as_str().unwrap().contains("Review $ARGUMENTS against the source."));
        assert!(crate::ui_plugin_components::command_context(db,&json!({"id":"other","skills_restricted_to_pinned":true}),&[json!({"role":"user","content":"/plugin-fixture-review page"})],true,&host).is_err());
    }
    #[test]
    fn multiple_hook_documents_preserve_order_and_reject_bad_event_shapes() {
        let documents=vec![json!({"hooks":{"SessionStart":[{"hooks":[{"command":"first"}]}]}}),
            json!({"SessionStart":[{"hooks":[{"command":"second"}]}],"Stop":[{"hooks":[{"command":"stop"}]}]})];
        let merged=merge_hook_documents(&documents).unwrap();
        assert_eq!(merged["hooks"]["SessionStart"][0]["hooks"][0]["command"],"first");
        assert_eq!(merged["hooks"]["SessionStart"][1]["hooks"][0]["command"],"second");
        assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(),1);
        assert!(merge_hook_documents(&[json!({"hooks":{"Stop":"not-a-list"}})]).is_err());
        assert!(merge_hook_documents(&[json!({"hooks":{},"extra":true})]).is_err());
        assert!(!relative("hooks/../../outside.json"));
    }
    #[test]
    fn mixed_hook_events_keep_supported_groups_and_report_the_rest() {
        let mut manifest=json!({"hooks":{
            "SessionStart":[{"hooks":[{"type":"command","command":"echo ready"}]}],
            "BeforeShellExecution":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo shell-hook"}]}],
            "Stop":[{"hooks":[{"type":"command","command":"echo stop","timeout":901}]}],
            "SessionEnd":[{"hooks":[{"type":"command","command":"echo end"}]}]
        }});
        let mut unsupported=Vec::new();filter_hook_events(&mut manifest,&mut unsupported);
        assert_eq!(unsupported.len(),1);
        assert!(unsupported.iter().any(|s|s.contains("Stop")));
        let config=crate::ui_plugin_components::hook_definition("fixture","0123456789012345678901234567890123456789",&manifest).unwrap();
        let hooks:Vec<kageha_workspace::hooks::Hook>=serde_json::from_value(config["hooks"].clone()).unwrap();
        assert!(hooks.iter().any(|h|h.matches("sessionStart","session")));
        assert!(hooks.iter().any(|h|h.matches("beforeShell","shell")));
        assert!(hooks.iter().any(|h|h.matches("sessionEnd","session")));
        let mut long_stop=json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo stop","timeout":900}]}]}});
        let mut errors=Vec::new();filter_hook_events(&mut long_stop,&mut errors);
        assert!(errors.is_empty());
        assert!(crate::ui_plugin_components::hook_definition("fixture","0123456789012345678901234567890123456789",&long_stop).is_ok());
    }
    #[test]
    fn before_shell_plugin_hook_runs_through_tool_host() {
        if !crate::ui_shell::available() {return;}
        use kageha_runtime::lifecycle::RunStore;
        use crate::native_chat::ToolHost;
        let mut fixture=crate::audited_session_tests::Fixture::new();
        let commit="0123456789012345678901234567890123456789";
        let config=crate::ui_plugin_components::hook_definition("fixture",commit,&json!({"hooks":{"BeforeShellExecution":[{"matcher":"Bash","hooks":[{"type":"command","command":"printf shell-hook-fired"}]}]}})).unwrap();
        {
            let db=fixture.db.lock().unwrap();
            ui_store::put(&db,"plugin-installed","fixture",json!({"name":"fixture","status":"installed","commit":commit,"hooks_sha256":config["sha256"]})).unwrap();
            ui_store::put(&db,"plugin-hooks","fixture",config).unwrap();
        }
        crate::ui_plugin_components::set_hooks_enabled(&fixture.db,"fixture",true).unwrap();
        let project=fixture.tools(json!({"id":"main"})).workspace.to_path_buf();
        let hooks=crate::ui_hooks::capture(&fixture.db,&project).unwrap();
        let request="before-shell-hook";
        fixture.turn=crate::webui::turn_id(crate::audited_session_tests::CHAT,request);
        let mut configuration=json!({"project_hooks":hooks});
        let spec={let conn=fixture.db.lock().unwrap();
            crate::ui_persistence::admit(&conn,&fixture.turn,&mut configuration).unwrap();
            let spec=crate::runtime_host::spec(&project,crate::audited_session_tests::CHAT,request,&fixture.turn,"main","Run shell",0,None,None,
                vec![json!({"role":"user","content":"Run shell"})],configuration,64);
            crate::runtime_host::prepare(&conn,&spec).unwrap();spec};
        crate::runtime_host::store(&fixture.db).unwrap().stage(&spec,64).unwrap();
        let host=fixture.tools(json!({"id":"main","name":"Main","tool_policy":{"allow":[],"deny":[]}}));
        let result=host.invoke(&json!({"id":"shell-call","name":"shell","arguments":{"command":"printf shell-result"}})).unwrap();
        let result:Value=serde_json::from_str(&result).unwrap();
        assert!(result["hook_context"].as_str().unwrap().contains("shell-hook-fired"));
        assert!(result["tool_result"].as_str().unwrap().contains("shell-result"));
    }
    #[test]
    fn plugin_node_hook_uses_pinned_package_root_and_cleans_it_up() {
        if std::process::Command::new("node").arg("--version").output().is_err() {return;}
        let fixture=crate::audited_session_tests::Fixture::new();let db=&fixture.db;
        let commit="0123456789012345678901234567890123456789";
        let manifest=json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"node \"${CLAUDE_PLUGIN_ROOT}/scripts/main.mjs\" start","timeout":5}]}]}});
        let main=b"import {value} from './lib.mjs';\nimport fs from 'node:fs';\nconst input=JSON.parse(fs.readFileSync(0,'utf8'));\nfs.writeFileSync('node-result.json',JSON.stringify({value,input,root:process.env.CLAUDE_PLUGIN_ROOT,arg:process.argv[2]}));\n";
        let library=b"export const value = 'pinned-resource';\n";
        let scripts=BTreeMap::from([("./scripts/main.mjs".into(),main.to_vec()),("./scripts/lib.mjs".into(),library.to_vec())]);
        let config=crate::ui_plugin_components::hook_definition_with_scripts("nodefixture",commit,&manifest,&scripts).unwrap();
        assert_eq!(hook_summary(&config)[0]["resources"]["scripts/lib.mjs"],std::str::from_utf8(library).unwrap());
        let summary=json!({"preview_id":"node-hooks","name":"nodefixture","source":"openai/plugins","commit":commit,"package_sha256":"digest",
            "skills":[],"commands":[],"agents":[],"mcp":[],"apps":[],"hooks":hook_summary(&config),"hooks_sha256":config["sha256"],"hooks_enabled":false,"unsupported":[],"installable":true});
        let files:BTreeMap<_,_>=scripts.iter().map(|(path,bytes)|(path.clone(),data_encoding::BASE64.encode(bytes))).collect();
        let row=json!({"owner":"chat","summary":summary,"packages":[],"agent_snapshots":[],"configs":[],"app_manifest":null,"hook_manifest":manifest,"hook_scripts":files,"status":"preview"});
        ui_store::put(&db.lock().unwrap(),PREVIEW,"node-hooks",row).unwrap();
        install(db,"chat","node-hooks",&summary,&AtomicBool::new(false)).unwrap();
        crate::ui_plugin_components::set_hooks_enabled(db,"nodefixture",true).unwrap();
        let set=kageha_workspace::hooks::Set{hooks:crate::ui_plugin_components::active_hooks(db).unwrap()};
        let workspace=tempfile::tempdir().unwrap();
        let result=crate::ui_hooks::run(db,crate::audited_session_tests::CHAT,&fixture.turn,workspace.path(),&set,"sessionStart",
            &json!({"id":"node-start","name":"session","arguments":{}}),&AtomicBool::new(false)).unwrap();
        assert_eq!(result.ran,1);assert!(result.warning.is_none(),"{:?}",result.warning);
        let result:Value=serde_json::from_slice(&std::fs::read(workspace.path().join("node-result.json")).unwrap()).unwrap();
        assert_eq!(result["value"],"pinned-resource");assert_eq!(result["arg"],"start");
        assert_eq!(result["input"]["event"],"sessionStart");
        assert_eq!(result["input"]["hook_event_name"],"SessionStart");
        assert!(!std::path::Path::new(result["root"].as_str().unwrap()).exists(),"temporary package must be removed");
        let mut altered=ui_store::get(&db.lock().unwrap(),"plugin-hooks","nodefixture").unwrap().unwrap();
        altered["hooks"][0]["plugin_files"]["scripts/lib.mjs"]=json!("export const value='altered';");
        ui_store::put(&db.lock().unwrap(),"plugin-hooks","nodefixture",altered).unwrap();
        assert!(crate::ui_plugin_components::active_hooks(db).is_err());
        assert!(crate::ui_plugin_components::set_hooks_enabled(db,"nodefixture",true).is_err());
        let unsafe_resources=BTreeMap::from([("./scripts/../../escape.mjs".into(),main.to_vec())]);
        assert!(crate::ui_plugin_components::hook_definition_with_scripts("nodefixture",commit,&manifest,&unsafe_resources).is_err());
        assert!(crate::ui_plugin_components::hook_definition("nodefixture",commit,&json!({"hooks":{"UnsupportedEvent":[{"hooks":[{"type":"command","command":"echo unsupported"}]}]}})).is_err());
    }
    #[test]
    fn figma_style_script_hooks_use_pinned_contents_and_stdin() {
        let fixture=crate::audited_session_tests::Fixture::new(); let db=&fixture.db;
        let commit="0123456789012345678901234567890123456789";
        let manifest=json!({"hooks":{"PostToolUse":[{"matcher":"Write|Edit","hooks":[{"type":"command","command":"./scripts/parity.sh"}]}]}});
        let script=b"#!/usr/bin/env bash\ncat > hook-payload\necho parity\n";
        let scripts=BTreeMap::from([("./scripts/parity.sh".to_string(),script.to_vec())]);
        let config=crate::ui_plugin_components::hook_definition_with_scripts("figma",commit,&manifest,&scripts).unwrap();
        let summary=json!({"preview_id":"figma-hooks","name":"figma","source":"openai/plugins","commit":commit,"package_sha256":"digest",
            "skills":[],"commands":[],"agents":[],"mcp":[],"apps":[],"hooks":hook_summary(&config),"hooks_sha256":config["sha256"],"hooks_enabled":false,"unsupported":[],"installable":true});
        assert!(summary["hooks"][0]["resources"]["scripts/parity.sh"].as_str().unwrap().contains("cat > hook-payload"));
        let mut row=json!({"owner":"chat","summary":summary,"packages":[],"agent_snapshots":[],"configs":[],"app_manifest":null,"hook_manifest":manifest,"hook_scripts":{"./scripts/parity.sh":data_encoding::BASE64.encode(script)},"status":"preview"});
        row["hook_scripts"]["./scripts/parity.sh"]=json!(data_encoding::BASE64.encode(b"#!/bin/sh\necho altered"));
        ui_store::put(&db.lock().unwrap(),PREVIEW,"figma-hooks",row.clone()).unwrap();
        assert!(install(db,"chat","figma-hooks",&summary,&AtomicBool::new(false)).is_err());
        row["hook_scripts"]["./scripts/parity.sh"]=json!(data_encoding::BASE64.encode(script));
        ui_store::put(&db.lock().unwrap(),PREVIEW,"figma-hooks",row).unwrap();
        install(db,"chat","figma-hooks",&summary,&AtomicBool::new(false)).unwrap();
        crate::ui_plugin_components::set_hooks_enabled(db,"figma",true).unwrap();
        let set=kageha_workspace::hooks::Set{hooks:crate::ui_plugin_components::active_hooks(db).unwrap()};
        assert_eq!(set.hooks.len(),2);
        assert!(set.hooks.iter().any(|h|h.matches("postToolUse","write_file")));
        assert!(set.hooks.iter().any(|h|h.matches("postToolUse","edit_file")));
        assert!(!set.hooks.iter().any(|h|h.matches("postToolUse","read_file")));
        let workspace=tempfile::tempdir().unwrap();
        let result=crate::ui_hooks::run(db,crate::audited_session_tests::CHAT,&fixture.turn,workspace.path(),&set,"postToolUse",
            &json!({"id":"write-1","name":"write_file","arguments":{"path":"screen.html"}}),&AtomicBool::new(false)).unwrap();
        assert_eq!(result.ran,1); assert!(result.extra_context.contains("parity"));
        let payload:Value=serde_json::from_slice(&std::fs::read(workspace.path().join("hook-payload")).unwrap()).unwrap();
        assert_eq!(payload["tool_name"],"write_file");
    }
    #[test]
    fn plugin_hooks_install_disabled_and_can_be_revoked() {
        let fixture=crate::audited_session_tests::Fixture::new(); let db=&fixture.db;
        let commit="0123456789012345678901234567890123456789";
        let manifest=json!({"hooks":{"PreToolUse":[{"matcher":"browser_*","hooks":[{"type":"command","command":"printf allowed > hook-result"}]}]}});
        let config=crate::ui_plugin_components::hook_definition("fixture",commit,&manifest).unwrap();
        let summary=json!({"preview_id":"hook-preview","name":"fixture","source":"openai/plugins","commit":commit,"package_sha256":"digest",
            "skills":[],"commands":[],"agents":[],"mcp":[],"apps":[],"hooks":hook_summary(&config),"hooks_sha256":config["sha256"],"hooks_enabled":false,"unsupported":[],"installable":true});
        let row=json!({"owner":"chat","summary":summary,"packages":[],"agent_snapshots":[],"configs":[],"app_manifest":null,"hook_manifest":manifest,"status":"preview"});
        ui_store::put(&db.lock().unwrap(),PREVIEW,"hook-preview",row).unwrap();
        let installed=install(db,"chat","hook-preview",&summary,&AtomicBool::new(false)).unwrap();
        assert_eq!(installed["plugin"]["hooks_enabled"],false);
        assert!(crate::ui_plugin_components::active_hooks(db).unwrap().is_empty());
        crate::ui_plugin_components::set_hooks_enabled(db,"fixture",true).unwrap();
        assert_eq!(list(db).unwrap()["plugins"][0]["hooks_enabled"],true);
        let hooks=crate::ui_plugin_components::active_hooks(db).unwrap();
        assert_eq!(hooks.len(),1); assert!(hooks[0].matches("preToolUse","browser_click"));
        let workspace=tempfile::tempdir().unwrap();
        let set=kageha_workspace::hooks::Set{hooks:hooks.clone()};
        let outcome=crate::ui_hooks::run(db,crate::audited_session_tests::CHAT,&fixture.turn,workspace.path(),&set,"preToolUse",
            &json!({"id":"call-1","name":"browser_click","arguments":{}}),&AtomicBool::new(false)).unwrap();
        assert_eq!(outcome.ran,1);
        assert_eq!(std::fs::read_to_string(workspace.path().join("hook-result")).unwrap(),"allowed");
        // Revocation while a manual approval is pending must stop dispatch even
        // when the operator later approves the earlier request.
        let journal=crate::ui_approvals::owned(&db.lock().unwrap()).unwrap();
        journal.set_profile(crate::audited_session_tests::CHAT,kageha_contracts::policy::PermissionProfile::Ask).unwrap();
        let cancelled=AtomicBool::new(false);
        std::thread::scope(|scope| {
            let runner=scope.spawn(||crate::ui_hooks::run(db,crate::audited_session_tests::CHAT,&fixture.turn,workspace.path(),&set,"preToolUse",
                &json!({"id":"call-pending","name":"browser_click","arguments":{}}),&cancelled));
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(3);
            let pending=loop {
                if let Some(record)=journal.pending_family(crate::audited_session_tests::CHAT).unwrap().into_iter().next() {break Some(record)}
                if std::time::Instant::now()>=deadline {cancelled.store(true,std::sync::atomic::Ordering::Relaxed);break None}
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            crate::ui_plugin_components::set_hooks_enabled(db,"fixture",false).unwrap();
            let observed=pending.is_some();
            if let Some(record)=pending {
                journal.decide(&record.id,&kageha_contracts::policy::ApprovalDecision{expected_revision:record.revision,authority:record.request.authority,approved:true,scope:kageha_contracts::policy::GrantScope::Once}).unwrap();
            }
            assert!(runner.join().unwrap().is_err());
            assert!(observed,"hook must wait for operator approval");
        });
        assert!(crate::ui_plugin_components::verify_active_hook(db,&hooks[0]).is_err());
        assert!(crate::ui_hooks::run(db,crate::audited_session_tests::CHAT,&fixture.turn,workspace.path(),&set,"preToolUse",
            &json!({"id":"call-2","name":"browser_click","arguments":{}}),&AtomicBool::new(false)).is_err());
    }
    #[test]
    fn app_binding_does_not_create_or_widen_an_integration_grant() {
        let fixture=crate::audited_session_tests::Fixture::new();
        let manifest=json!({"apps":{"gmail":{"id":"connector_2128aebfecb84f64a069897515042a44"}}});
        let apps=crate::ui_plugin_apps::parse("gmail", &manifest).unwrap().0;
        let summary=json!({"preview_id":"app-preview","name":"gmail","source":"openai/plugins",
            "commit":"0123456789012345678901234567890123456789","package_sha256":"digest",
            "skills":[],"mcp":[],"apps":apps,"unsupported":[],"installable":true});
        let row=json!({"owner":"chat","summary":summary,"packages":[],"configs":[],"app_manifest":manifest,"status":"preview"});
        ui_store::put(&fixture.db.lock().unwrap(),PREVIEW,"app-preview",row).unwrap();
        let result=install(&fixture.db,"chat","app-preview",&summary,&AtomicBool::new(false)).unwrap();
        assert_eq!(result["plugin"]["apps"][0]["integration"],"gmail");
        assert!(crate::native_auth::readable(&fixture.db,"main").is_empty());
        assert!(ui_store::get(&fixture.db.lock().unwrap(),"integration","gmail").unwrap().is_none());
    }
    #[test]
    fn downloaded_bytes_are_checked_against_advertised_size_and_total_limit() {
        let mut total=8*1024*1024-2;
        assert!(account_download(&mut total,2,2).is_ok());
        assert!(account_download(&mut total,1,1).is_err());
        assert!(account_download(&mut 0,1,2).is_err());
        let mut full=usize::MAX; assert!(account_download(&mut full,1,1).is_err());
    }
    #[test]
    fn preview_storage_is_bounded_and_keeps_interrupted_installs() {
        let fixture=crate::audited_session_tests::Fixture::new();let conn=fixture.db.lock().unwrap();
        for i in 0..MAX_PREVIEWS {
            ui_store::put(&conn,PREVIEW,&format!("p{i}"),json!({"owner":"chat"})).unwrap();
        }
        assert!(reserve_preview(&conn).is_err());
        conn.execute("UPDATE ui_documents SET updated=0 WHERE kind=?",[PREVIEW]).unwrap();
        ui_store::put(&conn,INSTALLED,"fixture",json!({"status":"installing","preview_id":"p0"})).unwrap();
        assert!(reserve_preview(&conn).is_ok());
        assert!(ui_store::get(&conn,PREVIEW,"p0").unwrap().is_some());
        assert!(ui_store::get(&conn,PREVIEW,"p1").unwrap().is_none());
        let history:u64=conn.query_row("SELECT count(*) FROM ui_document_history WHERE kind=? AND id='p1'",[PREVIEW],|row|row.get(0)).unwrap();
        assert_eq!(history,0);
    }
    fn fixture() -> (crate::audited_session_tests::Fixture, Value) {
        let fixture=crate::audited_session_tests::Fixture::new();
        let files=BTreeMap::from([("SKILL.md".to_owned(),b"---\nname: fixture-plugin-skill\ndescription: Explain fixture plugin tests\n---\nDo the relevant task.\n".to_vec())]);
        let package=Package::from_files("fixture-plugin-skill",files).unwrap();
        let encoded: BTreeMap<_,_>=package.files.iter().map(|(p,b)|(p.clone(),data_encoding::BASE64.encode(b))).collect();
        let config=json!({"id":"plugin-fixture-remote","name":"plugin-fixture-remote","transport":"http","url":"https://example.test/mcp","enabled":false,"connected":false,"tools":[],"origin":"plugin","plugin":"fixture","editable":true,"runtime_available":true,"read_only_tools":[]});
        let summary=json!({"preview_id":"fixture-preview","name":"fixture","source":"openai/plugins","commit":"0123456789012345678901234567890123456789","package_sha256":"digest","skills":[{"name":package.skill.name,"sha256":package.sha256}],"mcp":[{"name":"plugin-fixture-remote","transport":"http","endpoint":"https://example.test/mcp","enabled":false,"config_sha256":digest(&config)}],"unsupported":["Host app connectors are not supported"],"installable":true});
        let row=json!({"owner":"chat","summary":summary,"packages":[{"name":package.skill.name,"sha256":package.sha256,"files":encoded}],"configs":[config],"status":"preview"});
        ui_store::put(&fixture.db.lock().unwrap(),PREVIEW,"fixture-preview",row).unwrap();
        (fixture,summary)
    }
    #[test]
    fn approved_install_recovers_after_interrupted_skill_write_and_is_idempotent() {
        let (fixture,approved)=fixture(); let db=&fixture.db; let cancel=AtomicBool::new(false);
        assert!(summary(db,"other","fixture-preview").is_err());
        let mut changed=approved.clone();changed["commit"]=json!("changed");
        assert!(install(db,"chat","fixture-preview",&changed,&cancel).is_err());
        assert_eq!(list(db).unwrap()["plugins"].as_array().unwrap().len(),0);
        // Simulate crash just after a successful first independent skill write.
        let source="plugin:fixture@0123456789012345678901234567890123456789#fixture-preview";
        let package=Package::from_files("fixture-plugin-skill", BTreeMap::from([("SKILL.md".to_owned(),b"---\nname: fixture-plugin-skill\ndescription: Explain fixture plugin tests\n---\nDo the relevant task.\n".to_vec())])).unwrap();
        let pending=json!({"name":"fixture","status":"installing","preview_id":"fixture-preview","owner":"chat","source":"openai/plugins","commit":approved["commit"],"package_sha256":"digest","skills":[],"mcp":[],"unsupported":approved["unsupported"]});
        ui_store::put(&db.lock().unwrap(),INSTALLED,"fixture",pending).unwrap();
        crate::ui_skills::owned(db).unwrap().install(&package,source,Some(0)).unwrap();
        let incomplete=status(db,"chat","fixture-preview").unwrap();
        assert_eq!(incomplete["installation"]["status"],"installing");
        assert_eq!(incomplete["installation"]["skills"][0]["name"],"fixture-plugin-skill");
        let installed=install(db,"chat","fixture-preview",&approved,&cancel).unwrap();
        assert_eq!(installed["plugin"]["status"],"installed");
        assert_eq!(installed["plugin"]["skills"][0]["revision"],1);
        assert_eq!(install(db,"chat","fixture-preview",&approved,&cancel).unwrap()["replayed"],true);
        let conn=db.lock().unwrap();let config=ui_store::get(&conn,MCP,"plugin-fixture-remote").unwrap().unwrap();
        assert_eq!(config["enabled"],false);assert_eq!(config["tools"],json!([]));
        drop(conn);
        let mut current=config;current["enabled"]=json!(true);current["connected"]=json!(true);
        ui_store::put(&db.lock().unwrap(),MCP,"plugin-fixture-remote",current).unwrap();
        let listed=list(db).unwrap();
        assert_eq!(listed["plugins"][0]["mcp"][0]["configuration_status"],"configured");
        assert_eq!(listed["plugins"][0]["mcp"][0]["enabled"],true);
        assert_eq!(listed["plugins"][0]["mcp"][0]["recorded_connected"],true);
        let loaded=crate::ui_skills::load_for_model(db,&json!({"id":"main"}),"fixture-plugin-skill",true).unwrap();
        assert!(loaded.text.contains("Do the relevant task"));
    }
    #[test]
    fn changed_snapshot_or_conflicting_skill_prevents_any_install() {
        let (fixture,approved)=fixture(); let db=&fixture.db; let cancel=AtomicBool::new(false);
        let mut row=ui_store::get(&db.lock().unwrap(),PREVIEW,"fixture-preview").unwrap().unwrap();
        row["configs"][0]["url"]=json!("https://attacker.test/mcp");
        ui_store::put(&db.lock().unwrap(),PREVIEW,"fixture-preview",row).unwrap();
        assert!(install(db,"chat","fixture-preview",&approved,&cancel).is_err());
        assert!(list(db).unwrap()["plugins"].as_array().unwrap().is_empty());
        assert!(crate::ui_skills::owned(db).unwrap().get("fixture-plugin-skill").unwrap().is_none());
    }
    #[test]
    fn installing_stdio_mcp_records_disabled_sandbox_without_spawning() {
        let (fixture,mut approved)=fixture();let db=&fixture.db;let cancel=AtomicBool::new(false);
        let directory=tempfile::tempdir().unwrap();let marker=directory.path().join("must-not-exist");
        let config=json!({"id":"plugin-fixture-process","name":"plugin-fixture-process","transport":"stdio",
            "command":"touch","args":[marker.to_str().unwrap()],
            "sandbox":{"read_roots":[],"write_roots":[],"hidden_roots":[],"network":true},
            "enabled":false,"connected":false,"tools":[],"origin":"plugin","plugin":"fixture","editable":true,"runtime_available":true,"read_only_tools":[]});
        approved["mcp"]=json!([{"name":"plugin-fixture-process","transport":"stdio","command":"touch",
            "args":[marker.to_str().unwrap()],"enabled":false,"setup_required":true,"config_sha256":digest(&config)}]);
        let mut row=ui_store::get(&db.lock().unwrap(),PREVIEW,"fixture-preview").unwrap().unwrap();
        row["summary"]=approved.clone();row["configs"]=json!([config]);
        ui_store::put(&db.lock().unwrap(),PREVIEW,"fixture-preview",row).unwrap();
        let result=install(db,"chat","fixture-preview",&approved,&cancel).unwrap();
        assert_eq!(result["plugin"]["mcp"][0]["enabled"],false);
        assert!(!marker.exists(),"installation unexpectedly spawned the process");
        let config=ui_store::get(&db.lock().unwrap(),MCP,"plugin-fixture-process").unwrap().unwrap();
        assert_eq!(config["enabled"],false);
        assert!(!kageha_mcp::Config::from_legacy(&config).unwrap().inherit_env);
    }
    #[test]
    #[ignore = "requires network access to the official plugin repository"]
    fn official_figma_hook_preview_and_install() {
        let fixture=crate::audited_session_tests::Fixture::new();let cancel=AtomicBool::new(false);
        let prepared=preview(&fixture.db,"chat","figma",&cancel).unwrap();
        assert_eq!(prepared["hooks"].as_array().unwrap().len(),2,"{prepared}");
        assert!(prepared["hooks"].as_array().unwrap().iter().any(|h|h["matcher"]=="write_file" && h["resources"]["scripts/post_write_figma_parity_check.sh"].as_str().unwrap().contains("Figma parity")));
        let installed=install(&fixture.db,"chat",prepared["preview_id"].as_str().unwrap(),&prepared,&cancel).unwrap();
        assert_eq!(installed["plugin"]["hooks_enabled"],false);
        crate::ui_plugin_components::set_hooks_enabled(&fixture.db,"figma",true).unwrap();
        let set=kageha_workspace::hooks::Set{hooks:crate::ui_plugin_components::active_hooks(&fixture.db).unwrap()};
        let workspace=tempfile::tempdir().unwrap();
        let result=crate::ui_hooks::run(&fixture.db,crate::audited_session_tests::CHAT,&fixture.turn,workspace.path(),&set,"postToolUse",
            &json!({"id":"figma-write","name":"write_file","arguments":{"path":"index.html"}}),&cancel).unwrap();
        assert_eq!(result.ran,1);assert!(result.extra_context.contains("Figma parity"));
    }
    #[test]
    #[ignore = "requires network access to the official plugin repository"]
    fn official_install_and_app_only_preview() {
        let fixture=crate::audited_session_tests::Fixture::new();let cancel=AtomicBool::new(false);
        let app=preview(&fixture.db,"chat","gmail",&cancel).unwrap();
        assert_eq!(app["installable"],true);assert_eq!(app["apps"][0]["integration"],"gmail");
        let installed_app=install(&fixture.db,"chat",app["preview_id"].as_str().unwrap(),&app,&cancel).unwrap();
        assert_eq!(installed_app["plugin"]["apps"][0]["integration"],"gmail");
        assert!(!crate::native_auth::readable(&fixture.db,"main").contains(&"gmail".to_string()));
        let prepared=preview(&fixture.db,"chat","remotion",&cancel).unwrap();
        assert_eq!(prepared["installable"],true,"{prepared}");
        let installed=install(&fixture.db,"chat",prepared["preview_id"].as_str().unwrap(),&prepared,&cancel).unwrap();
        assert_eq!(installed["plugin"]["status"],"installed");
        let skill=installed["plugin"]["skills"][0]["name"].as_str().unwrap();
        assert!(!crate::ui_skills::load_for_model(&fixture.db,&json!({"id":"main"}),skill,true).unwrap().text.is_empty());
    }
}
