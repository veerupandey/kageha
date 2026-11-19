use crate::application_journal::{ActionBackend, ActionJournal};
use crate::cdp::{self, field, Cdp, Result};
use crate::{
    action_journal, actions, observe, panel, protocol, research, script, script_artifact,
    watchdog::OwnedChromium,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Weak;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

pub const METHODS: &[&str] = &[
    "launch_browser",
    "open_tab",
    "list_tabs",
    "observe_tab",
    "disconnect_browser",
    "close_browser",
    "browser_selection",
    "switch_profile",
    "detach_profile",
    "select_tab",
    "list_user_tabs",
    "claim_user_tab",
    "query_tab",
    "submission_context",
    "confirm_submission",
    "preview_tab",
    "capture_tab",
    "panel_action",
    "browser_adapters",
    "read_page",
];
const MAX_UNCONFIGURED_BROWSERS: usize = 16;
const MAX_TABS: usize = 32;
const PROFILE_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// Reading the selected tab. The attached limit stays inside `PROFILE_PROBE_TIMEOUT`.
const TAB_READ_OWNED: Duration = Duration::from_secs(2);
const TAB_READ_ATTACHED: Duration = Duration::from_millis(600);
/// How long a tab that failed to read is skipped before it is tried again.
const UNREADABLE_TAB_MEMORY: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostedAdapters {
    workspace_id: String,
    endpoints: HashMap<String, String>,
    embedded_target: Option<String>,
}

impl HostedAdapters {
    pub(crate) fn load(path: &str) -> Result<Self> {
        if !Path::new(path).is_absolute() {
            return Err("expected an absolute path");
        }
        let file = std::fs::File::open(path).map_err(|_| "could not read adapter configuration")?;
        let mut data = Vec::new();
        file.take(8193)
            .read_to_end(&mut data)
            .map_err(|_| "could not read adapter configuration")?;
        let config: Self =
            serde_json::from_slice(&data).map_err(|_| "invalid adapter configuration")?;
        let ports: BTreeSet<_> = config
            .endpoints
            .values()
            .map(|endpoint| cdp::endpoint_port(endpoint).ok_or("expected a loopback CDP endpoint"))
            .collect::<Result<_>>()?;
        if data.len() > 8192
            || config.workspace_id.is_empty()
            || config.workspace_id.len() > protocol::limit("id_bytes")
            || config.endpoints.keys().any(|name| !hosted_profile(name))
            || ports.len() != config.endpoints.len()
            || config.endpoints.contains_key("embedded") != config.embedded_target.is_some()
            || config.embedded_target.as_ref().is_some_and(|target| {
                target.is_empty()
                    || target.len() > protocol::limit("id_bytes")
                    || !target
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err("invalid adapter configuration");
        }
        Ok(config)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineAdapter {
    endpoint: String,
    #[serde(flatten)]
    transfer: crate::workstation_transfer::Transfer,
}

impl MachineAdapter {
    fn load(root: &Path, workspace: &str) -> Option<Self> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        if workspace.is_empty() || workspace.len() > 64
            || !workspace.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
            return None;
        }
        let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("machine-bindings").join(workspace)).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.uid() != crate::state::uid() || metadata.mode() & 0o077 != 0 {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(8193).read_to_end(&mut bytes).ok()?;
        let value: Self = serde_json::from_slice(&bytes).ok()?;
        let metadata = std::fs::symlink_metadata(&value.transfer.transfer).ok()?;
        if bytes.len() > 8192 || cdp::endpoint_port(&value.endpoint).is_none() || !value.transfer.transfer.is_absolute()
            || !value.transfer.guest_transfer.is_absolute()
            || !metadata.is_dir() || metadata.uid() != crate::state::uid() || metadata.mode() & 0o077 != 0 {
            return None;
        }
        Some(value)
    }
}

fn hosted_profile(name: &str) -> bool {
    matches!(name, "docker" | "machine" | "remote" | "embedded")
}

#[derive(Default)]
pub(crate) struct Adapters {
    pub lightpanda: Option<PathBuf>,
    pub curl: Option<PathBuf>,
    pub hosted: Option<HostedAdapters>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenTab {
    browser_id: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserveTab {
    browser_id: String,
    target_id: String,
    #[serde(default)]
    compact: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserRef {
    browser_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileRef {
    profile: String,
    binding_id: String,
    #[serde(default)]
    consent: bool,
    endpoint: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionRef {
    browser_id: String,
    target_id: String,
    binding_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TabClaim {
    binding_id: String,
    browser_id: String,
    target_id: String,
    url: String,
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TabQuery {
    selection: SelectionRef,
    selector: String,
    frame_id: Option<String>,
    text: Option<String>,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
    wait_for: Option<String>,
    url: Option<String>,
    #[serde(default)]
    console: bool,
    value_probe: Option<crate::input_evidence::Probe>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ImageFormat {
    Jpeg,
    Png,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotRequest {
    selection: SelectionRef,
    format: ImageFormat,
    quality: u8,
    max_width: u64,
    #[serde(default)]
    full_page: bool,
    clip: Option<ScreenshotClip>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotClip {
    x: u64,
    y: u64,
    width: u64,
    height: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingRef {
    binding_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PanelRequest {
    selection: SelectionRef,
    viewport: Option<panel::Size>,
    input: panel::Input,
}

#[derive(Default)]
struct Workspace {
    active: String,
    before_machine: Option<String>,
    profiles: HashMap<String, Browser>,
    revision: u64,
    canary_epoch: Option<String>,
}

impl Workspace {
    fn selected(&self) -> Option<&Browser> {
        self.profiles.get(&self.active)
    }

    fn binding(&self) -> String {
        let binding = format!(
            "{}:{}",
            self.selected().map_or("empty", |b| b.id.as_str()),
            self.revision
        );
        self.canary_epoch
            .as_ref()
            .map_or_else(|| binding.clone(), |epoch| format!("{epoch}:{binding}"))
    }

    fn profiles(&self) -> Vec<Value> {
        let mut profiles: Vec<_> = self.profiles.iter().collect();
        profiles.sort_by_key(|(name, _)| *name);
        profiles
            .into_iter()
            .map(|(name, browser)| {
                json!({
                    "name": name, "browser_id": browser.id, "selected": *name == self.active,
                    "owned": browser.process.is_some(), "connected": browser.online,
                })
            })
            .collect()
    }

    fn disconnected(&self) -> Value {
        json!({"binding_id": self.binding(), "revision": self.revision,
               "profile": if self.active.is_empty() { "private" } else { &self.active },
               "browser_id": null, "target_id": null, "tabs": [], "profiles": self.profiles()})
    }
}

pub struct Browsers {
    executable: Option<PathBuf>,
    root: PathBuf,
    workspaces: Mutex<HashMap<String, Workspace>>,
    revision: AtomicU64,
    adapters: Adapters,
    research_slots: Semaphore,
    canary_epoch: Option<String>,
    initial_profile: String,
    pub action_backend: ActionBackend,
    pub scripts: script::Registry,
}

impl Browsers {
    pub(crate) fn research_http_transport(&self) -> Option<&'static str> {
        matches!(self.action_backend, ActionBackend::Application | ActionBackend::Standalone)
            .then_some(if self.adapters.curl.is_some() { "curl" } else { "native" })
    }

    pub(crate) async fn decision_evidence(&self,request:&protocol::Request,observation:Option<&str>)->Result<(Value,Value)> {
        let workspaces=self.workspaces.lock().await;
        let ws=workspaces.get(&request.workspace_id).ok_or("browser_not_launched")?;
        let browser=ws.selected().filter(|b|b.online).ok_or("cdp_unavailable")?;
        if request.method == "close_tab" {
            let target = request.params["target_id"].as_str().ok_or("invalid_params")?;
            if request.params["binding_id"] != ws.binding() || request.params["browser_id"] != browser.id {
                return Err("browser_binding_changed");
            }
            if !browser.targets.contains(target) { return Err("target_not_owned"); }
            let mut cdp = browser.connect().await?;
            let targets = cdp.call(None,"Target.getTargets",json!({})).await?;
            let tab = targets["targetInfos"].as_array().and_then(|tabs|tabs.iter().find(|tab|tab["targetId"]==target))
                .ok_or("target_not_owned")?;
            return Ok((json!({"binding_id":ws.binding(),"browser_id":browser.id,"target_id":target,"profile":ws.active}),
                json!({"url":tab["url"],"controls":[],"text":[]})));
        }
        if request.params["browser_id"].as_str().is_some_and(|id|id!=browser.id)
            || request.params["target_id"].as_str().is_some_and(|id|id!=browser.selected) {return Err("browser_binding_changed");}
        let snapshot=browser.observations.get(&browser.selected).filter(|(at,s)|at.elapsed()<Duration::from_secs(30)&&observation.is_none_or(|id|s["observation_id"]==id)).map(|(_,s)|s.clone());
        if observation.is_some()&&snapshot.is_none(){return Err("observation_stale");}
        Ok((json!({"binding_id":ws.binding(),"browser_id":browser.id,"target_id":browser.selected,"profile":ws.active}),snapshot.map(observe::expanded).unwrap_or(Value::Null)))
    }
    pub fn new(
        executable: Option<PathBuf>,
        root: PathBuf,
        action_backend: ActionBackend,
        adapters: Adapters,
        managed: Option<(String, String)>,
    ) -> Self {
        let (canary_epoch, initial_profile) = managed.map_or_else(
            || (None, String::new()),
            |(epoch, profile)| (Some(epoch), profile),
        );
        Self {
            executable,
            root,
            workspaces: Mutex::new(HashMap::new()),
            revision: AtomicU64::new(0),
            adapters,
            research_slots: Semaphore::new(2),
            canary_epoch,
            initial_profile,
            action_backend,
            scripts: script::Registry::default(),
        }
    }

    fn next_revision(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn empty_workspace(&self) -> Workspace {
        Workspace {
            active: self.initial_profile.clone(),
            canary_epoch: self.canary_epoch.clone(),
            ..Workspace::default()
        }
    }

    fn save_managed_profile(&self, profile: &str) -> Result<()> {
        if self.canary_epoch.is_some() {
            crate::state::save_managed_profile(&self.root, profile)
                .map_err(|_| "browser_profile_state_unavailable")?;
        }
        Ok(())
    }

    fn hosted_endpoints(&self, workspace: &str) -> Option<&HashMap<String, String>> {
        self.adapters
            .hosted
            .as_ref()
            .filter(|hosted| hosted.workspace_id == workspace)
            .map(|hosted| &hosted.endpoints)
    }

    fn research_backends(&self) -> Vec<&'static str> {
        if !matches!(
            self.action_backend,
            ActionBackend::Application | ActionBackend::Standalone
        ) {
            return Vec::new();
        }
        let mut backends = vec!["chromium"];
        if self.adapters.lightpanda.is_some() {
            backends.push("lightpanda");
        }
        if self.adapters.curl.is_some() {
            backends.push("http");
        }
        backends
    }

    async fn read_page(&self, request: &protocol::Request) -> Result<Value> {
        if !matches!(
            self.action_backend,
            ActionBackend::Application | ActionBackend::Standalone
        ) {
            return Err("research_backend_unavailable");
        }
        let input: research::ReadPage =
            serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
        if !(1..=research::MAX_CHARS).contains(&input.max_chars) || input.title_contains.len() > 200 {
            return Err("invalid_params");
        }
        research::validate_url(&input.url)?;
        let _slot = self
            .research_slots
            .acquire()
            .await
            .map_err(|_| "research_backend_unavailable")?;
        match input.backend {
            research::Backend::Http => {
                research::http(self.adapters.curl.as_deref(), &input).await
            }
            research::Backend::Lightpanda => {
                research::lightpanda(
                    self.adapters
                        .lightpanda
                        .as_deref()
                        .ok_or("research_backend_unavailable")?,
                    &input,
                )
                .await
            }
            research::Backend::Chromium => {
                let browser = Browser::launch(self.executable.as_deref().ok_or("local_chromium_unavailable")?, &self.root, true).await?;
                // The watchdog closes the browser and removes its profile if the
                // request times out while either extraction or shutdown is pending.
                let remaining = request.deadline_ms.saturating_sub(protocol::now_ms()).saturating_sub(200);
                let result = browser.research_page(&input, Duration::from_millis(remaining)).await;
                let closed = browser.close().await;
                if result.is_ok() { closed?; }
                result
            }
        }
    }

    pub async fn preview(&self, request: &protocol::Request) -> Result<(Value, Vec<u8>)> {
        let remaining = request.deadline_ms.saturating_sub(protocol::now_ms());
        tokio::time::timeout(
            Duration::from_millis(remaining),
            self.capture_preview(request),
        )
        .await
        .unwrap_or(Err("deadline_exceeded"))
    }

    async fn capture_preview(&self, request: &protocol::Request) -> Result<(Value, Vec<u8>)> {
        let (p, format, quality, max_width, maximum, full_page, clip) =
            if request.method == "capture_tab" {
                let capture: ScreenshotRequest =
                    serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
                if !(1..=100).contains(&capture.quality)
                    || capture.max_width > 4096
                    || (capture.full_page && capture.clip.is_some())
                {
                    return Err("invalid_params");
                }
                (
                    capture.selection,
                    capture.format,
                    capture.quality,
                    capture.max_width,
                    protocol::limit("screenshot_bytes"),
                    capture.full_page,
                    capture.clip,
                )
            } else {
                let selection =
                    serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
                let quality = request.params["quality"]
                    .as_u64()
                    .unwrap_or(82)
                    .clamp(50, 95) as u8;
                (
                    selection,
                    ImageFormat::Jpeg,
                    quality,
                    0,
                    protocol::limit("frame_bytes"),
                    false,
                    None,
                )
            };
        let (port, path, document, size) = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(&request.workspace_id)
                .ok_or("browser_not_launched")?;
            let b = ws.selected().ok_or("browser_not_launched")?;
            if ws.binding() != p.binding_id
                || !b.online
                || b.id != p.browser_id
                || b.selected != p.target_id
                || !b.targets.contains(&p.target_id)
            {
                return Err("browser_binding_changed");
            }
            (
                b.port,
                b.path.clone(),
                b.selected_document.clone(),
                b.viewport,
            )
        };
        // This connection never owns the workspace command transport.
        let mut cdp = Cdp::connect(port, &path, false).await?;
        let session = cdp.attach(&p.target_id).await?;
        let (frame_id, loader) = main_frame(&mut cdp, &session).await?;
        if loader != document {
            return Err("browser_binding_changed");
        }
        let viewport = panel::Viewport::read(&mut cdp, &session, &frame_id).await?;
        if Some(viewport.size) != size {
            return Err("browser_binding_changed");
        }
        let panel::Size {
            mut width,
            mut height,
            ..
        } = viewport.size;
        let (format, content_type) = match format {
            ImageFormat::Jpeg => ("jpeg", "image/jpeg"),
            ImageFormat::Png => ("png", "image/png"),
        };
        let mut params = json!({"format": format, "captureBeyondViewport": false});
        if format == "jpeg" {
            params["quality"] = json!(quality);
        }
        if full_page || clip.is_some() || (max_width > 0 && max_width < width) {
            let metrics = cdp
                .call(Some(&session), "Page.getLayoutMetrics", json!({}))
                .await?;
            let viewport = &metrics["cssVisualViewport"];
            let mut x = viewport["pageX"]
                .as_f64()
                .ok_or("preview_geometry_unavailable")?;
            let mut y = viewport["pageY"]
                .as_f64()
                .ok_or("preview_geometry_unavailable")?;
            if full_page {
                let content = &metrics["cssContentSize"];
                x = 0.0;
                y = 0.0;
                width = content["width"]
                    .as_u64()
                    .ok_or("preview_geometry_unavailable")?;
                height = content["height"]
                    .as_u64()
                    .ok_or("preview_geometry_unavailable")?;
                params["captureBeyondViewport"] = json!(true);
            } else if let Some(clip) = clip {
                if clip.width == 0
                    || clip.height == 0
                    || clip.x.saturating_add(clip.width) > width
                    || clip.y.saturating_add(clip.height) > height
                {
                    return Err("invalid_screenshot_clip");
                }
                x += clip.x as f64;
                y += clip.y as f64;
                width = clip.width;
                height = clip.height;
            }
            if width == 0 || height == 0 || width > 32_768 || height > 32_768 {
                return Err("screenshot_dimensions_too_large");
            }
            let max_width = if max_width == 0 { 4096 } else { max_width };
            let scale = (max_width as f64 / width as f64)
                .min(4096.0 / height as f64)
                .min(1.0);
            params["clip"] =
                json!({"x": x, "y": y, "width": width, "height": height, "scale": scale});
            width = ((width as f64 * scale).round() as u64).max(1);
            height = ((height as f64 * scale).round() as u64).max(1);
        }
        let frame = cdp
            .call(Some(&session), "Page.captureScreenshot", params)
            .await?;
        let encoded = field(&frame, "data")?;
        if encoded.len() > maximum.div_ceil(3) * 4 {
            return Err("preview_too_large");
        }
        let bytes = data_encoding::BASE64
            .decode(encoded.as_bytes())
            .map_err(|_| "invalid_preview")?;
        let valid = if format == "jpeg" {
            bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9])
        } else {
            bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.ends_with(b"IEND\xae\x42\x60\x82")
        };
        if bytes.len() > maximum || !valid {
            return Err("invalid_preview");
        }
        if panel::Viewport::read(&mut cdp, &session, &frame_id)
            .await?
            .size
            != viewport.size
            || main_frame(&mut cdp, &session).await?.1 != document
        {
            return Err("browser_binding_changed");
        }
        let workspaces = self.workspaces.lock().await;
        if workspaces
            .get(&request.workspace_id)
            .is_none_or(|ws| ws.binding() != p.binding_id)
        {
            return Err("browser_binding_changed");
        }
        Ok((
            json!({"binding_id": p.binding_id, "browser_id": p.browser_id, "target_id": p.target_id,
                   "width": width, "height": height, "content_type": content_type, "byte_length": bytes.len()}),
            bytes,
        ))
    }

    pub async fn dispatch(&self, request: &protocol::Request) -> Result<Value> {
        let remaining = request.deadline_ms.saturating_sub(protocol::now_ms());
        if remaining == 0 {
            return Err("deadline_exceeded");
        }
        let dispatched = AtomicBool::new(false);
        let result = tokio::time::timeout(
            Duration::from_millis(remaining),
            self.run(request, &dispatched),
        )
        .await
        .unwrap_or(Err("deadline_exceeded"));
        if result.is_err() && dispatched.load(Ordering::Relaxed) {
            eprintln!("browser uncertain method={} cause={}", request.method, result.as_ref().unwrap_err());
            Err("action_outcome_uncertain")
        } else {
            result
        }
    }

    async fn run(&self, request: &protocol::Request, dispatched: &AtomicBool) -> Result<Value> {
        let machine = MachineAdapter::load(&self.root, &request.workspace_id);
        // Retire stale browser bindings before any action, including requests from an older panel.
        {
            let mut workspaces = self.workspaces.lock().await;
            if let Some(ws) = workspaces.get_mut(&request.workspace_id) {
                if ws.active == "machine" && ws.before_machine.is_some() {
                    let endpoint_matches = machine.as_ref().is_some_and(|adapter| {
                        ws.selected().is_some_and(|browser| {
                            adapter.endpoint == format!("ws://127.0.0.1:{}{}", browser.port, browser.path)
                        })
                    });
                    if !endpoint_matches {
                        ws.profiles.remove("machine");
                        ws.active = ws.before_machine.take().unwrap_or_else(|| self.initial_profile.clone());
                        ws.revision = self.next_revision();
                    }
                }
            }
            if machine.is_some() && !matches!(request.method.as_str(), "browser_selection" | "browser_adapters" | "switch_profile")
                && workspaces.get(&request.workspace_id).is_none_or(|ws| ws.active != "machine") {
                return Err("machine_browser_unavailable");
            }
        }
        if request.method == "close_tab" {
            return self.close_tab(request, dispatched).await;
        }
        if matches!(request.method.as_str(),"submission_context"|"confirm_submission") {return self.submission_confirmation(request).await;}
        if request.method == "query_tab" {
            return self.query_tab(request).await;
        }
        if matches!(request.method.as_str(), "list_user_tabs" | "claim_user_tab") {
            return self.user_tabs(request).await;
        }
        if request.method == "read_page" {
            return self.read_page(request).await;
        }
        if request.method == "browser_adapters" {
            if request.params != json!({}) {
                return Err("invalid_params");
            }
            let hosted_profiles: BTreeSet<_> = self
                .hosted_endpoints(&request.workspace_id)
                .into_iter()
                .flat_map(|endpoints| endpoints.keys())
                .collect();
            return Ok(
                json!({"research": self.research_backends(), "hosted_profiles": hosted_profiles}),
            );
        }
        if action_journal::CONTROL_METHODS.contains(&request.method.as_str()) {
            if !matches!(self.action_backend,ActionBackend::Standalone|ActionBackend::Fixture) {
                return Err("unsupported_method");
            }
            return action_journal::control(&self.root, request);
        }
        if actions::METHODS.contains(&request.method.as_str()) {
            if self.action_backend == ActionBackend::Disabled {
                return Err("fixture_actions_unavailable");
            }
            return self.action(request, dispatched).await;
        }
        if request.method == "panel_action" {
            return self.panel_action(request, dispatched).await;
        }
        if request.method == "run_script" {
            if !self.scripts_available() {
                return Err("script_runner_unavailable");
            }
            return script::run(self, request, dispatched).await;
        }
        if request.method == "browser_selection" {
            if request.params != json!({}) {
                return Err("invalid_params");
            }
            let mut selection = self.selection(&request.workspace_id).await?;
            selection["tab_count"] = json!(selection["tabs"].as_array().map_or(0, Vec::len));
            selection["tab_limit"] = json!(MAX_TABS);
            if matches!(self.action_backend,ActionBackend::Standalone | ActionBackend::Fixture) {
                selection["input_status"] = crate::journal_store::status(&self.root);
            }
            if machine.is_some() {
                selection["available_profiles"] = json!(["machine"]);
                if selection["profile"] != "machine" {
                    selection["browser_id"] = Value::Null;
                    selection["target_id"] = Value::Null;
                    selection["tabs"] = json!([]);
                }
            } else if self.canary_epoch.is_some() {
                selection["available_profiles"] = json!(["private", "chrome", "comet"]);
            } else if let Some(endpoints) = self.hosted_endpoints(&request.workspace_id) {
                let profiles: BTreeSet<_> = endpoints.keys().collect();
                selection["available_profiles"] = json!(profiles);
            }
            return Ok(selection);
        }
        let mut workspaces = self.workspaces.lock().await;
        let workspace = &request.workspace_id;
        let method = request.method.as_str();
        if method == "launch_browser" || method == "switch_profile" {
            if machine.is_some() && (method != "switch_profile" || request.params["profile"] != "machine") {
                return Err("machine_browser_profile_required");
            }
            let (profile, endpoint) = if method == "switch_profile" {
                let p: ProfileRef =
                    serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
                let binding = workspaces
                    .get(workspace)
                    .map_or_else(|| self.empty_workspace().binding(), Workspace::binding);
                if p.binding_id != binding {
                    return Err("browser_binding_changed");
                }
                if p.profile.is_empty()
                    || p.profile.len() > 24
                    || !p
                        .profile
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                {
                    return Err("invalid_profile");
                }
                if self.canary_epoch.is_some() && machine.is_none()
                    && !matches!(p.profile.as_str(), "private" | "chrome" | "comet")
                {
                    return Err("browser_adapter_unavailable");
                }
                let endpoint = match (p.profile.as_str(), p.consent, p.endpoint) {
                    ("machine", false, None) if machine.is_some() =>
                        machine.as_ref().map(|adapter| adapter.endpoint.clone()),
                    ("chrome" | "comet" | "cdp", true, Some(endpoint)) => Some(endpoint),
                    ("chrome" | "comet" | "cdp", false, _) => {
                        return Err("browser_profile_consent_required")
                    }
                    ("chrome" | "comet" | "cdp", true, None) => return Err("cdp_invalid_endpoint"),
                    (name, false, None) if hosted_profile(name) => Some(
                        self.hosted_endpoints(workspace)
                            .and_then(|endpoints| endpoints.get(name))
                            .ok_or("browser_adapter_unavailable")?
                            .clone(),
                    ),
                    (
                        "electron" | "desktop" | "webview" | "safari" | "webkit" | "lightpanda"
                        | "http",
                        _,
                        _,
                    ) => return Err("browser_adapter_unavailable"),
                    (_, false, None) => None,
                    _ => return Err("invalid_params"),
                };
                (p.profile, endpoint)
            } else if request.params != json!({}) {
                return Err("invalid_params");
            } else {
                if self.canary_epoch.is_some()
                    && matches!(
                        workspaces
                            .get(workspace)
                            .map_or(self.initial_profile.as_str(), |ws| ws.active.as_str()),
                        "chrome" | "comet"
                    )
                {
                    return Err("browser_profile_reconnect_required");
                }
                ("private".into(), None)
            };
            if method == "launch_browser"
                && workspaces
                    .get(workspace)
                    .and_then(Workspace::selected)
                    .is_some()
            {
                return Err("browser_already_launched");
            }
            let existing = workspaces
                .get(workspace)
                .and_then(|ws| ws.profiles.get(&profile));
            if existing.is_none()
                && !hosted_profile(&profile)
                && workspaces
                    .values()
                    .flat_map(|ws| ws.profiles.iter())
                    .filter(|(name,_)| !hosted_profile(name))
                    .map(|(_,browser)|browser.port)
                    .collect::<BTreeSet<_>>().len()
                    >= MAX_UNCONFIGURED_BROWSERS
                && !endpoint.as_deref().and_then(cdp::endpoint_port).is_some_and(|port|workspaces.values().any(|ws|ws.profiles.get(&profile).is_some_and(|browser|browser.port==port&&browser.process.is_none())))
            {
                return Err("browser_capacity_exceeded");
            }
            let allow_web = matches!(
                self.action_backend,
                ActionBackend::Application | ActionBackend::Standalone
            );
            let attachment = if let Some(endpoint) = endpoint {
                if !allow_web {
                    return Err("browser_personal_profile_unavailable");
                }
                Some(
                    tokio::time::timeout(PROFILE_PROBE_TIMEOUT, async {
                        let (port, path) = cdp::discover(&endpoint).await?;
                        if workspaces.iter().any(|(id, ws)| {
                            ws.profiles.iter().any(|(name, browser)| {
                                browser.port == port
                                    && (name != &profile
                                        || browser.process.is_some()
                                        || (id != workspace && (self.canary_epoch.is_some() || !matches!(profile.as_str(),"chrome"|"comet"|"cdp"))))
                            })
                        }) || (!hosted_profile(&profile)
                            && self.adapters.hosted.as_ref().is_some_and(|hosted| {
                                hosted
                                    .endpoints
                                    .values()
                                    .any(|endpoint| cdp::endpoint_port(endpoint) == Some(port))
                            }))
                        {
                            return Err("browser_profile_endpoint_conflict");
                        }
                        let mut transport = Cdp::connect(port, &path, true).await?;
                        transport
                            .call(None, "Browser.getVersion", json!({}))
                            .await?;
                        let fixed_target = if profile == "embedded" {
                            let target = self
                                .adapters
                                .hosted
                                .as_ref()
                                .and_then(|hosted| hosted.embedded_target.as_deref())
                                .ok_or("browser_adapter_unavailable")?;
                            let response = transport
                                .call(None, "Target.getTargetInfo", json!({"targetId": target}))
                                .await?;
                            if !embedded_target(&response["targetInfo"], target) {
                                return Err("browser_adapter_unavailable");
                            }
                            Some(target.to_owned())
                        } else {
                            None
                        };
                        Ok((port, path, transport, fixed_target))
                    })
                    .await
                    .unwrap_or(Err("cdp_unavailable"))?,
                )
            } else {
                None
            };
            let ws = workspaces
                .entry(workspace.clone())
                .or_insert_with(|| self.empty_workspace());
            let mut changed = ws.active != profile;
            if let Some((port, path, transport, fixed_target)) = attachment {
                match ws.profiles.get_mut(&profile) {
                    Some(browser) if browser.port == port && browser.path == path => {
                        changed |= !browser.online;
                        browser.online = true;
                        browser.transport = Some(transport);
                    }
                    _ => {
                        let mut browser = Browser::new(None, port, path, transport, true);
                        browser.machine = machine.is_some();
                        if self.canary_epoch.is_some()
                            && matches!(profile.as_str(), "chrome" | "comet")
                        {
                            let retained = crate::state::load_personal_tabs(&self.root, &profile)
                                .map_err(|_| "browser_profile_state_unavailable")?;
                            if let Some(tabs) = retained.filter(|tabs| {
                                tabs.port == browser.port
                                    && tabs.debugger_path == browser.path
                                    && tabs.targets.len() <= MAX_TABS
                            }) {
                                browser.targets.extend(tabs.targets.iter().cloned());
                                if browser.targets.contains(&tabs.selected) {
                                    browser.selected = tabs.selected.clone();
                                }
                                browser.saved_personal_tabs = Some(tabs);
                            }
                        }
                        if let Some(target) = fixed_target {
                            browser.targets.insert(target.clone());
                            browser.selected = target.clone();
                            browser.fixed_target = Some(target);
                        }
                        ws.profiles.insert(profile.clone(), browser);
                        changed = true;
                    }
                }
            } else {
                let retained = match ws.profiles.get_mut(&profile) {
                    Some(browser) => browser.alive()?,
                    None => false,
                };
                if !retained {
                    let browser = Browser::launch(self.executable.as_deref().ok_or("local_chromium_unavailable")?, &self.root, allow_web).await?;
                    ws.profiles.insert(profile.clone(), browser);
                    changed = true;
                }
            }
            if changed {
                if machine.is_some() {
                    if ws.active != "machine" {
                        ws.before_machine = Some(ws.active.clone());
                    }
                } else {
                    self.save_managed_profile(&profile)?;
                }
                if let Some(previous) = ws.profiles.get_mut(&ws.active) {
                    previous.observations.clear();
                    if ws.active != profile {
                        previous.layout = None;
                    }
                }
                ws.active = profile;
                ws.revision = self.next_revision();
            }
            let browser = ws.selected().ok_or("browser_not_launched")?;
            return Ok(json!({"browser_id": browser.id}));
        }
        if method == "detach_profile" {
            let p: BindingRef =
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
            let ws = workspaces
                .entry(workspace.clone())
                .or_insert_with(|| self.empty_workspace());
            if ws.binding() != p.binding_id {
                return Err("browser_binding_changed");
            }
            if !matches!(ws.active.as_str(), "chrome" | "comet" | "cdp") {
                return Err("browser_profile_not_personal");
            }
            if self.canary_epoch.is_some() {
                crate::state::forget_personal_tabs(&self.root, &ws.active)
                    .map_err(|_| "browser_profile_state_unavailable")?;
            }
            self.save_managed_profile("private")?;
            ws.profiles.remove(&ws.active);
            ws.active = "private".into();
            ws.revision = self.next_revision();
            return Ok(json!({"binding_id": ws.binding()}));
        }
        if method == "select_tab" {
            let p: SelectionRef =
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
            let ws = workspaces
                .get_mut(workspace)
                .ok_or("browser_not_launched")?;
            if ws.binding() != p.binding_id {
                return Err("browser_binding_changed");
            }
            let browser = ws
                .profiles
                .get_mut(&ws.active)
                .ok_or("browser_not_launched")?;
            if !browser.online
                || browser.id != p.browser_id
                || !browser.targets.contains(&p.target_id)
            {
                return Err("target_not_owned");
            }
            if browser.selected != p.target_id {
                if self.canary_epoch.is_some() && matches!(ws.active.as_str(), "chrome" | "comet") {
                    browser.save_personal_selection(&self.root, &ws.active, &p.target_id, false)?;
                }
                browser.selected = p.target_id;
                browser.selected_document.clear();
                browser.viewport = None;
                browser.layout = None;
                ws.revision = self.next_revision();
            }
            return Ok(json!({"binding_id": ws.binding()}));
        }
        let (browser_id, argument, compact) = match method {
            "open_tab" => {
                let p: OpenTab =
                    serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
                (p.browser_id, p.url, false)
            }
            "observe_tab" => {
                let p: ObserveTab =
                    serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
                (p.browser_id, p.target_id, p.compact)
            }
            _ => {
                let p: BrowserRef =
                    serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
                (p.browser_id, String::new(), false)
            }
        };
        let ws = workspaces
            .get_mut(workspace)
            .ok_or("browser_not_launched")?;
        let current = ws.selected().ok_or("browser_not_launched")?;
        if current.id != browser_id {
            return Err("browser_binding_changed");
        }
        if method == "close_browser" {
            if self.canary_epoch.is_some() && matches!(ws.active.as_str(), "chrome" | "comet") {
                crate::state::forget_personal_tabs(&self.root, &ws.active)
                    .map_err(|_| "browser_profile_state_unavailable")?;
            }
            self.save_managed_profile("private")?;
            let browser = ws
                .profiles
                .remove(&ws.active)
                .ok_or("browser_not_launched")?;
            ws.active = if browser.process.is_some() {
                ws.profiles
                    .iter()
                    .filter(|(_, b)| b.process.is_some())
                    .map(|(name, _)| name)
                    .min()
                    .cloned()
                    .unwrap_or_else(|| "private".into())
            } else {
                "private".into()
            };
            ws.revision = self.next_revision();
            browser.close().await?;
            return Ok(json!({"closed": true}));
        }
        let browser = ws
            .profiles
            .get_mut(&ws.active)
            .ok_or("browser_not_launched")?;
        if method == "disconnect_browser" {
            browser.transport = None;
            return Ok(json!({"browser_id": browser.id, "connected": false}));
        }
        if !browser.online {
            return Err("cdp_unavailable");
        }
        if !browser.alive()? {
            browser.transport = None;
            return Err("browser_exited");
        }
        // Cancellation drops the socket. The next read reconnects to the same
        // owned browser, with the same explicit tab membership.
        let mut cdp = match browser.transport.take() {
            Some(cdp) => cdp,
            None => browser.connect().await?,
        };
        if method == "open_tab" { ws.revision = self.next_revision(); }
        let result = browser
            .operation(&mut cdp, method, &argument, compact)
            .await;
        if result.is_ok() {
            browser.transport = Some(cdp);
        }
        if let Ok(value) = &result {
            if method == "open_tab" {
                browser.selected = field(value, "target_id")?.to_owned();
                browser.selected_document.clear();
                browser.viewport = None;
                browser.layout = None;
                ws.revision = self.next_revision();
            }
        }
        result
    }

    async fn user_tabs(&self, request: &protocol::Request) -> Result<Value> {
        let claim: Option<TabClaim> = if request.method == "claim_user_tab" {
            Some(serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?)
        } else {
            if request.params != json!({}) {
                return Err("invalid_params");
            }
            None
        };
        let mut workspaces = self.workspaces.lock().await;
        let elsewhere: BTreeSet<_> = workspaces.iter().filter(|(id,_)|*id!=&request.workspace_id).flat_map(|(_,ws)|ws.profiles.values()).flat_map(|browser|browser.targets.iter().map(move|target|(browser.port,target.clone()))).collect();
        let ws = workspaces
            .get_mut(&request.workspace_id)
            .ok_or("browser_not_launched")?;
        if !matches!(ws.active.as_str(), "chrome" | "comet" | "cdp") {
            return Err("browser_profile_not_personal");
        }
        let binding = ws.binding();
        let browser = ws
            .profiles
            .get_mut(&ws.active)
            .filter(|b| b.online)
            .ok_or("cdp_unavailable")?;
        if claim.as_ref().is_some_and(|p|elsewhere.contains(&(browser.port,p.target_id.clone()))) {return Err("browser_tab_in_use");}
        if claim
            .as_ref()
            .is_some_and(|p| p.binding_id != binding || p.browser_id != browser.id)
        {
            return Err("browser_binding_changed");
        }
        // This is discovery only until an exact candidate is explicitly claimed.
        let mut cdp = browser.connect().await?;
        let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
        let candidates = targets["targetInfos"]
            .as_array()
            .ok_or("cdp_invalid_reply")?
            .iter()
            .filter_map(personal_tab_identity)
            .filter(|(id, url, title)| {
                !browser.targets.contains(*id)
                    && !elsewhere.contains(&(browser.port,(*id).to_owned()))
                    && url.len() <= 2048
                    && title.len() <= 512
                    && *url != "about:blank"
                    && cdp.check_url(url).is_ok()
            });
        if let Some(claim) = claim {
            if !candidates.into_iter().any(|(id, url, title)| {
                id == claim.target_id && url == claim.url && title == claim.title
            }) {
                return Err("browser_tab_changed");
            }
            if browser.targets.len() >= MAX_TABS {
                return Err("tab_capacity_exceeded");
            }
            if self.canary_epoch.is_some() && matches!(ws.active.as_str(), "chrome" | "comet") {
                browser.save_personal_selection(&self.root, &ws.active, &claim.target_id, true)?;
            }
            browser.targets.insert(claim.target_id.clone());
            browser.selected = claim.target_id;
            browser.selected_document.clear();
            browser.viewport = None;
            browser.layout = None;
            ws.revision = self.next_revision();
            return Ok(json!({"binding_id": ws.binding()}));
        }
        Ok(json!({"binding_id": binding, "tabs": candidates.take(16)
            .map(|(id, url, title)| json!({"tabHandle": id, "url": url, "title": title}))
            .collect::<Vec<_>>()}))
    }

    async fn close_tab(&self, request: &protocol::Request, dispatched: &AtomicBool) -> Result<Value> {
        if self.action_backend == ActionBackend::Disabled {
            return Err("fixture_actions_unavailable");
        }
        let p: SelectionRef = serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
        let mut workspaces = self.workspaces.lock().await;
        // Popup discovery may race another chat claiming the same external
        // target. Conflicting ownership must fail closed, even if this chat's
        // retained target set also contains it.
        if let Some(port) = workspaces.get(&request.workspace_id).and_then(Workspace::selected).map(|browser|browser.port) {
            if workspaces.iter().any(|(id,other)| id != &request.workspace_id &&
                other.profiles.values().any(|browser| browser.port == port && browser.targets.contains(&p.target_id))) {
                return Err("browser_tab_in_use");
            }
        }
        let ws = workspaces.get_mut(&request.workspace_id).ok_or("browser_not_launched")?;
        if ws.binding() != p.binding_id { return Err("browser_binding_changed"); }
        let browser = ws.profiles.get_mut(&ws.active).ok_or("browser_not_launched")?;
        if browser.id != p.browser_id { return Err("browser_binding_changed"); }
        if !browser.targets.contains(&p.target_id) { return Err("target_not_owned"); }
        if browser.fixed_target.is_some() { return Err("browser_embedded_single_tab"); }
        if !browser.online { return Err("cdp_unavailable"); }
        let journal = ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        journal.authorize(request).await?;
        journal.check_navigation(request, &p.browser_id, &p.target_id).await?;
        let mut cdp = browser.connect().await?;
        // A close does not require a functioning renderer, only exact owned target identity.
        browser.refresh(&mut cdp).await?;
        if !browser.targets.contains(&p.target_id) { return Err("target_not_owned"); }
        journal.authorize(request).await?;
        if request.deadline_ms <= protocol::now_ms() { return Err("deadline_exceeded"); }
        dispatched.store(true, Ordering::Relaxed);
        let closed = cdp.call(None, "Target.closeTarget", json!({"targetId":p.target_id})).await?;
        if closed["success"] != true { return Err("tab_close_unconfirmed"); }
        loop {
            let current = cdp.call(None, "Target.getTargets", json!({})).await?;
            if current["targetInfos"].as_array().is_some_and(|tabs| !tabs.iter().any(|t| t["targetId"] == p.target_id)) { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        browser.targets.remove(&p.target_id);
        browser.observations.remove(&p.target_id);
        browser.uploads.retain(|id| id != p.target_id);
        browser.unreadable.remove(&p.target_id);
        if browser.selected == p.target_id || browser.selected.is_empty() {
            browser.selected = browser.targets.first().cloned().unwrap_or_default();
            browser.selected_document.clear();
            browser.viewport = None;
            browser.layout = None;
            browser.download_view = None;
        }
        if self.canary_epoch.is_some() && matches!(ws.active.as_str(), "chrome" | "comet") {
            let selected = browser.selected.clone();
            browser.save_personal_selection(&self.root, &ws.active, &selected, false)?;
        }
        ws.revision = self.next_revision();
        Ok(json!({"closed":true,"browser_id":p.browser_id,"target_id":p.target_id,
            "selected_target_id":ws.selected().and_then(|b| (!b.selected.is_empty()).then_some(&b.selected)),
            "tab_count":ws.selected().map_or(0, |b| b.targets.len()),"tab_limit":MAX_TABS}))
    }

    async fn submission_confirmation(&self,request:&protocol::Request)->Result<Value> {
        let p:crate::confirmation::Query=serde_json::from_value(request.params.clone()).map_err(|_|"invalid_params")?;
        if request.method=="confirm_submission" && !cfg!(feature="submission-confirmation") {return Err("confirmation_disabled");}
        if !matches!(self.action_backend,ActionBackend::Standalone|ActionBackend::Fixture) {return Err("confirmation_backend_unsupported");}
        let workspaces=self.workspaces.lock().await;
        let ws=workspaces.get(&request.workspace_id).ok_or("browser_not_launched")?;
        let browser=ws.selected().filter(|b|b.online).ok_or("browser_not_connected")?;
        if ws.binding()!=p.binding_id || browser.id!=p.browser_id {return Err("browser_binding_changed");}
        if !browser.targets.contains(&p.target_id) {return Err("target_not_owned");}
        let original=if request.method=="confirm_submission" || p.request_id.is_some() {
            let id=p.request_id.as_deref().filter(|id|!id.is_empty()&&id.len()<=128).ok_or("invalid_params")?;
            Some(action_journal::Journal::lock(&self.root)?.receipt(&request.workspace_id,id)?)
        } else {None};
        let anchor=original.as_ref().and_then(|r|r.submission_context.as_ref());
        let mut cdp=browser.connect().await?;
        let generic=request.method=="submission_context" && p.allow_generic;
        let read=crate::confirmation::inspect(&mut cdp,&p.target_id,&ws.active,&browser.path,self.action_backend==ActionBackend::Fixture,generic,anchor).await;
        let (context,affirmative)=match read {
            Err("document_changed"|"cdp_invalid_reply"|"cdp_context_unavailable")=>{
                tokio::time::sleep(Duration::from_millis(50)).await;
                crate::confirmation::inspect(&mut cdp,&p.target_id,&ws.active,&browser.path,self.action_backend==ActionBackend::Fixture,generic,anchor).await?
            },
            other=>other?,
        };
        if request.method=="submission_context" {return Ok(json!({"context":context,"confirmation":affirmative.map(|(kind,text)|json!({"kind":kind,"text":text})),"input_sent":false}));}
        let id=p.request_id.as_deref().filter(|id|!id.is_empty()&&id.len()<=128).ok_or("invalid_params")?;
        let mut journal=action_journal::Journal::lock(&self.root)?;
        let original=journal.receipt(&request.workspace_id,id)?;
        // A previously committed confirmation is immutable historical evidence.
        let (kind,text)=if original.resolution.is_some(){(String::new(),String::new())}else{affirmative.ok_or("postcondition_unobserved")?};
        let proof=journal.confirm(request,id,context,kind,text,p.legacy_source.as_ref())?;
        Ok(json!({"receipt":proof,"original_receipt":journal.receipt(&request.workspace_id,id)?,"verification":"independent_site_confirmation","input_sent":false}))
    }

    async fn query_tab(&self, request: &protocol::Request) -> Result<Value> {
        let query: TabQuery =
            serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
        if query.selector.trim().is_empty()
            || query.selector.len() > 512
            || query.offset > 100_000
            || query.limit.is_some_and(|limit| !(1..=12).contains(&limit))
            || query.text.as_ref().is_some_and(|text| text.len() > 512)
            || query.wait_for.as_deref().is_some_and(|state| {
                !matches!(
                    state,
                    "visible" | "hidden" | "attached" | "detached" | "url"
                )
            })
            || (query.wait_for.as_deref() == Some("url")) != query.url.is_some()
            || query.url.as_ref().is_some_and(|url| {
                url.is_empty() || url.len() > 8192 || query.frame_id.is_some() || query.console
            })
            || query.value_probe.as_ref().is_some_and(|probe| {
                !probe.valid() || query.text.is_some() || query.wait_for.is_some() || query.console
            })
        {
            return Err("invalid_params");
        }
        let target = &query.selection;
        let (port, path, document, cached) = {
            let workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get(&request.workspace_id)
                .ok_or("browser_not_launched")?;
            let b = ws.selected().ok_or("browser_not_launched")?;
            if ws.binding() != target.binding_id
                || !b.online
                || b.id != target.browser_id
                || !b.targets.contains(&target.target_id)
            {
                return Err("browser_binding_changed");
            }
            (
                b.port,
                b.path.clone(),
                (b.selected == target.target_id).then(|| b.selected_document.clone()),
                b.observations
                    .get(&target.target_id)
                    .map(|(_, value)| value.clone()),
            )
        };
        let mut cdp = Cdp::connect(port, &path, true).await?;
        let session = cdp.attach(&target.target_id).await?;
        if let Some(expected) = query.url {
            // The document may change by design. Keep polling the exact tab
            // and reject profile or ownership changes before using a match.
            loop {
                let live = cdp
                    .call(Some(&session), "Page.getFrameTree", json!({}))
                    .await?;
                let url = actions::frame_url(&live["frameTree"]["frame"])?;
                let valid = self
                    .workspaces
                    .lock()
                    .await
                    .get(&request.workspace_id)
                    .is_some_and(|ws| {
                        ws.binding() == target.binding_id
                            && ws.selected().is_some_and(|browser| {
                                browser.online
                                    && browser.id == target.browser_id
                                    && browser.targets.contains(&target.target_id)
                            })
                    });
                if !valid {
                    return Err("browser_binding_changed");
                }
                if url == expected {
                    return Ok(json!({"tab_handle": target.target_id, "url": url}));
                }
                tokio::time::sleep(Duration::from_millis(75)).await;
            }
        }
        let (frame, loader) = main_frame(&mut cdp, &session).await?;
        cdp.detach(&session).await?;
        if document.is_some_and(|document| loader != document) {
            return Err("browser_binding_changed");
        }
        let snapshot = if query.value_probe.is_some()
            || query.frame_id.as_deref().is_some_and(|id| id != frame)
        {
            cached.ok_or("frame_scope_unavailable")?
        } else {
            json!({"main_frame_id": frame, "frames": {(frame.clone()): loader}})
        };
        let page = actions::Page::from_scope(
            &mut cdp,
            &target.target_id,
            &snapshot,
            query.frame_id.as_deref(),
        )
        .await?;
        if let Some(probe) = &query.value_probe {
            let result =
                crate::input_evidence::probe(&mut cdp, &page, &snapshot, &query.selector, probe)
                    .await?;
            let valid = self
                .workspaces
                .lock()
                .await
                .get(&request.workspace_id)
                .is_some_and(|ws| ws.binding() == target.binding_id);
            page.detach(&mut cdp).await?;
            if !valid {
                return Err("browser_binding_changed");
            }
            return Ok(json!({"tab_handle":target.target_id,"value_probe":result}));
        }
        if query.console {
            let messages = cdp.console_messages(&page.session).await?;
            page.check_document(&mut cdp).await?;
            if self
                .workspaces
                .lock()
                .await
                .get(&request.workspace_id)
                .is_none_or(|ws| ws.binding() != target.binding_id)
            {
                return Err("browser_binding_changed");
            }
            return Ok(json!({"tab_handle": target.target_id, "messages": messages}));
        }
        loop {
            page.check_document(&mut cdp).await?;
            let mut result = page
                .inspect(&mut cdp, &query.selector, query.text.as_deref(), query.offset, query.limit.unwrap_or(12))
                .await?;
            page.check_document(&mut cdp).await?;
            if self
                .workspaces
                .lock()
                .await
                .get(&request.workspace_id)
                .is_none_or(|ws| ws.binding() != target.binding_id)
            {
                return Err("browser_binding_changed");
            }
            let count = result["count"].as_u64().ok_or("cdp_invalid_reply")?;
            let visible = result["visible"].as_bool().ok_or("cdp_invalid_reply")?;
            let matched = match query.wait_for.as_deref() {
                Some("visible") => visible,
                Some("hidden") => !visible,
                Some("attached") => count > 0,
                Some("detached") => count == 0,
                _ => true,
            };
            if matched {
                let mut observation = snapshot.clone();
                observation["browser_id"] = json!(target.browser_id);
                observation["target_id"] = json!(target.target_id);
                observation["document_id"] = json!(page.document.clone());
                observation["tab_document_id"] = json!(page.tab_document.clone());
                observation["observation_id"] = json!(Uuid::new_v4().simple().to_string());
                if query.frame_id.as_deref().is_none_or(|frame| frame == snapshot["main_frame_id"]) {
                    observation["url"] = json!(actions::frame_url(&page.current_frame(&mut cdp).await?)?);
                }
                observation["dom"] = result.as_object_mut().unwrap().remove("observed_dom").unwrap_or(json!([]));
                observation["ax"] = result.as_object_mut().unwrap().remove("observed_ax").unwrap_or(json!([]));
                page.check_document(&mut cdp).await?;
                let mut workspaces = self.workspaces.lock().await;
                let ws = workspaces.get_mut(&request.workspace_id).ok_or("browser_binding_changed")?;
                if ws.binding() != target.binding_id { return Err("browser_binding_changed"); }
                let browser = ws.profiles.get_mut(&ws.active).ok_or("browser_binding_changed")?;
                browser.observations.insert(target.target_id.clone(), (Instant::now(), observation.clone()));
                let mut expanded = observe::expanded(observation);
                if let Some(controls) = expanded["controls"].as_array_mut() {
                    for control in controls {
                        control["ref"] = json!(format!("backend={}@{}", control["backend_node_id"], control["document_id"].as_str().unwrap_or("")));
                    }
                }
                return Ok(json!({"tab_handle": target.target_id, "matches": result,
                    "observation_id":expanded["observation_id"],"observation":expanded}));
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    }

    async fn action(&self, request: &protocol::Request, dispatched: &AtomicBool) -> Result<Value> {
        let workspace = &request.workspace_id;
        if request.method == "pending_actions" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Scope {
                browser_id: String,
                target_id: String,
            }
            let p: Scope =
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
            let journal =
                ActionJournal::open(&self.root, self.action_backend, request, false).await?;
            return Ok(json!({"receipts":journal.pending(request,&p.browser_id,&p.target_id)?}));
        }
        if request.method == "batch_status" {
            let params: actions::ReceiptQuery =
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
            let _workspaces = self.workspaces.lock().await;
            return crate::batch::status(
                &self.root,
                self.action_backend,
                request,
                &params.request_id,
            )
            .await;
        }
        if request.method == "action_receipt" {
            let params: actions::ReceiptQuery =
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
            let journal =
                ActionJournal::open(&self.root, self.action_backend, request, false).await?;
            return Ok(json!({"receipt": journal.receipt(workspace, &params.request_id).await?}));
        }
        enum Operation {
            Click(actions::Click),
            Navigate(actions::Navigate),
            Input(actions::Input),
            Batch(crate::batch::Batch),
            Reconcile(actions::Reconcile),
        }
        let operation = match request.method.as_str() {
            "click_observed" => Operation::Click(
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?,
            ),
            "navigate_tab" => Operation::Navigate(
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?,
            ),
            "fill_observed" | "press_observed" | "select_observed" | "check_observed" => {
                Operation::Input(
                    serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?,
                )
            }
            "batch_observed" => Operation::Batch(
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?,
            ),
            _ => Operation::Reconcile(
                serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?,
            ),
        };
        let (binding, target, observation) = match &operation {
            Operation::Click(p) => (&p.browser_id, &p.target_id, &p.observation_id),
            Operation::Navigate(p) => (&p.browser_id, &p.target_id, &p.observation_id),
            Operation::Input(p) => (&p.browser_id, &p.target_id, &p.observation_id),
            Operation::Batch(p) => (&p.browser_id, &p.target_id, &p.observation_id),
            Operation::Reconcile(p) => (&p.browser_id, &p.target_id, &p.observation_id),
        };
        let mut workspaces = self.workspaces.lock().await;
        let browser = workspaces
            .get_mut(workspace)
            .and_then(|ws| ws.profiles.get_mut(&ws.active))
            .ok_or("browser_not_launched")?;
        if &browser.id != binding {
            return Err("browser_binding_changed");
        }
        if !browser.targets.contains(target) {
            return Err("target_not_owned");
        }
        if !browser.online {
            return Err("cdp_unavailable");
        }
        if !browser.alive()? {
            return Err("browser_exited");
        }
        let (observed_at, snapshot) = browser
            .observations
            .get(target)
            .filter(|(_, snapshot)| snapshot["observation_id"].as_str() == Some(observation))
            .ok_or("observation_stale")?;
        if observed_at.elapsed() >= Duration::from_secs(30) {
            return Err("observation_expired");
        }
        let mut cdp = match browser.transport.take() {
            Some(cdp) => cdp,
            None => browser.connect().await?,
        };
        let result = match operation {
            Operation::Click(params) => {
                // Consume observations before starting any potentially dispatching call.
                let (_, snapshot) = browser
                    .observations
                    .remove(&params.target_id)
                    .ok_or("observation_stale")?;
                // Keep the binding locked through input. The outcome check uses
                // its own CDP connection so the preview can read while it waits.
                return actions::click(
                    &mut cdp,
                    &self.root,
                    self.action_backend,
                    request,
                    params,
                    &snapshot,
                    dispatched,
                    || drop(workspaces),
                )
                .await;
            }
            Operation::Navigate(params) => {
                let (_, snapshot) = browser
                    .observations
                    .remove(&params.target_id)
                    .ok_or("observation_stale")?;
                actions::navigate(
                    &mut cdp,
                    &self.root,
                    self.action_backend,
                    request,
                    params,
                    &snapshot,
                    dispatched,
                )
                .await
            }
            Operation::Input(params) => {
                let (_, snapshot) = browser
                    .observations
                    .remove(&params.target_id)
                    .ok_or("observation_stale")?;
                actions::input(
                    &mut cdp,
                    &self.root,
                    self.action_backend,
                    request,
                    params,
                    &snapshot,
                    dispatched,
                )
                .await
            }
            Operation::Batch(params) => {
                let (_, snapshot) = browser
                    .observations
                    .remove(&params.target_id)
                    .ok_or("observation_stale")?;
                crate::batch::run(
                    &mut cdp,
                    &self.root,
                    self.action_backend,
                    request,
                    params,
                    snapshot,
                    dispatched,
                )
                .await
            }
            Operation::Reconcile(params) => {
                actions::reconcile(
                    &mut cdp,
                    &self.root,
                    self.action_backend,
                    request,
                    params,
                    snapshot,
                )
                .await
            }
        };
        if result
            .as_ref()
            .is_ok_and(|result| result["status"] != "stopped")
        {
            browser.transport = Some(cdp);
        }
        result
    }

    async fn selection(&self, workspace: &str) -> Result<Value> {
        let mut workspaces = self.workspaces.lock().await;
        let Some(ws) = workspaces.get_mut(workspace) else {
            return Ok(self.empty_workspace().disconnected());
        };
        if let Some((active, view)) = ws
            .selected()
            .and_then(|browser| browser.download_view.as_ref())
        {
            if active.strong_count() > 0 && view["binding_id"] == ws.binding() {
                return Ok(view.clone());
            }
        }
        let Some(browser) = ws.profiles.get_mut(&ws.active) else {
            return Ok(ws.disconnected());
        };
        browser.download_view = None;
        if !browser.alive()? {
            ws.profiles.remove(&ws.active);
            ws.revision = self.next_revision();
            return Ok(ws.disconnected());
        }
        let result = if browser.process.is_some() {
            browser.read_selection().await
        } else {
            tokio::time::timeout(PROFILE_PROBE_TIMEOUT, browser.read_selection())
                .await
                .unwrap_or(Err("cdp_unavailable"))
        };
        let (result, target, document, viewport) = match result {
            Ok(result) => result,
            Err(error) if browser.process.is_some() => return Err(error),
            Err(error) => {
                if browser.online {
                    // Name the cause: "disconnected" alone cannot be diagnosed afterwards.
                    eprintln!("browser profile offline profile={} cause={error}", ws.active);
                    browser.online = false;
                    browser.transport = None;
                    browser.layout = None;
                    browser.observations.clear();
                    ws.revision = self.next_revision();
                }
                return Ok(ws.disconnected());
            }
        };
        if !browser.online
            || browser.selected != target
            || browser.selected_document != document
            || browser.viewport != viewport
        {
            if browser.selected != target {
                browser.layout = None;
            }
            browser.online = true;
            browser.selected = target.clone();
            browser.selected_document = document;
            browser.viewport = viewport;
            ws.revision = self.next_revision();
        }
        let layout = browser.layout.as_ref().map(|layout| layout.size);
        let document_id = browser.selected_document.clone();
        if self.canary_epoch.is_some() && matches!(ws.active.as_str(), "chrome" | "comet") {
            browser.save_personal_selection(&self.root, &ws.active, &target, false)?;
        }
        Ok(
            json!({"binding_id": ws.binding(), "revision": ws.revision, "profile": ws.active,
            "browser_id": result["browser_id"], "target_id": if target.is_empty() { None } else { Some(target) },
            "tabs": result["tabs"], "profiles": ws.profiles(), "viewport": viewport, "document_id": document_id,
            "layout": layout}),
        )
    }

    pub fn scripts_available(&self) -> bool {
        cfg!(any(
            target_os = "macos",
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            )
        )) && matches!(
            self.action_backend,
            ActionBackend::Application | ActionBackend::Standalone
        )
    }

    pub async fn script_start(
        &self,
        request: &protocol::Request,
        browser_id: &str,
        binding: &str,
        target: Option<&str>,
    ) -> Result<()> {
        let workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get(&request.workspace_id)
            .ok_or("browser_not_launched")?;
        let browser = ws.selected().ok_or("browser_not_launched")?;
        if ws.binding() != binding
            || browser.id != browser_id
            || browser.selected != target.unwrap_or("")
            || !browser.online
        {
            return Err("browser_binding_changed");
        }
        let journal = ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        journal.authorize(request).await
    }

    pub async fn script_artifact(
        &self,
        request: &protocol::Request,
        browser_id: &str,
        target: &str,
        name: &str,
        content: script_artifact::Content,
        dispatched: &AtomicBool,
    ) -> Result<script_artifact::Artifact> {
        use script_artifact::Content;
        let mime_type = script_artifact::validate_name(name, &content)?;
        {
            let workspaces = self.workspaces.lock().await;
            let browser = workspaces
                .get(&request.workspace_id)
                .and_then(Workspace::selected)
                .ok_or("browser_not_launched")?;
            if browser.id != browser_id || browser.selected != target || !browser.online {
                return Err("browser_binding_changed");
            }
        }
        let journal = ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        journal.authorize(request).await?;
        let application = journal.script_artifact_workspace().await?;
        let bytes = match content {
            Content::Text { text } => text.into_bytes(),
            Content::Json { data } => {
                let mut bytes = serde_json::to_vec(&data).map_err(|_| "invalid_script_artifact")?;
                bytes.push(b'\n');
                bytes
            }
            Content::Screenshot => {
                let selection = self.selection(&request.workspace_id).await?;
                if target.is_empty() {
                    return Err("target_not_owned");
                }
                if selection["browser_id"] != browser_id || selection["target_id"] != target {
                    return Err("browser_binding_changed");
                }
                let mut capture = request.clone();
                capture.params = json!({
                    "binding_id": selection["binding_id"], "browser_id": browser_id, "target_id": target,
                });
                self.capture_preview(&capture).await?.1
            }
            Content::Download => return Err("invalid_script_artifact"),
        };
        if mime_type != "image/jpeg" && bytes.len() > script_artifact::MAX_TEXT_BYTES {
            return Err("script_artifact_too_large");
        }
        journal.authorize(request).await?;
        if request.deadline_ms <= protocol::now_ms() {
            return Err("deadline_exceeded");
        }
        let artifact = script_artifact::write(
            &self.root,
            application.as_ref(),
            &request.workspace_id,
            &request.request_id,
            name,
            mime_type,
            &bytes,
        )?;
        dispatched.store(true, Ordering::Relaxed);
        Ok(artifact)
    }

    pub async fn script_download(
        &self,
        request: &protocol::Request,
        run_id: &str,
        browser_id: &str,
        target: &str,
        locator: &str,
        name: &str,
        expected_url: Option<&str>,
        dispatched: &AtomicBool,
        stop: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<script_artifact::Artifact> {
        let mime = script_artifact::validate_name(name, &script_artifact::Content::Download)?;
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get_mut(&request.workspace_id)
            .ok_or("browser_not_launched")?;
        let browser = ws
            .profiles
            .get_mut(&ws.active)
            .ok_or("browser_not_launched")?;
        if browser.id != browser_id || browser.selected != target || !browser.online {
            return Err("browser_binding_changed");
        }
        let machine = MachineAdapter::load(&self.root, &request.workspace_id);
        if browser.process.is_none() && machine.is_none() {
            return Err("script_download_requires_private");
        }
        let mut cdp = browser.connect().await?;
        let targets = browser.refresh(&mut cdp).await?;
        if !browser.targets.contains(target) {
            return Err("target_not_owned");
        }
        let snapshot = observe::capture(&mut cdp, browser_id, target, &targets).await?;
        let mut journal =
            ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        let application = journal.script_artifact_workspace().await?;
        let destination = script_artifact::prepare(
            &self.root,
            application.as_ref(),
            &request.workspace_id,
            run_id,
            name,
        )?;
        let mut download = crate::download::Download::new(&self.root, request, machine.as_ref().map(|adapter| &adapter.transfer))?;
        if *stop.borrow() {
            return Err("script_cancelled");
        }
        browser.observations.remove(target);
        let started = download
            .start(
                &mut cdp,
                &mut journal,
                request,
                browser_id,
                target,
                locator,
                expected_url,
                &snapshot,
                dispatched,
                stop,
            )
            .await;
        if started.is_ok() {
            let tabs = browser.owned_tabs(&targets);
            let viewport = browser.viewport;
            let layout = browser.layout.as_ref().map(|layout| layout.size);
            let view = json!({
                "binding_id": ws.binding(), "revision": ws.revision, "profile": ws.active,
                "browser_id": browser_id, "target_id": target, "tabs": tabs,
                "profiles": ws.profiles(), "viewport": viewport, "layout": layout,
            });
            if let Some(browser) = ws.profiles.get_mut(&ws.active) {
                browser.download_view = Some((download.view(), view));
            }
        }
        drop(workspaces);
        let bytes = match started {
            Ok(trigger) => {
                download
                    .finish(&mut cdp, &mut journal, request, &trigger, stop)
                    .await
            }
            Err(error) => Err(error),
        };
        download.cleanup(&mut cdp, bytes.is_err()).await;
        let bytes = bytes?;
        journal.authorize(request).await?;
        if *stop.borrow() || request.deadline_ms <= protocol::now_ms() {
            return Err(if *stop.borrow() {
                "script_cancelled"
            } else {
                "script_download_timed_out"
            });
        }
        destination.publish(mime, &bytes)
    }

    pub async fn script_upload(
        &self,
        request: &protocol::Request,
        browser_id: &str,
        target: &str,
        locator: &str,
        paths: Vec<String>,
        approval: crate::upload::Approval,
        dispatched: &AtomicBool,
        stop: &tokio::sync::watch::Receiver<bool>,
    ) -> Result<Value> {
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get_mut(&request.workspace_id)
            .ok_or("browser_not_launched")?;
        let machine = MachineAdapter::load(&self.root, &request.workspace_id);
        if hosted_profile(&ws.active) && machine.is_none() {
            return Err("script_upload_adapter_unavailable");
        }
        let browser = ws
            .profiles
            .get_mut(&ws.active)
            .ok_or("browser_not_launched")?;
        if browser.id != browser_id || browser.selected != target || !browser.online {
            return Err("browser_binding_changed");
        }
        let mut cdp = browser.connect().await?;
        let targets = browser.refresh(&mut cdp).await?;
        if !browser.targets.contains(target) {
            return Err("target_not_owned");
        }
        let snapshot = observe::capture(&mut cdp, browser_id, target, &targets).await?;
        let mut journal =
            ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        browser.observations.remove(target);
        let mut result = browser
            .uploads
            .attach(
                &mut cdp,
                &mut journal,
                request,
                &self.root,
                machine.as_ref().map(|adapter| &adapter.transfer),
                browser_id,
                target,
                locator,
                paths,
                approval,
                &snapshot,
                dispatched,
                stop,
            )
            .await?;
        result["tab_handle"] = json!(target);
        Ok(result)
    }

    pub async fn script_step(
        &self,
        request: &protocol::Request,
        browser_id: &str,
        target: &mut String,
        operation: script::Operation,
        dispatched: &AtomicBool,
    ) -> Result<Value> {
        use script::Operation;
        let operation = match operation {
            Operation::FillCredential { target, credential } => Operation::Batch {
                actions: vec![
                    json!({"action":"fill_credential","target":target,"credential":credential}),
                ],
            },
            other => other,
        };
        let mut workspaces = self.workspaces.lock().await;
        let elsewhere: BTreeSet<_> = workspaces.iter().filter(|(id,_)|*id!=&request.workspace_id).flat_map(|(_,ws)|ws.profiles.values()).flat_map(|browser|browser.targets.iter().map(move|target|(browser.port,target.clone()))).collect();
        let ws = workspaces
            .get_mut(&request.workspace_id)
            .ok_or("browser_not_launched")?;
        let browser = ws
            .profiles
            .get_mut(&ws.active)
            .ok_or("browser_not_launched")?;
        if browser.id != browser_id || browser.selected != *target || !browser.online {
            return Err("browser_binding_changed");
        }
        let mut cdp = browser.connect().await?;
        if let Operation::Credential { account, mode, credential } = &operation {
            let journal =
                ActionJournal::open(&self.root, self.action_backend, request, true).await?;
            journal.authorize(request).await?;
            let targets = browser.refresh(&mut cdp).await?;
            if !browser.targets.contains(target) {
                return Err("target_not_owned");
            }
            let snapshot = observe::capture(&mut cdp, browser_id, target, &targets).await?;
            let origin =
                crate::credentials::origin(snapshot["url"].as_str().ok_or("cdp_invalid_reply")?)?;
            let vault = crate::credentials::Vault::configured()?;
            let result = match mode.as_deref().unwrap_or("get") {
                "get" if credential.is_none() => vault.generate(&request.workspace_id, &origin, account)?,
                "stage_rotation" if credential.is_none() => vault.stage_rotation(&request.workspace_id, &origin, account)?,
                "commit_rotation" => vault.commit_rotation(&request.workspace_id, &origin, account, credential.as_deref().ok_or("credential_reference_invalid")?)?,
                _ => return Err("credential_operation_invalid"),
            };
            let mut response = json!({"credential":result,"origin":origin,"stored":true,"operation":mode.as_deref().unwrap_or("get")});
            if mode.as_deref() == Some("commit_rotation") {
                // commit_rotation has durably written the candidate and checked that it
                // is still active. This attests only that local vault operation, not the
                // site's password reset or a future credential state.
                response["commit_receipt"] = json!({"state":"verified","operation":"commit_rotation",
                    "workspace_id":request.workspace_id,"origin":origin,"account":account,
                    "candidate":credential,"active":result});
            }
            return Ok(response);
        }
        if let Operation::Batch { actions } = operation {
            let batch: crate::batch::Batch = serde_json::from_value(json!({
                "browser_id": browser_id, "target_id": target, "observation_id": "", "actions": actions,
            })).map_err(|_| "invalid_batch_actions")?;
            let targets = browser.refresh(&mut cdp).await?;
            if !browser.targets.contains(target) {
                return Err("target_not_owned");
            }
            let snapshot = observe::capture(&mut cdp, browser_id, target, &targets).await?;
            browser.observations.remove(target);
            return crate::batch::run(
                &mut cdp,
                &self.root,
                self.action_backend,
                request,
                batch,
                snapshot,
                dispatched,
            )
            .await;
        }
        if let Operation::Navigate { url } = operation {
            let navigation: actions::Navigate = serde_json::from_value(json!({
                "browser_id": browser_id, "target_id": target, "observation_id": "", "url": url,
            }))
            .map_err(|_| "invalid_params")?;
            let targets = browser.refresh(&mut cdp).await?;
            if !browser.targets.contains(target) {
                return Err("target_not_owned");
            }
            let snapshot = observe::capture(&mut cdp, browser_id, target, &targets).await?;
            browser.observations.remove(target);
            return actions::navigate(
                &mut cdp,
                &self.root,
                self.action_backend,
                request,
                navigation,
                &snapshot,
                dispatched,
            )
            .await;
        }
        let journal = ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        journal.authorize(request).await?;
        let next = match operation {
            Operation::Open { url } => {
                cdp.check_url(&url)?;
                journal
                    .check_navigation(request, browser_id, target)
                    .await?;
                dispatched.store(true, Ordering::Relaxed);
                ws.revision = self.next_revision();
                let tab = browser
                    .operation(&mut cdp, "open_tab", &url, false)
                    .await
                    .map_err(|_| "script_open_unconfirmed")?;
                field(&tab, "target_id")?.to_owned()
            }
            Operation::Select { tab_handle } => {
                browser.refresh(&mut cdp).await?;
                if !browser.targets.contains(&tab_handle) {
                    return Err("target_not_owned");
                }
                tab_handle
            }
            Operation::AttachTab {
                tab_handle,
                url,
                title,
            } => {
                if elsewhere.contains(&(browser.port,tab_handle.clone())) {return Err("browser_tab_in_use");}
                if !matches!(ws.active.as_str(), "chrome" | "comet" | "cdp") {
                    return Err("browser_profile_not_personal");
                }
                cdp.check_url(&url)?;
                if url == "about:blank" || title.len() > 512 {
                    return Err("browser_tab_changed");
                }
                let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
                if !targets["targetInfos"].as_array().is_some_and(|tabs| {
                    tabs.iter().any(|tab| {
                        personal_tab_identity(tab)
                            == Some((tab_handle.as_str(), url.as_str(), title.as_str()))
                    })
                }) {
                    return Err("browser_tab_changed");
                }
                let new_tab = !browser.targets.contains(&tab_handle);
                if new_tab && browser.targets.len() >= MAX_TABS {
                    return Err("tab_capacity_exceeded");
                }
                journal.authorize(request).await?;
                if self.canary_epoch.is_some() && matches!(ws.active.as_str(), "chrome" | "comet") {
                    browser.save_personal_selection(
                        &self.root,
                        &ws.active,
                        &tab_handle,
                        new_tab,
                    )?;
                }
                browser.targets.insert(tab_handle.clone());
                tab_handle
            }
            Operation::Tabs => {
                let mut result = browser.operation(&mut cdp, "list_tabs", "", false).await?;
                result["selected"] = json!(target);
                return Ok(result);
            }
            Operation::Snapshot => {
                let mut result = browser
                    .operation(&mut cdp, "observe_tab", target, true)
                    .await?;
                result["tab_handle"] = json!(target);
                return Ok(result);
            }
            _ => return Err("invalid_script_operation"),
        };
        if *target != next {
            browser.selected.clone_from(&next);
            browser.selected_document.clear();
            browser.viewport = None;
            browser.layout = None;
            ws.revision = self.next_revision();
            *target = next;
        }
        Ok(json!({"tab_handle": target, "binding_id": ws.binding()}))
    }

    pub async fn script_preview_ready(
        &self,
        request: &protocol::Request,
        browser_id: &str,
        target_id: &str,
    ) -> Result<Value> {
        let journal = ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        journal.authorize(request).await?;
        let selection = self.selection(&request.workspace_id).await?;
        if selection["browser_id"] != browser_id || selection["target_id"] != target_id {
            return Err("browser_binding_changed");
        }
        let mut preview = request.clone();
        preview.method = "preview_tab".into();
        preview.params = json!({
            "binding_id": selection["binding_id"], "browser_id": browser_id, "target_id": target_id,
        });
        let (frame, _) = self.preview(&preview).await?;
        Ok(json!({
            "tab_handle": target_id, "binding_id": selection["binding_id"],
            "profile": selection["profile"], "preview_frame_available": true,
            "width": frame["width"], "height": frame["height"],
        }))
    }

    async fn panel_action(
        &self,
        request: &protocol::Request,
        dispatched: &AtomicBool,
    ) -> Result<Value> {
        if !matches!(
            self.action_backend,
            ActionBackend::Standalone | ActionBackend::Application
        ) {
            return Err("panel_input_unavailable");
        }
        let p: PanelRequest =
            serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
        let mut workspaces = self.workspaces.lock().await;
        let ws = workspaces
            .get_mut(&request.workspace_id)
            .ok_or("browser_not_launched")?;
        if ws.binding() != p.selection.binding_id {
            return Err("browser_binding_changed");
        }
        let browser = ws
            .profiles
            .get_mut(&ws.active)
            .ok_or("browser_not_launched")?;
        if !browser.online
            || browser.id != p.selection.browser_id
            || browser.selected != p.selection.target_id
        {
            return Err("browser_binding_changed");
        }
        let journal = ActionJournal::open(&self.root, self.action_backend, request, true).await?;
        let mut cdp = browser.connect().await?;
        let open_url = match &p.input {
            panel::Input::TabNew => Some("about:blank"),
            panel::Input::Navigate { url } if browser.selected.is_empty() => Some(url.as_str()),
            _ => None,
        };
        if let Some(url) = open_url {
            cdp.check_url(url)?;
            if !browser.selected.is_empty() {
                let session = cdp.attach(&browser.selected).await?;
                if main_frame(&mut cdp, &session).await?.1 != browser.selected_document {
                    return Err("browser_binding_changed");
                }
            }
            journal.authorize(request).await?;
            dispatched.store(true, Ordering::Relaxed);
            ws.revision = self.next_revision();
            let result = browser.operation(&mut cdp, "open_tab", url, false).await?;
            browser.selected = field(&result, "target_id")?.to_owned();
            browser.selected_document.clear();
            browser.viewport = None;
            browser.layout = None;
            browser.observations.clear();
            ws.revision = self.next_revision();
            return Ok(result);
        }
        if !browser.targets.contains(&browser.selected) {
            return Err("target_not_owned");
        }
        let session = cdp.attach(&browser.selected).await?;
        let (frame_id, _) = main_frame(&mut cdp, &session).await?;
        let viewport = panel::Viewport::read(&mut cdp, &session, &frame_id).await?;
        if Some(viewport.size) != p.viewport
            || Some(viewport.size) != browser.viewport
            || main_frame(&mut cdp, &session).await?.1 != browser.selected_document
        {
            return Err("browser_binding_changed");
        }
        journal.authorize(request).await?;
        if let panel::Input::Layout { size } = &p.input {
            // A responsive panel may acknowledge the same size after a binding
            // refresh. A no-op must not invalidate observations or bump binding.
            if browser.layout.as_ref().map(|layout| layout.size) == *size {
                return Ok(json!({"binding_id":ws.binding()}));
            }
        }
        if matches!(&p.input, panel::Input::Layout { .. }) {
            // Locator actions check the live document, element, and layout.
            // A coordinate gesture still needs a fresh viewport observation.
            if let Some((_, snapshot)) = browser.observations.get_mut(&browser.selected) {
                snapshot["viewport_changed"] = json!(true);
            }
        } else {
            browser.observations.remove(&browser.selected);
        }
        match p.input {
            panel::Input::Layout { size } => {
                browser
                    .set_panel_layout(cdp, session, size, dispatched)
                    .await?;
                ws.revision = self.next_revision();
            }
            panel::Input::TabClose => {
                dispatched.store(true, Ordering::Relaxed);
                let result = cdp
                    .call(
                        None,
                        "Target.closeTarget",
                        json!({"targetId": browser.selected}),
                    )
                    .await?;
                if result["success"] != true {
                    return Err("panel_close_failed");
                }
                browser.targets.remove(&browser.selected);
                browser.uploads.retain(|target| target != browser.selected);
                browser.selected = browser.targets.first().cloned().unwrap_or_default();
                browser.selected_document.clear();
                browser.viewport = None;
                browser.layout = None;
                ws.revision = self.next_revision();
            }
            input => {
                input
                    .dispatch(&mut cdp, &session, &viewport, dispatched, None)
                    .await?
            }
        }
        Ok(json!({"binding_id": ws.binding()}))
    }

    pub async fn shutdown(&self) {
        let browsers = std::mem::take(&mut *self.workspaces.lock().await);
        let mut tasks = tokio::task::JoinSet::new();
        for browser in browsers
            .into_values()
            .flat_map(|workspace| workspace.profiles.into_values())
        {
            tasks.spawn(browser.close());
        }
        while tasks.join_next().await.is_some() {}
    }
}

struct Browser {
    id: String,
    process: Option<OwnedChromium>,
    machine: bool,
    online: bool,
    allow_web: bool,
    port: u16,
    path: String,
    transport: Option<Cdp>,
    targets: BTreeSet<String>,
    fixed_target: Option<String>,
    observations: HashMap<String, (Instant, Value)>,
    selected: String,
    selected_document: String,
    viewport: Option<panel::Size>,
    layout: Option<panel::Layout>,
    download_view: Option<(Weak<()>, Value)>,
    uploads: crate::upload::Uploads,
    saved_personal_tabs: Option<crate::state::PersonalTabs>,
    /// Tabs whose last read failed or timed out, so later reads skip them for a while.
    unreadable: HashMap<String, Instant>,
}

impl Browser {
    async fn research_page(&self, input: &research::ReadPage, remaining: Duration) -> Result<Value> {
        let mut cdp = self.connect().await?;
        let target = cdp
            .call(None, "Target.createTarget", json!({"url": "about:blank", "background": true}))
            .await?;
        let target_id = field(&target, "targetId")?.to_owned();
        let result = tokio::time::timeout(remaining, async {
        let session = cdp.attach(&target_id).await?;
        let navigation = cdp
            .call(Some(&session), "Page.navigate", json!({"url": input.url}))
            .await?;
        if navigation.get("errorText").is_some() || navigation["isDownload"] == true {
            return Err("research_source_unavailable");
        }
        let mut loader = navigation["loaderId"].as_str().unwrap_or("").to_owned();
        loop {
            let tree = cdp
                .call(Some(&session), "Page.getFrameTree", json!({}))
                .await?;
            let frame = &tree["frameTree"]["frame"];
            if let Some(current) = frame["loaderId"].as_str() {
                loader = current.to_owned();
            }
            if !loader.is_empty() {
                let world = match cdp
                    .call(
                        Some(&session),
                        "Page.createIsolatedWorld",
                        json!({"frameId": field(frame, "id")?, "worldName": "kageha-research"}),
                    )
                    .await {
                        Ok(world) => world,
                        Err("cdp_command_failed") => { tokio::time::sleep(Duration::from_millis(25)).await; continue; }
                        Err(error) => return Err(error),
                    };
                let value = match cdp.call(Some(&session), "Runtime.evaluate", json!({
                    "contextId": world["executionContextId"], "returnByValue": true,
                    "expression": r#"document.readyState === 'complete' && ({
                        url: location.href, title: document.title.slice(0, 200),
                        status: performance.getEntriesByType('navigation')[0]?.responseStatus ?? 0,
                        type: document.contentType, text: Array.from((document.body?.innerText ?? '').slice(0, SOURCE_LIMIT * 2)).slice(0, SOURCE_LIMIT).join(''),
                        links: Array.from(document.querySelectorAll('a[href]')).filter(a =>
                            /^https?:/.test(a.href) && a.innerText.trim() && a.checkVisibility()).slice(0, 60)
                            .map(a => ({title:a.innerText.trim().slice(0, 180),url:a.href.slice(0, 2048)}))
                    })"#.replace("SOURCE_LIMIT", &(if input.retain_text { research::MAX_SOURCE_CHARS + 1 } else { 17000 }).to_string()),
                })).await {
                    Ok(value) => value,
                    Err("cdp_command_failed") => { tokio::time::sleep(Duration::from_millis(25)).await; continue; }
                    Err(error) => return Err(error),
                };
                let page = &value["result"]["value"];
                if page["status"].as_u64().is_some_and(|status| status >= 400) {
                    return Err("research_source_unavailable");
                }
                if page["text"].as_str().is_some_and(|text| !text.trim().is_empty()) {
                    let mut result = research::page(
                        "chromium",
                        field(page, "url")?,
                        page["status"].as_u64().unwrap_or(0),
                        field(page, "type")?,
                        "text",
                        page["title"].as_str().unwrap_or(""),
                        page["text"].as_str().unwrap_or(""),
                        input.max_chars,
                    )?;
                    result["links"] = page["links"].clone();
                    research::retain_text(&mut result, page["text"].as_str().unwrap_or(""), input.retain_text);
                    return Ok(result);
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        }).await.unwrap_or(Err("research_unreadable"));
        let _ = cdp.call(None, "Target.closeTarget", json!({"targetId":target_id})).await;
        result
    }

    async fn launch(executable: &Path, root: &Path, allow_web: bool) -> Result<Self> {
        let mut process = OwnedChromium::spawn(executable, root).await?;
        let discovery = process.profile.join("DevToolsActivePort");
        let (port, path) = loop {
            if !process.alive()? {
                return Err("browser_launch_failed");
            }
            if let Ok(file) = tokio::fs::File::open(&discovery).await {
                let mut data = String::new();
                file.take(512)
                    .read_to_string(&mut data)
                    .await
                    .map_err(|_| "cdp_invalid_endpoint")?;
                if let Some(endpoint) = endpoint(&data) {
                    break endpoint;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let mut transport = Cdp::connect(port, &path, allow_web).await?;
        transport
            .call(None, "Browser.getVersion", json!({}))
            .await?;
        if allow_web {
            transport
                .call(
                    None,
                    "Browser.setDownloadBehavior",
                    json!({"behavior": "deny"}),
                )
                .await?;
        }
        Ok(Self::new(Some(process), port, path, transport, allow_web))
    }

    fn new(
        process: Option<OwnedChromium>,
        port: u16,
        path: String,
        transport: Cdp,
        allow_web: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string(),
            process,
            machine: false,
            online: true,
            allow_web,
            port,
            path,
            transport: Some(transport),
            targets: BTreeSet::new(),
            fixed_target: None,
            observations: HashMap::new(),
            selected: String::new(),
            selected_document: String::new(),
            viewport: None,
            layout: None,
            download_view: None,
            uploads: Default::default(),
            saved_personal_tabs: None,
            unreadable: HashMap::new(),
        }
    }

    fn alive(&mut self) -> Result<bool> {
        self.process.as_mut().map_or(Ok(true), OwnedChromium::alive)
    }

    async fn connect(&self) -> Result<Cdp> {
        Cdp::connect(self.port, &self.path, self.allow_web).await
    }

    fn save_personal_selection(
        &mut self,
        root: &Path,
        profile: &str,
        selected: &str,
        include_claim: bool,
    ) -> Result<()> {
        let mut targets = self.targets.clone();
        if include_claim {
            targets.insert(selected.to_owned());
        }
        let saved = crate::state::PersonalTabs {
            port: self.port,
            debugger_path: self.path.clone(),
            selected: selected.to_owned(),
            targets: targets.into_iter().collect(),
        };
        if self.saved_personal_tabs.as_ref() != Some(&saved) {
            crate::state::save_personal_tabs(root, profile, &saved)
                .map_err(|_| "browser_profile_state_unavailable")?;
            self.saved_personal_tabs = Some(saved);
        }
        Ok(())
    }

    fn drop_selected_tab(&mut self, tabs: Value) -> (Value, String, String, Option<panel::Size>) {
        // The tab stays in the owned set: it may recover, and the agent can still see it.
        self.unreadable.insert(std::mem::take(&mut self.selected), Instant::now());
        self.selected_document.clear();
        self.layout = None;
        (tabs, String::new(), String::new(), None)
    }

    async fn read_selection(&mut self) -> Result<(Value, String, String, Option<panel::Size>)> {
        let mut cdp = self.connect().await?;
        let result = self.operation(&mut cdp, "list_tabs", "", false).await?;
        if !self.targets.contains(&self.selected) {
            if self.targets.len() != 1 {
                return Ok((result, String::new(), String::new(), None));
            }
            self.selected = self.targets.first().cloned().unwrap_or_default();
            self.selected_document.clear();
            self.layout = None;
        }
        // A selected tab that is closed, crashed, discarded or hung is a bad tab, not a bad
        // browser. A hung renderer never answers, so bound the read; the outer probe for an
        // attached browser would otherwise time out and mark the whole profile offline.
        self.unreadable.retain(|_, at| at.elapsed() < UNREADABLE_TAB_MEMORY);
        if self.unreadable.contains_key(&self.selected) {
            return Ok(self.drop_selected_tab(result));
        }
        let limit = if self.process.is_some() { TAB_READ_OWNED } else { TAB_READ_ATTACHED };
        let selected = self.selected.clone();
        let read = tokio::time::timeout(limit, async {
            let session = cdp.attach(&selected).await?;
            let frame = main_frame(&mut cdp, &session).await?;
            Ok::<_, &'static str>((session, frame))
        })
        .await;
        let (session, (frame_id, document)) = match read {
            Ok(Ok(read)) => read,
            Ok(Err("cdp_command_failed")) | Err(_) => return Ok(self.drop_selected_tab(result)),
            Ok(Err(error)) => return Err(error),
        };
        let viewport = panel::Viewport::read(&mut cdp, &session, &frame_id)
            .await
            .ok()
            .map(|v| v.size);
        Ok((result, self.selected.clone(), document, viewport))
    }

    async fn set_panel_layout(
        &mut self,
        cdp: Cdp,
        session: String,
        size: Option<panel::Size>,
        dispatched: &AtomicBool,
    ) -> Result<()> {
        let Some(size) = size else {
            if let Some(mut layout) = self.layout.take() {
                dispatched.store(true, Ordering::Relaxed);
                layout
                    .cdp
                    .call(
                        Some(&layout.session),
                        "Emulation.clearDeviceMetricsOverride",
                        json!({}),
                    )
                    .await?;
            }
            return Ok(());
        };
        // Standalone clients may use small viewports (e.g. MiniWoB 332×214).
        // UI minimum panel sizes belong to the host, not the browser runtime.
        if !(1..=2560).contains(&size.width) || !(1..=2400).contains(&size.height) {
            return Err("invalid_panel_geometry");
        }
        let mut layout = self
            .layout
            .take()
            .unwrap_or(panel::Layout { cdp, session, size });
        dispatched.store(true, Ordering::Relaxed);
        let dpr = size.dpr.clamp(1.0, 3.0);
        layout.cdp.call(Some(&layout.session), "Emulation.setDeviceMetricsOverride", json!({
            "width": size.width, "height": size.height, "deviceScaleFactor": dpr, "mobile": false,
        })).await?;
        layout.size = size;
        self.layout = Some(layout);
        Ok(())
    }

    async fn operation(
        &mut self,
        cdp: &mut Cdp,
        method: &str,
        argument: &str,
        compact: bool,
    ) -> Result<Value> {
        let targets = self.refresh(cdp).await?;
        match method {
            "open_tab" => {
                if self.fixed_target.is_some() {
                    return Err("browser_embedded_single_tab");
                }
                cdp.check_url(argument)?;
                if self.targets.len() >= MAX_TABS {
                    return Err("tab_capacity_exceeded");
                }
                let result = cdp
                    .call(None, "Target.createTarget", json!({"url": argument, "background": !self.machine}))
                    .await?;
                let id = field(&result, "targetId")?;
                self.targets.insert(id.to_owned());
                // Creation is acknowledged independently of navigation. Keep
                // the known target visible even if a later navigation reply is
                // lost; recovery must inspect it rather than create a duplicate.
                self.selected = id.to_owned();
                self.selected_document.clear();
                self.viewport = None;
                self.layout = None;
                self.observations.clear();
                // Native creation starts navigation without attaching a session
                // to a renderer still being initialized. This acknowledges only
                // target creation; snapshots/waits must verify the loaded page.
                Ok(json!({"browser_id": self.id, "target_id": id}))
            }
            "list_tabs" => Ok(json!({"browser_id": self.id, "tabs": self.owned_tabs(&targets), "tab_count": self.targets.len(), "tab_limit": MAX_TABS})),
            "observe_tab" => {
                if !self.targets.contains(argument) {
                    return Err("target_not_owned");
                }
                let mut snapshot = match observe::capture(cdp, &self.id, argument, &targets).await {
                    Ok(snapshot) => snapshot,
                    Err("document_changed") => {
                        // A new target may commit its initial navigation while
                        // its first read is in flight. Only repeat the read on a
                        // fresh connection; no page input is replayed.
                        *cdp = self.connect().await?;
                        let targets = self.refresh(cdp).await?;
                        observe::capture(cdp, &self.id, argument, &targets).await?
                    }
                    Err(error) => return Err(error),
                };
                snapshot["observation_id"] = json!(Uuid::new_v4().simple().to_string());
                self.observations
                    .insert(argument.to_owned(), (Instant::now(), snapshot.clone()));
                Ok(if compact {
                    observe::compact(&snapshot)
                } else {
                    observe::expanded(snapshot)
                })
            }
            _ => Err("unsupported_method"),
        }
    }

    fn owned_tabs(&self, targets: &[Value]) -> Vec<Value> {
        targets
            .iter()
            .filter_map(|target| {
                let id = target["targetId"].as_str()?;
                self.targets.contains(id).then(|| json!({
                "target_id": id,
                "title": observe::bounded_text(&target["title"]),
                // URLs bind observations, credentials and navigation authority.
                // Display clipping must never change that identity.
                "url": target["url"].as_str().unwrap_or(""),
                "opener_id": target["openerId"].as_str().filter(|id| self.targets.contains(*id)),
            }))
            })
            .collect()
    }

    async fn refresh(&mut self, cdp: &mut Cdp) -> Result<Vec<Value>> {
        let mut response = cdp.call(None, "Target.getTargets", json!({})).await?;
        let targets = response["targetInfos"]
            .take()
            .as_array()
            .cloned()
            .ok_or("cdp_invalid_reply")?;
        if let Some(target) = &self.fixed_target {
            if !targets.iter().any(|info| embedded_target(info, target)) {
                return Err("browser_adapter_unavailable");
            }
            return Ok(targets);
        }
        let pages: HashMap<&str, &Value> = targets
            .iter()
            .filter(|target| target["type"] == "page" && target["subtype"].is_null())
            .filter_map(|target| Some((target["targetId"].as_str()?, target)))
            .collect();
        self.targets.retain(|id| pages.contains_key(id.as_str()));
        if self.machine {
            for id in pages.keys() {
                if self.targets.len() >= MAX_TABS { break; }
                self.targets.insert((*id).to_owned());
            }
        }
        self.uploads.retain(|id| self.targets.contains(id));
        self.observations.retain(|id, _| self.targets.contains(id));
        loop {
            let previous = self.targets.len();
            for (id, target) in &pages {
                if self.targets.len() >= MAX_TABS {
                    break;
                }
                if let Some(opener) = target["openerId"]
                    .as_str()
                    .filter(|id| self.targets.contains(*id))
                {
                    if target["browserContextId"] == pages[opener]["browserContextId"] {
                        self.targets.insert((*id).to_owned());
                    }
                }
            }
            if previous == self.targets.len() {
                return Ok(targets);
            }
        }
    }

    async fn close(mut self) -> Result<()> {
        let Some(mut process) = self.process.take() else {
            return Ok(());
        };
        if !process.alive()? {
            return Ok(());
        }
        let graceful = async {
            let mut cdp = match self.transport.take() {
                Some(cdp) => cdp,
                None => self.connect().await?,
            };
            let _ = cdp.call(None, "Browser.close", json!({})).await;
            process.wait().await?;
            Ok::<_, &'static str>(())
        };
        if !matches!(
            tokio::time::timeout(Duration::from_secs(2), graceful).await,
            Ok(Ok(()))
        ) {
            process.stop().await?;
        }
        Ok(())
    }
}

fn embedded_target(info: &Value, target: &str) -> bool {
    info["targetId"] == target
        && matches!(info["type"].as_str(), Some("page" | "webview"))
        && info["subtype"].is_null()
}

async fn main_frame(cdp: &mut Cdp, session: &str) -> Result<(String, String)> {
    let frame = cdp
        .call(Some(session), "Page.getFrameTree", json!({}))
        .await?;
    let frame = &frame["frameTree"]["frame"];
    Ok((
        field(frame, "id")?.to_owned(),
        field(frame, "loaderId")?.to_owned(),
    ))
}

fn endpoint(value: &str) -> Option<(u16, String)> {
    let mut lines = value.lines();
    let port: u16 = lines.next()?.parse().ok()?;
    let path = lines.next()?;
    (port > 0 && cdp::debugger_path(path)).then(|| (port, path.to_owned()))
}

fn personal_tab_identity(tab: &Value) -> Option<(&str, &str, &str)> {
    if tab["type"] != "page" || !tab["subtype"].is_null() {
        return None;
    }
    Some((
        tab["targetId"].as_str()?,
        tab["url"].as_str()?,
        tab["title"].as_str()?,
    ))
}

pub(crate) fn fixture_url(url: &str) -> bool {
    use tokio_tungstenite::tungstenite::http::Uri;
    if url == "about:blank" {
        return true;
    }
    url.len() <= 2048
        && url.parse::<Uri>().is_ok_and(|uri| {
            uri.scheme_str() == Some("http")
                && matches!(uri.host(), Some("127.0.0.1" | "localhost"))
                && uri.port_u16().is_some_and(|port| port > 0)
                && !uri
                    .authority()
                    .is_some_and(|authority| authority.as_str().contains('@'))
        })
}
