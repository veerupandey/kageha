//! The standalone host has no dependency on Kageha's server, application DB or Python.
use kageha_models::{HttpModel, Provider};
use kageha_runtime::lifecycle::Limits;
use kageha_runtime_service::{ExecutionHost, ReadWorkspace};
use kageha_runtime_store::SqliteRunStore;
use serde::Deserialize;
use std::{path::Path, sync::Arc, time::Duration};
fn default_git()->std::path::PathBuf{"/usr/bin/git".into()}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    agent_profiles: Vec<kageha_agents::Definition>,
    #[serde(default)]
    skill_registry: Option<std::path::PathBuf>,
    #[serde(default)]
    profile_knowledge: Option<kageha_runtime_service::activation::Knowledge>,
    #[serde(default)]
    agent_repository: Option<AgentRepository>,
    #[serde(default)]
    hook_files: Vec<std::path::PathBuf>,
    #[serde(default)]
    specialists: Vec<kageha_runtime_service::workers::Specialist>,
    #[serde(default="default_git")]
    git_binary: std::path::PathBuf,
    #[serde(default)]
    browser: Option<kageha_tools::browser::Config>,
    #[serde(default)]
    remote:Option<kageha_execution::remote::Config>,
    #[serde(default)]
    workspace_worker:Option<std::path::PathBuf>,
    #[serde(default)]
    providers: Vec<Definition>,
    #[serde(default)]
    credential_service: Option<CredentialService>,
    #[serde(default)]
    catalog_credentials: std::collections::BTreeMap<String,kageha_contracts::credentials::CredentialRef>,
    #[serde(default)]
    catalog_files: Vec<std::path::PathBuf>,
    #[serde(default)]
    catalog_models: Vec<String>,
    #[serde(default)]
    memory: Option<MemoryDefinition>,
    #[serde(default)]
    mcp_servers: Vec<McpDefinition>,
    #[serde(default)]
    approval_tools: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentRepository { path:std::path::PathBuf, scope:String }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    endpoint: String,
    model: String,
    protocol: String,
    credentials_env: Option<String>,
    #[serde(default)]
    credential_ref: Option<kageha_contracts::credentials::CredentialRef>,
}
#[derive(Deserialize)]
#[serde(tag="transport",rename_all="snake_case",deny_unknown_fields)]
enum CredentialService {
    Unix {socket:std::path::PathBuf,token_file:std::path::PathBuf,principal:String},
    Https {endpoint:String,identity_file:std::path::PathBuf,ca_file:std::path::PathBuf,principal:String},
}
fn private_credential_input(path:&Path)->Result<kageha_contracts::credentials::SecretBytes,Box<dyn std::error::Error>> {
    use std::{io::Read,os::unix::fs::{MetadataExt,OpenOptionsExt}};
    let file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK).open(path)?;let metadata=file.metadata()?;
    if !metadata.is_file()||metadata.uid()!=unsafe{libc::geteuid()}||metadata.mode()&0o077!=0||metadata.len()>1024*1024{return Err("Credential client input must be a bounded private file".into());}
    let mut bytes=Vec::new();file.take(1024*1024+1).read_to_end(&mut bytes)?;Ok(kageha_contracts::credentials::SecretBytes::new(bytes))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpDefinition {
    #[serde(default)]
    sandbox: serde_json::Value,
    id: String,
    transport: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    env_refs: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    header_refs: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    read_only_tools: Vec<String>,
}
fn resolve_refs(
    refs: std::collections::BTreeMap<String, String>,
) -> Result<std::collections::BTreeMap<String, String>, Box<dyn std::error::Error>> {
    refs.into_iter()
        .map(|(key, reference)| {
            if reference.is_empty()
                || reference.len() > 128
                || !reference
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err("Invalid MCP credential reference".into());
            }
            Ok((
                key,
                std::env::var(reference).map_err(|_| "Configured MCP credential is unavailable")?,
            ))
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryDefinition {
    initial: String,
    providers: Vec<MemoryProvider>,
}
#[derive(Deserialize)]
#[serde(tag = "adapter", rename_all = "kebab-case", deny_unknown_fields)]
enum MemoryProvider {
    Sqlite {
        id: String,
    },
    MemoryHttp {
        id: String,
        endpoint: String,
        credentials_env: String,
    },
}
async fn memory_registry(
    root: &Path,
    config: MemoryDefinition,
) -> Result<Arc<kageha_services::Registry>, Box<dyn std::error::Error>> {
    if config.providers.is_empty() || config.providers.len() > 16 {
        return Err("Configure one to sixteen memory implementations".into());
    }
    let registry = Arc::new(kageha_services::Registry::open(
        &root.join("registry.sqlite"),
    )?);
    let mut names = std::collections::HashSet::new();
    for definition in config.providers {
        let id = match &definition {
            MemoryProvider::Sqlite { id } | MemoryProvider::MemoryHttp { id, .. } => id,
        };
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || !names.insert(id.clone())
        {
            return Err("Invalid or duplicate memory implementation ID".into());
        }
        let id = id.clone();
        let port: Arc<dyn kageha_contracts::MemoryPort> = match definition {
            MemoryProvider::Sqlite { .. } => Arc::new(kageha_memory::sqlite::SqliteMemory::open(
                &root.join(format!("{id}.sqlite")),
            )?),
            MemoryProvider::MemoryHttp {
                endpoint,
                credentials_env,
                ..
            } => {
                let token = resolve_refs(std::collections::BTreeMap::from([(
                    "token".into(),
                    credentials_env,
                )]))?
                .remove("token")
                .unwrap();
                Arc::new(kageha_memory::http::HttpMemory::new(&endpoint, token)?)
            }
        };
        registry.register(id, port).await?;
    }
    if !names.contains(&config.initial) {
        return Err("Initial memory implementation is unavailable".into());
    }
    let binding = registry.initialize(&config.initial).await?;
    if !names.contains(&binding.provider_id) {
        return Err("Current memory binding is absent from configuration".into());
    }
    registry.recover_pending().await?;
    Ok(registry)
}
fn state_lease(state:&Path)->std::io::Result<std::fs::File>{
    use std::os::{fd::AsRawFd,unix::fs::{DirBuilderExt,OpenOptionsExt,PermissionsExt}};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(state)?;
    let meta=std::fs::symlink_metadata(state)?;if !meta.is_dir()||meta.file_type().is_symlink()||meta.permissions().mode()&0o077!=0{return Err(std::io::Error::other("Runtime state directory must be private"));}
    let file=std::fs::OpenOptions::new().create(true).read(true).write(true).mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK).open(state.join("runtime-service.lock"))?;
    if !file.metadata()?.is_file(){return Err(std::io::Error::other("Runtime state lease must be regular"));}
    if unsafe{libc::flock(file.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}!=0{return Err(std::io::Error::other("Another runtime service already owns this state directory"));}Ok(file)
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let arg = |name: &str| -> Result<String, Box<dyn std::error::Error>> {
        args.iter().position(|s|s==name).and_then(|i|args.get(i+1)).cloned().ok_or_else(||format!("Supply {name}. Usage: kageha-runtime-service --state DIR --workspace DIR --config PATH --token-file PATH --bind 127.0.0.1:PORT").into())
    };
    if args.get(1).is_some_and(|s|s=="bridge") {
        let token_path=std::path::PathBuf::from(arg("--token-file")?);
        let meta=std::fs::symlink_metadata(&token_path)?;
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;if meta.permissions().mode()&0o077!=0{return Err("Runtime token must be private".into());}}
        if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>8192{return Err("Invalid runtime token file".into());}
        let token=std::fs::read_to_string(token_path)?.trim().to_owned();
        return kageha_runtime_service::bridge::serve(std::path::Path::new(&arg("--state")?),&arg("--bind")?,token).await;
    }
    let _lease=state_lease(Path::new(&arg("--state")?))?;
    let bytes = std::fs::read(arg("--config")?)?;
    if bytes.len() > 65536 {
        return Err("Runtime configuration exceeds limit".into());
    }
    let config: Config = serde_json::from_slice(&bytes)?;
    let mut providers = Vec::new();
    let mut credential_refs=Vec::new();
    let mut local_sources:Vec<Option<kageha_credentials::sources::Source>>=Vec::new();
    let (credential_resolver,credential_principal):(Option<Arc<dyn kageha_contracts::credentials::CredentialResolver>>,String)=match config.credential_service {
        Some(CredentialService::Unix{socket,token_file,principal})=>{let token=private_credential_input(&token_file)?;let token=std::str::from_utf8(token.expose()).map_err(|_|"Invalid credential client token")?.trim().as_bytes().to_vec();(Some(Arc::new(kageha_credentials::client::Client::unix(&socket,kageha_contracts::credentials::SecretBytes::new(token))?)),principal)},
        Some(CredentialService::Https{endpoint,identity_file,ca_file,principal})=>{let identity=private_credential_input(&identity_file)?;let ca=kageha_credentials::config::protected_input(&ca_file,1024*1024)?;(Some(Arc::new(kageha_credentials::client::Client::hosted(&endpoint,identity.expose(),ca.expose())?)),principal)},
        None=>(None,String::new()),
    };
    if !config.catalog_models.is_empty() {
        if config.catalog_files.is_empty() { return Err("Supply catalog_files for catalog_models".into()); }
        let catalog = kageha_models::catalog::Catalog::load(&config.catalog_files, |name| std::env::var(name).ok())?;
        for id in &config.catalog_models {
            if credential_resolver.is_some(){
                let row=catalog.models.iter().find(|v|v["id"]==*id).ok_or("Unknown catalog model")?;
                let protocol=row["protocol"].as_str().ok_or("Catalog protocol missing")?;
                providers.push(Provider{url:kageha_models::catalog::completion_url(row["base_url"].as_str().ok_or("Catalog endpoint missing")?,protocol)?,key:String::new(),model:row["model"].as_str().ok_or("Catalog model missing")?.into(),protocol:protocol.into()});
                credential_refs.push(config.catalog_credentials.get(id).cloned().ok_or("Each catalog model requires an explicit credential reference when using the vault")?);
            }else{let row=catalog.models.iter().find(|v|v["id"]==*id).ok_or("Unknown catalog model")?;let protocol=row["protocol"].as_str().ok_or("Catalog protocol missing")?;providers.push(Provider{url:kageha_models::catalog::completion_url(row["base_url"].as_str().ok_or("Catalog endpoint missing")?,protocol)?,key:String::new(),model:row["model"].as_str().ok_or("Catalog model missing")?.into(),protocol:protocol.into()});let name=row["api_key_env"].as_str().unwrap_or("");let aliases:&[&str]=match name{"CLOUDFLARE_API_TOKEN"=>&["CLOUDFLARE_API_KEY","CF_API_TOKEN","CF_API_KEY"],"FAL_KEY"=>&["FAL_API_KEY"],_=>&[]};let selected=std::iter::once(name).chain(aliases.iter().copied()).find(|n|std::env::var(n).is_ok_and(|v|!v.is_empty())).unwrap_or(name);local_sources.push((!selected.is_empty()).then(||kageha_credentials::sources::Source::Environment(selected.into())));}
        }
    }
    for definition in config.providers {
        if credential_resolver.is_some() {
            if definition.credentials_env.is_some(){return Err("Vault-backed models cannot fall back to environment credentials".into());}
            credential_refs.push(definition.credential_ref.clone().ok_or("Each model requires an explicit vault credential reference")?);
        }else if definition.credential_ref.is_some(){return Err("A credential service is required for referenced model credentials".into());}
        if credential_resolver.is_none(){let source=if let Some(reference)=definition.credentials_env {if reference.is_empty()||reference.len()>128||!reference.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'){return Err("Invalid credential reference".into());}Some(kageha_credentials::sources::Source::Environment(reference))}else{None};local_sources.push(source);}
        providers.push(Provider {
            url: definition.endpoint,
            key:String::new(),
            model: definition.model,
            protocol: definition.protocol,
        });
    }
    let token = std::fs::read_to_string(arg("--token-file")?)?
        .trim()
        .to_owned();
    let store = Arc::new(SqliteRunStore::open(
        &Path::new(&arg("--state")?).join("runtime.sqlite"),
    )?);
    if providers.iter().all(|provider| provider.protocol == "openai_embeddings") {
        return Err("The runtime needs at least one completion adapter".into());
    }
    let route_names: Vec<_> = providers.iter().enumerate().map(|(index, provider)| {
        let binding = kageha_agents::hash(&serde_json::json!([
            provider.url, provider.protocol, provider.model, credential_refs.get(index),
            local_sources.get(index).and_then(Option::as_ref).map(|source| match source {
                kageha_credentials::sources::Source::Environment(name) => name.as_str(),
                _ => "",
            }),
            credential_principal
        ]));
        (provider.model.clone(), config.catalog_models.get(index).cloned(), binding)
    }).collect();
    let model=Arc::new(if let Some(resolver)=credential_resolver{
        let configurations=providers.into_iter().zip(credential_refs).map(|(provider,reference)|{
            let destination=reqwest::Url::parse(&provider.url).map_err(|_|"Invalid model endpoint")?.origin().ascii_serialization();
            Ok((provider,kageha_models::CredentialBinding{allow_empty:false,reference,access:kageha_contracts::credentials::CredentialAccess{principal:credential_principal.clone(),purpose:"model".into(),destination}}))
        }).collect::<Result<Vec<_>,Box<dyn std::error::Error>>>()?;
        HttpModel::with_credentials(configurations,resolver)?
    }else{let mut bindings=Vec::new();let configurations=providers.into_iter().zip(local_sources).enumerate().map(|(index,(provider,source))|{let binding=if let Some(source)=source{let access=kageha_contracts::credentials::CredentialAccess{principal:"runtime".into(),purpose:"model".into(),destination:reqwest::Url::parse(&provider.url).map_err(|_|"Invalid model endpoint")?.origin().ascii_serialization()};let reference=kageha_contracts::credentials::CredentialRef{namespace:"runtime.explicit_sources".into(),id:format!("model-{index}")};bindings.push(kageha_credentials::sources::SourceBinding{reference:reference.clone(),access:access.clone(),source});Some(kageha_models::CredentialBinding{allow_empty:false,reference,access})}else{None};Ok((provider,binding))}).collect::<Result<Vec<_>,Box<dyn std::error::Error>>>()?;HttpModel::with_optional_credentials(configurations,Arc::new(kageha_credentials::sources::ReadOnlySources::new(bindings)?))?});
    let mut ports:Vec<Arc<dyn kageha_contracts::execution::ToolPort+Send+Sync>>=Vec::new();
    if let Some(browser)=config.browser {ports.push(Arc::new(kageha_tools::browser::BrowserTools::new(browser)?));}
    let execution=if let Some(remote)=config.remote {
        let journal=Arc::new(kageha_execution::journal::Journal::open(&Path::new(&arg("--state")?).join("execution.sqlite"))?);
        ports.push(Arc::new(kageha_execution::Tools::workspace(remote,Path::new(&arg("--workspace")?),journal.clone())?));Some(journal)
    }else{ports.push(Arc::new(ReadWorkspace::new(Path::new(&arg("--workspace")?))?));None};
    if !config.mcp_servers.is_empty() {
        let pool = Arc::new(kageha_mcp::Pool::open(
            &Path::new(&arg("--state")?).join("mcp.sqlite"),
        )?);
        let mut servers = Vec::new();
        for d in config.mcp_servers {
            let mut server: kageha_mcp::Config = serde_json::from_value(serde_json::json!({
                "id": d.id,
                "transport": d.transport,
                "sandbox": d.sandbox,
                "command": d.command,
                "args": d.args,
                "url": d.url,
                "read_only_tools": d.read_only_tools,
                "inherit_env": false
            }))?;
            server.env = resolve_refs(d.env_refs)?;
            server.headers = resolve_refs(d.header_refs)?;
            servers.push(server);
        }
        let adapter = tokio::task::spawn_blocking(move || {
            kageha_mcp::McpTools::connect(
                pool,
                servers,
                &kageha_contracts::execution::Control {
                    cancelled: &std::sync::atomic::AtomicBool::new(false),
                    deadline: None,
                },
            )
        })
        .await??;
        ports.push(Arc::new(adapter));
    }
    let memory = if let Some(definition) = config.memory {
        let registry =
            memory_registry(&Path::new(&arg("--state")?).join("memory"), definition).await?;
        let workspace = std::fs::canonicalize(arg("--workspace")?)?;
        ports.push(Arc::new(kageha_tools::memory::MemoryTools::sessions(
            registry.clone(),
            kageha_tools::memory::workspace_namespace(&workspace),
        )?));
        Some(registry)
    } else {
        None
    };
    if let Some(worker)=config.workspace_worker.clone() {ports.push(Arc::new(kageha_workspace::DocumentTools::new(Path::new(&arg("--workspace")?),&worker)?));}
    let tools = Arc::new(kageha_tools::Composite::new(ports)?);
    use kageha_runtime::execution::ToolPort;
    let mut exposed = tools.discover()?;
    if config.skill_registry.is_some() {
        if exposed.iter().any(|tool| tool["function"]["name"] == "read_skill_resource") {
            return Err("Configured tool collides with the skill resource reader".into());
        }
        exposed.push(serde_json::json!({"function":{"name":"read_skill_resource"}}));
    }
    exposed.push(serde_json::json!({"function":{"name":"project_hook"}}));
    exposed.extend(kageha_runtime_service::workers::specs());
    exposed.extend(kageha_runtime_service::planning::specs());
    exposed.push(serde_json::json!({"function":{"name":"approve_plan"}}));
    if config
        .approval_tools
        .iter()
        .any(|name| !exposed.iter().any(|tool| tool["function"]["name"] == *name))
    {
        return Err("Approval policy names an unavailable tool".into());
    }
    let policy = Arc::new(kageha_policy::RequireToolsPolicy::new(
        config.approval_tools,
    )?);
    let journal = Arc::new(kageha_policy::journal::Journal::open(
        &Path::new(&arg("--state")?).join("policy.sqlite"),
    )?);
    let specialists=if config.specialists.is_empty()&&config.agent_profiles.is_empty()&&config.agent_repository.is_none(){vec![kageha_runtime_service::workers::Specialist{id:"worker".into(),instructions:"Complete the delegated objective using the configured tool capabilities.".into(),allowed_tools:None}]}else{config.specialists};
    let worker_config=kageha_runtime_service::workers::Config{source:Path::new(&arg("--workspace")?).into(),journal:Path::new(&arg("--state")?).join("workspace/worktrees.sqlite"),git:config.git_binary,document_worker:config.workspace_worker,local_files:execution.is_none(),specialists};
    if config.agent_repository.is_some() && !config.agent_profiles.is_empty(){return Err("Choose persistent or inline profiles, not both".into());}
    let persistent_profiles:Option<Arc<dyn kageha_agents::Port>>=config.agent_repository.map(|r|kageha_agents::repository::sqlite::SqliteRepository::open(&r.path,&r.scope).map(|p|Arc::new(p) as Arc<dyn kageha_agents::Port>)).transpose()?;
    let mut workers=if let Some(port)=persistent_profiles {kageha_runtime_service::workers::Composition::new_with_profiles(worker_config,port)?}else if config.agent_profiles.is_empty(){kageha_runtime_service::workers::Composition::new(worker_config)?}else{kageha_runtime_service::workers::Composition::new_with_profiles(worker_config,Arc::new(kageha_agents::Catalog::new(config.agent_profiles)?))?};
    let mut named_routes = std::collections::BTreeMap::<String, Option<_>>::new();
    let mut embedding_routes = std::collections::BTreeMap::<String, Option<_>>::new();
    for (index, (model_name, catalog_name, binding)) in route_names.into_iter().enumerate() {
        if let Some(adapter) = model.select_embedding_adapter(index) {
            let adapter: kageha_runtime_service::activation::Embedding = Arc::new(adapter);
            embedding_routes.entry(model_name).and_modify(|v| *v = None)
                .or_insert(Some((binding, adapter)));
            continue;
        }
        let Some(adapter) = model.select_chat_adapter(index) else { continue };
        let adapter: kageha_runtime_service::activation::Model = Arc::new(adapter);
        let names = std::iter::once(model_name).chain(catalog_name).collect::<std::collections::BTreeSet<_>>();
        for name in names {
            named_routes.entry(name).and_modify(|v| *v = None)
                .or_insert_with(|| Some((binding.clone(), adapter.clone())));
        }
    }
    kageha_runtime_service::workers::Composition::configure_resources(
        &mut workers,
        kageha_runtime_service::activation::Resources {
            embeddings: embedding_routes.into_iter().filter_map(|(name, route)| route.map(|r| (name, r))).collect(),
            skills: config.skill_registry,
            knowledge: config.profile_knowledge,
            models: named_routes.into_iter().filter_map(|(name, route)| route.map(|r| (name, r))).collect(),
        },
    )?;
    let hooks=Arc::new(kageha_runtime_service::hooks::Lifecycle::new(Path::new(&arg("--workspace")?),config.hook_files,Arc::new(kageha_execution::journal::Journal::open(&Path::new(&arg("--state")?).join("hooks/execution.sqlite"))?),execution.is_none())?);
    let planning=Arc::new(kageha_runtime_service::planning::Coordinator::new(Arc::new(kageha_workflow::planning::sqlite::SqliteStore::open(&Path::new(&arg("--state")?).join("planning/plans.sqlite"))?)));
    let host = ExecutionHost::with_components(store, model, tools, Limits::default(), policy, journal,kageha_runtime_service::Components{workers:Some(workers),hooks:Some(hooks),planning:Some(planning)})?;
    let mut router = kageha_runtime_service::http::router(host.clone(), token.clone())?;
    if let Some(journal)=execution {router=router.merge(kageha_execution::http::router(journal,token.clone())?);}
    if let Some(registry) = memory {
        router = router.merge(kageha_services::http::router(registry, token)?);
    }
    host.supervisor.start()?;
    let listener = tokio::net::TcpListener::bind(arg("--bind")?).await?;
    eprintln!("runtime listening on {}", listener.local_addr()?);
    let shutdown = host.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
            let _ = tokio::task::spawn_blocking(move || shutdown.supervisor.shutdown()).await;
        })
        .await?;
    tokio::task::spawn_blocking(move || host.supervisor.join(Duration::from_secs(15))).await??;
    Ok(())
}
