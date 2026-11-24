//! Version 2 control authority, independent of Kageha and any model provider.
use crate::DesktopBackend;
use base64::Engine;
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::File,
    path::Path,
    time::{Duration, Instant},
};

const ACTIONS: &[&str] = &[
    "click",
    "type_text",
    "press_key",
    "set_value",
    "scroll",
    "drag",
    "perform_secondary_action",
    "select_text",
    "focus_window",
    "focus_element",
];
const OPERATIONS: &[&str] = &[
    "info",
    "permissions",
    "activity",
    "list_apps",
    "list_windows",
    "observe",
    "inspect",
    "preview",
    "acquire",
    "renew",
    "release",
    "execute",
    "receipt",
];
const MAX_RECEIPTS: i64 = 4096;

/// One controller owns one OS desktop, even when requests target different apps.
/// Call through a single serialized owner. Untrusted tenants need distinct OS sessions.
pub struct Controller<B> {
    backend: B,
    journal: Connection,
    _lock: File,
    _desktop_lock: Option<File>,
    epoch: String,
    lease: Option<Lease>,
    observation: Option<Observation>,
    preview: Option<Preview>,
    sequence: u64,
    hold: Option<String>,
    activity_revision: Option<String>,
    background_coverage: Option<u64>,
    authorize: Option<Authorizer>,
    capture_root: std::path::PathBuf,
    captures: std::collections::VecDeque<(String, std::path::PathBuf)>,
}
type Authorizer = Box<dyn FnMut(&str, &str, &Value) -> Result<(), String>>;
struct Lease {
    owner: String,
    token: String,
    expires: Instant,
    background: bool,
    target: Option<(u64, u64)>,
}
struct Preview {
    owner: String,
    token: String,
    pid: u64,
    window: u64,
    expires: Instant,
}
struct Observation {
    owner: String,
    token: String,
    pid: u64,
    window: u64,
    native_session: String,
    elements: HashSet<String>,
    at: Instant,
}

fn fresh_nonce() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| error.to_string())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn error(code: &str, message: &str) -> Value {
    json!({"status":"error","error":{"code":code,"message":message},"execution_phase":"not_dispatched","retryable":false})
}
fn fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let map = value.as_object().ok_or("expected an object")?;
    if map.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown field".into());
    }
    Ok(())
}
fn identifier<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
        })
        .ok_or_else(|| format!("invalid {key}"))
}
fn positive(value: &Value, key: &str, maximum: u64) -> Result<u64, String> {
    value[key]
        .as_u64()
        .filter(|n| *n > 0 && *n <= maximum)
        .ok_or_else(|| format!("invalid {key}"))
}
fn owner(request: &Value) -> Result<String, String> {
    Ok(json!([
        identifier(request, "workspace_id")?,
        identifier(request, "client_id")?
    ])
    .to_string())
}

impl<B: DesktopBackend> Controller<B> {
    /// Journal parent must be private. Never use a shared journal across OS desktops.
    /// A crashed pending dispatch remains `unknown`; neither restart nor duplicate
    /// request IDs will replay it. Stored replies contain no action text/arguments.
    pub fn open(backend: B, path: &Path) -> Result<Self, String> {
        let desktop_lock = backend.desktop_id().map(lock_desktop).transpose()?;
        #[cfg(unix)]
        let lock = {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or("journal parent required")?;
            let metadata = std::fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
            if !metadata.is_dir()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err("journal parent must be an owned private directory".into());
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .map_err(|e| e.to_string())?;
            let metadata = file.metadata().map_err(|e| e.to_string())?;
            if !metadata.is_file()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.nlink() != 1
            {
                return Err("journal must be an owned private regular file".into());
            }
            drop(file);
            // flock on the database itself conflicts with SQLite locking on macOS.
            // Keep the lifetime owner lock in its own private sidecar.
            let lock_path = path.with_file_name(format!(
                "{}.owner.lock",
                path.file_name()
                    .ok_or("journal file name required")?
                    .to_string_lossy()
            ));
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(lock_path)
                .map_err(|e| e.to_string())?;
            let metadata = lock.metadata().map_err(|e| e.to_string())?;
            if !metadata.is_file()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.nlink() != 1
            {
                return Err("journal lock must be an owned private regular file".into());
            }
            lock.try_lock_exclusive()
                .map_err(|_| "journal already has a live controller")?;
            lock
        };
        #[cfg(not(unix))]
        return Err("private journal storage is not implemented on this OS".into());
        #[cfg(unix)]
        {
            let journal = Connection::open(path).map_err(|e| e.to_string())?;
            journal
                .execute_batch(
                    "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
                CREATE TABLE IF NOT EXISTS receipts (
                    owner TEXT NOT NULL, request_id TEXT NOT NULL, intent TEXT NOT NULL,
                    epoch TEXT NOT NULL, response TEXT,
                    PRIMARY KEY(owner, request_id));
                CREATE TABLE IF NOT EXISTS control_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);",
                )
                .map_err(|e| e.to_string())?;
            let capture_root = path.with_file_name(format!(
                "{}.captures",
                path.file_name()
                    .ok_or("journal file name required")?
                    .to_string_lossy()
            ));
            let stored_hold: Option<String> = journal
                .query_row(
                    "SELECT value FROM control_state WHERE key='hold'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let armed: Option<String> = journal
                .query_row(
                    "SELECT value FROM control_state WHERE key='armed'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let hold = stored_hold
                .or_else(|| (armed.as_deref() == Some("1")).then(|| "helper_restarted".into()));
            if let Some(reason) = hold.as_ref() {
                journal.execute("INSERT INTO control_state(key,value) VALUES('hold',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[reason]).map_err(|e|e.to_string())?;
            }
            Ok(Self {
                backend,
                journal,
                _lock: lock,
                _desktop_lock: desktop_lock,
                epoch: format!("computer-{}", fresh_nonce()?),
                lease: None,
                observation: None,
                preview: None,
                sequence: 0,
                hold,
                activity_revision: None,
                background_coverage: None,
                authorize: None,
                capture_root,
                captures: Default::default(),
            })
        }
    }

    fn persist_control(&self, key: &str, value: &str) -> Result<(), String> {
        self.journal.execute("INSERT INTO control_state(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value]).map(|_|()).map_err(|e|e.to_string())
    }
    fn pause_for_intervention(&mut self, reason: &str) -> Result<(), String> {
        self.hold = Some(reason.into());
        if self.background_lease() {
            self.backend.clear_background_activity();
        }
        self.lease = None;
        self.observation = None;
        self.activity_revision = None;
        self.background_coverage = None;
        // The armed marker remains set if persisting the reason fails, so a
        // restart still requires operator review instead of clearing authority.
        self.persist_control("hold", reason)
    }
    fn sample_activity(&mut self) -> Result<Option<Value>, String> {
        let activity = self.backend.input_activity()?;
        if let Some(value) = activity.as_ref() {
            Self::check_activity(value)?;
        } else if self.backend.desktop_id().is_some() {
            return Err("Production adapter has no human-input monitor".into());
        }
        Ok(activity)
    }
    fn check_activity(value: &Value) -> Result<(), String> {
        if value["available"] != true
            || !value["revision"].as_str().is_some_and(|s| !s.is_empty() && s.len() <= 256)
            || !value["active"].is_boolean()
        {
            return Err(if value["unavailable_reason"] == "secure_input" { "macOS Secure Input is active; finish the secure input session before returning computer control" } else { "Human-input monitoring is unavailable; enable Input Monitoring for Kageha Computer" }.into());
        }
        Ok(())
    }
    fn background_lease(&self) -> bool {
        self.lease.as_ref().is_some_and(|lease| lease.background)
    }
    fn background_activity(&mut self, pid: u64, window: u64) -> Result<Value, String> {
        let value = self.backend.background_activity(pid, window)?;
        Self::check_activity(&value)?;
        if !value["coverage_revision"].is_u64() {
            return Err("Background input monitoring did not report coverage".into());
        }
        Ok(value)
    }
    fn sync_activity(&mut self) -> Result<(), String> {
        let Some(expected) = self.activity_revision.clone() else {
            return Ok(());
        };
        if let Some((pid, window)) = self.lease.as_ref().and_then(|lease| lease.target) {
            match self.background_activity(pid, window) {
                Ok(value) if value["coverage_revision"].as_u64() != self.background_coverage => {
                    self.pause_for_intervention("monitor_unavailable")?;
                }
                Ok(value) if value["active"] == false && value["revision"] == expected => {}
                Ok(_) => {
                    self.observation = None;
                    self.activity_revision = None;
                }
                Err(_) => self.pause_for_intervention("monitor_unavailable")?,
            }
            return Ok(());
        }
        match self.sample_activity() {
            // Hardware state can briefly include our own posted key-down before its
            // queued key-up is processed. Once monitoring has started, external
            // events are identified by the tap revision, not that state.
            Ok(Some(value)) if value["revision"] == expected => Ok(()),
            Ok(Some(_)) => self.pause_for_intervention("human_input"),
            _ => self.pause_for_intervention("monitor_unavailable"),
        }
    }
    /// Host-only operator gesture. Never exposed as an agent operation.
    /// It clears a pause, not a receipt, and grants no lease or observation.
    pub fn resume_control(&mut self) -> Result<(), String> {
        if self
            .sample_activity()?
            .as_ref()
            .is_some_and(|v| v["active"] == true)
        {
            return Err("Release physical keys/buttons before returning computer control".into());
        }
        self.journal
            .execute_batch(
                "BEGIN IMMEDIATE; DELETE FROM control_state WHERE key IN ('hold','armed'); COMMIT;",
            )
            .map_err(|e| e.to_string())?;
        self.hold = None;
        self.lease = None;
        self.observation = None;
        self.activity_revision = None;
        self.background_coverage = None;
        Ok(())
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Return pixels only from a capture this controller issued for this exact
    /// observation. Never open an arbitrary path supplied by a model or result.
    pub fn observation_image(&self, reply: &Value) -> Result<Option<(String, String)>, String> {
        let Some(token) = reply["observation_id"]
            .as_str()
            .or_else(|| reply["inspection_id"].as_str())
        else {
            return Ok(None);
        };
        let Some((_, path)) = self.captures.iter().find(|(id, _)| id == token) else {
            return Ok(None);
        };
        read_image(path).map(Some)
    }

    /// Short-lived read-only frames never replace an actionable observation.
    fn preview(&mut self, who: &str, args: &Value) -> Result<Value, String> {
        fields(args, &["preview_id"])?;
        let id = identifier(args, "preview_id")?;
        let Some(grant) = self
            .preview
            .as_ref()
            .filter(|p| p.owner == who && p.token == id && p.expires > Instant::now())
        else {
            return Ok(error(
                "preview_expired",
                "Observe this window again to grant a new read-only preview",
            ));
        };
        let (pid, window) = (grant.pid, grant.window);
        private_capture_directory(&self.capture_root)?;
        let frame_token = self.token("preview-frame");
        let path = self.capture_root.join(format!("{frame_token}.jpg"));
        let reply = self.backend.call("capture_window", json!({"pid":pid,"window_id":window,"screenshot_out_file":path,"image_format":"jpeg","max_image_dimension":1280}));
        let result = match reply {
            Ok(value) if value["pid"] == pid && value["window_id"] == window && value["screenshot_file_path"].as_str() == path.to_str() => read_image(&path).map(|(mime,data)| json!({"status":"ok","result":{"pid":pid,"window_id":window,"image":{"mime_type":mime,"data":data},"width":value["width"],"height":value["height"]}})),
            Ok(_) => Ok(error("preview_binding_failed", "Native preview did not match the granted window")),
            Err(message) => Ok(error("preview_unavailable", &message)),
        };
        let _ = std::fs::remove_file(path);
        result
    }

    /// Host-only policy hook, denied by default. Receives immutable resolved
    /// arguments (including exact pid/window/session) immediately before journal
    /// admission. An agent cannot install or change this callback via RPC.
    /// The host must authenticate scope labels before routing remote requests.
    pub fn set_authorizer<F>(&mut self, authorize: F)
    where
        F: FnMut(&str, &str, &Value) -> Result<(), String> + 'static,
    {
        self.authorize = Some(Box::new(authorize));
    }

    /// Host-only takeover, deliberately absent from the agent operation catalog.
    /// It prevents later input; an OS call already entered cannot be cancelled.
    pub fn take_control(&mut self) {
        if self.background_lease() {
            self.backend.clear_background_activity();
        }
        self.lease = None;
        self.observation = None;
        self.activity_revision = None;
        self.background_coverage = None;
        if self.hold.is_none() {
            let _ = self.persist_control("armed", "0");
        }
    }

    fn token(&mut self, prefix: &str) -> String {
        self.sequence += 1;
        format!("{prefix}-{}-{}", self.epoch, self.sequence)
    }
    fn lease_valid(&self, who: &str, args: &Value) -> bool {
        self.lease.as_ref().is_some_and(|lease| {
            lease.owner == who
                && lease.token == args["lease_id"].as_str().unwrap_or("")
                && Instant::now() < lease.expires
        })
    }
    fn receipt(&self, who: &str, request_id: &str) -> Result<Option<(String, Value)>, String> {
        let record: Option<(String, String, Option<String>)> = self
            .journal
            .query_row(
                "SELECT intent, epoch, response FROM receipts WHERE owner=? AND request_id=?",
                params![who, request_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        record.map(|(intent, epoch, response)| {
            let response = match response {
                Some(text) => serde_json::from_str(&text).map_err(|e| e.to_string())?,
                None => json!({"status":"unknown","execution_phase":"dispatch_started","receipt":{"request_id":request_id,"service_epoch":epoch,"state":"unknown"},"retryable":false,"summary":"Dispatch was reserved but its outcome was not recorded. Inspect fresh state; do not replay input."}),
            };
            Ok((intent, response))
        }).transpose()
    }

    /// Strict JSON RPC payload. Scope labels are routing, not authentication.
    /// Embed behind authenticated host transport or use an owned stdio process.
    pub fn handle(&mut self, request: Value) -> Value {
        let mut reply = match self.handle_inner(&request) {
            Ok(value) => value,
            Err(message) => error("invalid_request", &message),
        };
        if let Some(reason) = self.hold.as_ref() {
            reply["control"] =
                json!({"owner":"user","reason":reason,"operator_resume_required":true});
        }
        reply
    }
    fn handle_inner(&mut self, request: &Value) -> Result<Value, String> {
        fields(
            request,
            &[
                "protocol",
                "workspace_id",
                "client_id",
                "request_id",
                "service_epoch",
                "operation",
                "args",
            ],
        )?;
        if request["protocol"] != 2 {
            return Ok(error("protocol_mismatch", "computer protocol 2 required"));
        }
        let who = owner(request)?;
        let request_id = identifier(request, "request_id")?;
        let op = request["operation"].as_str().ok_or("operation required")?;
        let args = request.get("args").ok_or("args required")?;
        if !args.is_object() {
            return Err("args must be an object".into());
        }
        if op == "info" {
            fields(args, &[])?;
            return Ok(
                json!({"status":"ok","protocol":2,"service_epoch":self.epoch,"process_id":std::process::id(),"operations":OPERATIONS,"actions":ACTIONS,
                "limits":{"lease_ms":60000,"observation_ms":30000,"receipts":MAX_RECEIPTS},
                "host_takeover":true,"input_authorized":self.authorize.is_some(),"background_accessibility":self.backend.supports_background_accessibility(),"automatic_input_retry":false,"tenant_isolation":"one trusted desktop owner per process"}),
            );
        }
        // Receipt lookup intentionally survives epoch changes. It cannot dispatch.
        if op == "receipt" {
            fields(args, &["request_id"])?;
            let lookup = identifier(args, "request_id")?;
            return Ok(self
                .receipt(&who, lookup)?
                .map(|(_, value)| value)
                .unwrap_or_else(|| {
                    error(
                        "receipt_not_found",
                        "no admitted action with this request id",
                    )
                }));
        }
        if request["service_epoch"].as_str() != Some(&self.epoch) {
            return Ok(error(
                "service_epoch_stale",
                "Connect to this controller and observe again after restart",
            ));
        }
        if [
            "acquire", "renew", "observe", "execute", "activity", "preview",
        ]
        .contains(&op)
        {
            self.sync_activity()?;
        }
        match op {
            "activity" => {
                fields(args, &[])?;
                Ok(json!({"status":"ok","result":self.backend.input_activity()?}))
            }
            "acquire" => {
                fields(args, &["ttl_ms", "background"])?;
                let ttl = positive(args, "ttl_ms", 60000)?;
                let background = match args.get("background") {
                    Some(value) => value.as_bool().ok_or("background must be boolean")?,
                    None => false,
                };
                if background && !self.backend.supports_background_accessibility() {
                    return Ok(error("background_action_unavailable", "Update the Mac computer helper for background Accessibility input"));
                }
                if self.hold.is_some() {
                    return Ok(error(
                        "human_intervention",
                        "Computer control is paused; an operator must explicitly return control",
                    ));
                }
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|lease| Instant::now() < lease.expires)
                {
                    return Ok(error(
                        "desktop_busy",
                        "desktop already has a live owner; renew or explicitly release it",
                    ));
                }
                if self.background_lease() {
                    self.backend.clear_background_activity();
                }
                self.activity_revision = if self.authorize.is_some() {
                    match self.sample_activity() {
                        Ok(Some(value)) if background || value["active"] == false => {
                            self.persist_control("armed", "1")?;
                            (!background).then(|| value["revision"].as_str().unwrap().into())
                        }
                        Ok(None) if !background => None,
                        Ok(None) => return Ok(error("input_monitor_unavailable", "Background input requires an active input monitor")),
                        Ok(Some(_)) => {
                            self.pause_for_intervention("human_input")?;
                            return Ok(error(
                                "human_intervention",
                                "Physical input is active; an operator must return control",
                            ));
                        }
                        Err(message) => {
                            self.pause_for_intervention("monitor_unavailable")?;
                            return Ok(error("input_monitor_unavailable", &message));
                        }
                    }
                } else {
                    None
                };
                self.preview = None;
                let token = self.token("lease");
                self.lease = Some(Lease {
                    owner: who,
                    token: token.clone(),
                    expires: Instant::now() + Duration::from_millis(ttl),
                    background,
                    target: None,
                });
                self.observation = None;
                Ok(json!({"status":"ok","lease_id":token,"expires_in_ms":ttl}))
            }
            "renew" | "release" => {
                fields(
                    args,
                    if op == "renew" {
                        &["lease_id", "ttl_ms"]
                    } else {
                        &["lease_id"]
                    },
                )?;
                if !self.lease_valid(&who, args) {
                    return Ok(error(
                        "lease_invalid",
                        "desktop lease expired or belongs to another owner",
                    ));
                }
                if op == "renew" {
                    let ttl = positive(args, "ttl_ms", 60000)?;
                    self.lease.as_mut().unwrap().expires =
                        Instant::now() + Duration::from_millis(ttl);
                } else {
                    self.take_control();
                }
                Ok(json!({"status":"ok"}))
            }
            "permissions" | "list_apps" | "list_windows" => {
                fields(args, if op == "list_windows" { &["pid"] } else { &[] })?;
                if op == "list_windows" {
                    positive(args, "pid", i32::MAX as u64)?;
                }
                let method = match op {
                    "permissions" => "check_permissions",
                    "list_apps" => "list_apps",
                    _ => "list_windows",
                };
                Ok(match self.backend.call(method, args.clone()) {
                    Ok(value) => json!({"status":"ok","result":value}),
                    Err(message) => error("observation_failed", &message),
                })
            }
            "observe" => self.observe(&who, args),
            "inspect" => self.inspect(&who, args),
            "preview" => self.preview(&who, args),
            "execute" => self.execute(&who, request_id, args),
            _ => Ok(error(
                "unsupported_operation",
                "operation is not advertised by this controller",
            )),
        }
    }
    /// Recovery reads remain possible while input is held. Inspection never
    /// creates an actionable observation or grants an input lease.
    fn inspect(&mut self, who: &str, args: &Value) -> Result<Value, String> {
        fields(
            args,
            &[
                "pid",
                "window_id",
                "include_screenshot",
                "max_elements",
                "max_image_dimension",
            ],
        )?;
        let pid = positive(args, "pid", i32::MAX as u64)?;
        let window = positive(args, "window_id", u32::MAX as u64)?;
        let screenshot = match args.get("include_screenshot") {
            None => false,
            Some(v) => v.as_bool().ok_or("include_screenshot must be boolean")?,
        };
        let token = self.token("inspection");
        let cap = match args.get("max_elements") {
            None => 400,
            Some(_) => positive(args, "max_elements", 2000)?,
        };
        let dimension = match args.get("max_image_dimension") {
            None => 1280,
            Some(_) => positive(args, "max_image_dimension", 4096)?,
        };
        let mut native = json!({"pid":pid,"window_id":window,"session":token,"include_screenshot":screenshot,"include_accessibility_tree":true,"max_elements":cap,"max_image_dimension":dimension,"image_format":"jpeg"});
        let capture = if screenshot {
            private_capture_directory(&self.capture_root)?;
            let path = self.capture_root.join(format!("{token}.jpg"));
            native["screenshot_out_file"] = json!(path);
            Some(path)
        } else {
            None
        };
        let value = self.backend.call("get_window_state", native)?;
        if value["ok"] != true || value["pid"] != pid || value["window_id"] != window {
            return Ok(error(
                "target_changed",
                "Inspection did not match the requested exact window",
            ));
        }
        if let Some(path) = capture {
            if value["screenshot_file_path"].as_str() != path.to_str() {
                return Ok(error(
                    "capture_binding_failed",
                    "Inspection capture path did not match",
                ));
            }
            self.captures.push_back((token.clone(), path));
            while self.captures.len() > 8 {
                if let Some((_, old)) = self.captures.pop_front() {
                    let _ = std::fs::remove_file(old);
                }
            }
        }
        let preview = self.token("preview");
        self.preview = Some(Preview {
            owner: who.into(),
            token: preview.clone(),
            pid,
            window,
            expires: Instant::now() + Duration::from_secs(60),
        });
        Ok(
            json!({"status":"ok","read_only":true,"inspection_id":token,"preview_id":preview,"result":value}),
        )
    }

    fn observe(&mut self, who: &str, args: &Value) -> Result<Value, String> {
        fields(
            args,
            &[
                "lease_id",
                "pid",
                "window_id",
                "include_screenshot",
                "max_elements",
                "image_format",
                "max_image_dimension",
            ],
        )?;
        if !self.lease_valid(who, args) {
            return Ok(error(
                "lease_invalid",
                "Acquire the desktop before observing its actionable state",
            ));
        }
        let pid = positive(args, "pid", i32::MAX as u64)?;
        let window = positive(args, "window_id", u32::MAX as u64)?;
        let screenshot = match args.get("include_screenshot") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or("include_screenshot must be boolean")?,
        };
        let max_elements = match args.get("max_elements") {
            None => 400,
            Some(_) => positive(args, "max_elements", 2000)?,
        };
        let max_image_dimension = match args.get("max_image_dimension") {
            None => 1920,
            Some(_) => positive(args, "max_image_dimension", 4096)?,
        };
        let format = match args.get("image_format") {
            None => "jpeg",
            Some(value) => value.as_str().ok_or("image_format must be text")?,
        };
        if !["jpeg", "png"].contains(&format) {
            return Err("image_format must be jpeg or png".into());
        }
        self.observation = None;
        let native_session = format!("control-{}", self.epoch);
        let token = self.token("observation");
        if self.background_lease() {
            self.activity_revision = None;
            let activity = match self.background_activity(pid, window) {
                Ok(value) => value,
                Err(message) => {
                    self.pause_for_intervention("monitor_unavailable")?;
                    return Ok(error("input_monitor_unavailable", &message));
                }
            };
            if activity["active"] == true {
                return Ok(background_target_in_use());
            }
            self.lease.as_mut().unwrap().target = Some((pid, window));
            self.activity_revision = activity["revision"].as_str().map(str::to_owned);
            self.background_coverage = activity["coverage_revision"].as_u64();
        }
        let mut native = json!({"pid":pid,"window_id":window,"session":native_session,"include_screenshot":screenshot,"include_accessibility_tree":true,"max_elements":max_elements,"max_image_dimension":max_image_dimension,"image_format":format,"follow_focus":false});
        let capture = if screenshot {
            private_capture_directory(&self.capture_root)?;
            let path = self.capture_root.join(format!(
                "{token}.{}",
                if format == "jpeg" { "jpg" } else { "png" }
            ));
            native["screenshot_out_file"] = json!(path);
            Some(path)
        } else {
            None
        };
        let value = match self.backend.call("get_window_state", native) {
            Ok(value) if value["ok"] == true => value,
            Ok(_) => {
                return Ok(error(
                    "observation_failed",
                    "native adapter did not confirm an observation",
                ))
            }
            Err(message) => return Ok(error("observation_failed", &message)),
        };
        if self.background_lease() {
            self.sync_activity()?;
            if self.activity_revision.is_none() {
                if let Some(path) = capture.as_ref() {
                    let _ = std::fs::remove_file(path);
                }
                return Ok(if self.hold.is_some() {
                    error("input_monitor_unavailable", "Input monitoring stopped during observation")
                } else {
                    background_target_in_use()
                });
            }
        }
        // Never silently switch to a different modal/window.
        if value["pid"] != pid || value["window_id"] != window {
            return Ok(error(
                "target_changed",
                "native observation did not match the requested exact window",
            ));
        }
        let elements = value["elements"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["element_token"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(path) = capture {
            if value["screenshot_file_path"].as_str() != path.to_str() {
                return Ok(error(
                    "capture_binding_failed",
                    "adapter returned pixels for a different capture path",
                ));
            }
            self.captures.push_back((token.clone(), path));
            while self.captures.len() > 8 {
                if let Some((_, old)) = self.captures.pop_front() {
                    let _ = std::fs::remove_file(old);
                }
            }
        }
        let preview_id = self.token("preview");
        self.preview = Some(Preview {
            owner: who.into(),
            token: preview_id.clone(),
            pid,
            window,
            expires: Instant::now() + Duration::from_secs(60),
        });
        self.observation = Some(Observation {
            owner: who.into(),
            token: token.clone(),
            pid,
            window,
            native_session,
            elements,
            at: Instant::now(),
        });
        Ok(
            json!({"status":"ok","service_epoch":self.epoch,"observation_id":token,"preview_id":preview_id,"result":value}),
        )
    }
    fn execute(&mut self, who: &str, request_id: &str, args: &Value) -> Result<Value, String> {
        fields(args, &["lease_id", "observation_id", "action"])?;
        // Duplicate lookup precedes lease and observation checks: a lost response
        // can be recovered after observation consumption, lease expiry or restart.
        let intent = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(args).map_err(|e| e.to_string())?)
        );
        if let Some((stored, receipt)) = self.receipt(who, request_id)? {
            return Ok(if stored == intent {
                receipt
            } else {
                error(
                    "request_id_conflict",
                    "request id was admitted for different arguments; no input sent",
                )
            });
        }
        if self.hold.is_some() {
            return Ok(error(
                "human_intervention",
                "Computer control is paused; an operator must explicitly return control",
            ));
        }
        if !self.lease_valid(who, args) {
            return Ok(error(
                "lease_invalid",
                "desktop lease expired or belongs to another owner",
            ));
        }
        let background = self.background_lease();
        if background && self.activity_revision.is_none() {
            return Ok(background_target_in_use());
        }
        let observed = match self.observation.as_ref() {
            Some(observed)
                if observed.owner == who
                    && args["observation_id"].as_str() == Some(&observed.token)
                    && observed.at.elapsed() <= Duration::from_secs(30) =>
            {
                observed
            }
            _ => {
                return Ok(error(
                    "observation_stale",
                    "Observe the exact target again; no input was dispatched",
                ))
            }
        };
        let action = &args["action"];
        fields(action, &["method", "args"])?;
        let method = action["method"].as_str().ok_or("action method required")?;
        if !ACTIONS.contains(&method) {
            return Ok(error(
                "unsupported_action",
                "action is not advertised by this controller",
            ));
        }
        let mut native = action["args"].clone();
        validate_action(method, &native)?;
        if background {
            if let Err(message) = validate_background_action(method, &native) {
                return Ok(error("background_action_unavailable", &message));
            }
            native["background_only"] = json!(true);
        }
        if let Some(element) = native["element_token"].as_str() {
            if !observed.elements.contains(element) {
                return Ok(error(
                    "element_not_observed",
                    "element token was not present in this exact observation",
                ));
            }
        }
        native["pid"] = json!(observed.pid);
        native["window_id"] = json!(observed.window);
        native["session"] = json!(observed.native_session);
        if let Some(revision) = self.activity_revision.as_ref() {
            native["activity_revision"] = json!(revision);
        } else if self.backend.desktop_id().is_some() {
            return Ok(error(
                "observation_stale",
                "Acquire and observe again with input authorization before native input",
            ));
        }
        let authorization = match self.authorize.as_mut() {
            Some(authorize) => authorize(who, method, &native),
            None => Err("Host has not authorized native input for this controller".into()),
        };
        if let Err(message) = authorization {
            return Ok(error("authorization_required", &message));
        }
        self.sync_activity()?;
        if self.hold.is_some() {
            return Ok(error(
                "human_intervention",
                "Human input interrupted host approval; no input sent",
            ));
        }
        if background && self.activity_revision.is_none() {
            return Ok(background_target_in_use());
        }
        // Approval callbacks can wait for a human. Do not let that wait extend
        // the lease or silently refresh an observation selected before approval.
        if !self.lease_valid(who, args) {
            return Ok(error(
                "lease_invalid",
                "Desktop lease expired while awaiting host authorization; no input sent",
            ));
        }
        if self
            .observation
            .as_ref()
            .is_none_or(|observation| observation.at.elapsed() > Duration::from_secs(30))
        {
            return Ok(error(
                "observation_stale",
                "Observation expired while awaiting host authorization; no input sent",
            ));
        }
        let count: i64 = self
            .journal
            .query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        if count >= MAX_RECEIPTS {
            return Ok(error(
                "journal_capacity",
                "action journal is full; archive it while the controller is stopped",
            ));
        }
        // Commit before OS dispatch; failure here sends no input. An unrecorded
        // completion stays unknown, never erased or silently dispatched again.
        self.journal
            .execute(
                "INSERT INTO receipts(owner,request_id,intent,epoch) VALUES(?,?,?,?)",
                params![who, request_id, intent, self.epoch],
            )
            .map_err(|e| e.to_string())?;
        self.sync_activity()?;
        let intervention = self.hold.is_some();
        let target_in_use = background && !intervention && self.activity_revision.is_none();
        let lease_expired = !self.lease_valid(who, args);
        let observation_expired = self
            .observation
            .as_ref()
            .is_none_or(|observation| observation.at.elapsed() > Duration::from_secs(30));
        if lease_expired || observation_expired {
            self.observation = None;
            let reply = json!({"status":"error","execution_phase":"not_dispatched","receipt":{"request_id":request_id,"service_epoch":self.epoch,"state":"not_dispatched"},"error":{"code":if intervention {"human_intervention"} else if target_in_use {"background_target_in_use"} else if lease_expired { "lease_invalid" } else { "observation_stale" },"message":"Authority or observation changed during durable admission; no input sent"},"retryable":false});
            let _ = self.journal.execute(
                "UPDATE receipts SET response=? WHERE owner=? AND request_id=?",
                params![reply.to_string(), who, request_id],
            );
            return Ok(reply);
        }
        self.observation = None;
        let result = self.backend.call(method, native);
        // Even when the just-finished effect is verified, intervention revokes
        // every later input and is returned independently from effect evidence.
        let _ = self.sync_activity();
        let (state, phase, result, message) = match result {
            Ok(value) if value["ok"] == false && value["execution_phase"] == "not_dispatched" => {
                let message = value["error"]["message"]
                    .as_str()
                    .unwrap_or("Native preflight refused input")
                    .to_owned();
                ("not_dispatched", "not_dispatched", value, Some(message))
            }
            Ok(value) if value["verified"] == true && value["ok"] == true => {
                ("verified", "effect_verified", value, None)
            }
            Ok(value) if value["ok"] == true => {
                ("dispatched", "dispatch_acknowledged", value, None)
            }
            Ok(value) => (
                "unknown",
                "dispatch_started",
                value,
                Some("native action did not establish its outcome".to_owned()),
            ),
            Err(message) => ("unknown", "dispatch_started", Value::Null, Some(message)),
        };
        let mut reply = json!({"status":match state { "unknown" => "unknown", "not_dispatched" => "error", _ => "ok" },"execution_phase":phase,"receipt":{"request_id":request_id,"service_epoch":self.epoch,"state":state},"result":result,"retryable":false,
            "summary":"A desktop effect is not proof of task completion or a server-side commit. Observe again before any further input."});
        if let Some(message) = message {
            reply["error"] = json!({"code":if state == "not_dispatched" { "native_preflight_refused" } else { "native_outcome_unknown" },"message":message});
        }
        if background && self.hold.is_none() && self.activity_revision.is_none() {
            reply["background_target_in_use"] = json!(true);
        }
        if self
            .journal
            .execute(
                "UPDATE receipts SET response=? WHERE owner=? AND request_id=?",
                params![reply.to_string(), who, request_id],
            )
            .is_err()
        {
            return Ok(
                json!({"status":"unknown","execution_phase":"dispatch_started","receipt":{"request_id":request_id,"service_epoch":self.epoch,"state":"unknown"},"retryable":false,"summary":"Input may have executed, but its outcome could not be persisted. Do not replay."}),
            );
        }
        Ok(reply)
    }
}

fn background_target_in_use() -> Value {
    error("background_target_in_use", "The target app is in use. Switch to another app and observe the target again.")
}

pub fn validate_background_action(method: &str, args: &Value) -> Result<(), String> {
    if args["element_token"].is_string()
        && args["delivery_mode"] != "foreground"
        && matches!(method, "click" | "set_value" | "type_text" | "select_text" | "perform_secondary_action")
    {
        return Ok(());
    }
    Err("Background action unavailable. Use an observed Accessibility control or pause this action.".into())
}

/// Structural preflight only: does not authorize input or resolve live handles.
pub fn validate_action(method: &str, args: &Value) -> Result<(), String> {
    let allowed: &[&str] = match method {
        "click" => &[
            "element_token",
            "x",
            "y",
            "button",
            "count",
            "delivery_mode",
        ],
        "type_text" => &["element_token", "text"],
        "set_value" => &["element_token", "value"],
        "press_key" => &["key", "delivery_mode"],
        "scroll" => &["direction", "amount", "delivery_mode"],
        "drag" => &["from_x", "from_y", "to_x", "to_y", "delivery_mode"],
        "perform_secondary_action" => &["element_token", "action"],
        "select_text" => &["element_token", "mode", "text", "prefix", "suffix"],
        "focus_window" => &[],
        "focus_element" => &["element_token"],
        _ => return Err("unsupported action".into()),
    };
    fields(args, allowed)?;
    if let Some(mode) = args.get("delivery_mode") {
        if !matches!(mode.as_str(), Some("background" | "foreground")) {
            return Err("unsupported delivery_mode".into());
        }
        if mode == "foreground" && args.get("element_token").is_some() {
            return Err("foreground mouse delivery requires coordinates".into());
        }
    }
    let text = |key: &str| {
        args[key]
            .as_str()
            .filter(|text| text.len() <= 65536)
            .ok_or_else(|| format!("{key} must be bounded text"))
    };
    for key in [
        "element_token",
        "text",
        "value",
        "key",
        "action",
        "prefix",
        "suffix",
    ] {
        if args.get(key).is_some() {
            text(key)?;
        }
    }
    for key in ["x", "y", "from_x", "from_y", "to_x", "to_y"] {
        if args.get(key).is_some()
            && !args[key]
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=100000.0).contains(&n))
        {
            return Err(format!("{key} must be a nonnegative finite coordinate"));
        }
    }
    if [
        "set_value",
        "select_text",
        "perform_secondary_action",
        "focus_element",
    ]
    .contains(&method)
    {
        text("element_token")?;
    }
    match method {
        "click" => {
            let element = args.get("element_token").is_some();
            if element && (args.get("x").is_some() || args.get("y").is_some()) {
                return Err("click requires element or coordinates, not both".into());
            }
            if !(element || args.get("x").is_some() && args.get("y").is_some()) {
                return Err("click requires element or both coordinates".into());
            }
            if args.get("button").is_some()
                && !["left", "right"].contains(&args["button"].as_str().unwrap_or(""))
            {
                return Err("unsupported button".into());
            }
            if let Some(count) = args.get("count") {
                if !count
                    .as_u64()
                    .is_some_and(|n| (1..=3).contains(&n) && (!element || n == 1))
                {
                    return Err("invalid click count".into());
                }
            }
        }
        "type_text" => {
            text("text")?;
        }
        "set_value" => {
            text("value")?;
        }
        "press_key" => {
            if text("key")?.is_empty() {
                return Err("key required".into());
            }
        }
        "perform_secondary_action" => {
            text("action")?;
        }
        "scroll" => {
            if !["up", "down", "left", "right"].contains(&args["direction"].as_str().unwrap_or(""))
            {
                return Err("invalid direction".into());
            }
            if args.get("amount").is_some() {
                positive(args, "amount", 100)?;
            }
        }
        "drag" => {
            for key in ["from_x", "from_y", "to_x", "to_y"] {
                if args.get(key).is_none() {
                    return Err(format!("{key} required"));
                }
            }
        }
        "select_text" => {
            let mode = match args.get("mode") {
                None => "all",
                Some(value) => value.as_str().ok_or("mode must be text")?,
            };
            if !["all", "text", "cursor_before", "cursor_after"].contains(&mode) {
                return Err("unsupported selection mode".into());
            }
            if mode != "all" && text("text")?.is_empty() {
                return Err("selection text must be nonempty".into());
            }
        }
        _ => {}
    }
    Ok(())
}

// A stable per-user device lock prevents independent journals/processes from
// each believing they exclusively own the same physical desktop. This covers
// protocol-2 controllers only; legacy executors must not run alongside them.
#[cfg(unix)]
fn lock_desktop(identity: &str) -> Result<File, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    let uid = unsafe { libc::geteuid() };
    let directory = std::path::Path::new("/tmp").join(format!("kageha-computer-owner-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = std::fs::symlink_metadata(&directory).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 || metadata.uid() != uid {
        return Err("desktop lock directory must be owned and private".into());
    }
    let name = format!("{:x}.lock", Sha256::digest(identity.as_bytes()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join(name))
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err("desktop lock must be an owned private regular file".into());
    }
    file.try_lock_exclusive()
        .map_err(|_| "physical desktop already has a live computer controller")?;
    Ok(file)
}
#[cfg(not(unix))]
fn lock_desktop(_identity: &str) -> Result<File, String> {
    Err("desktop locking is not implemented on this OS".into())
}

#[cfg(unix)]
fn private_capture_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("capture directory must be owned and private".into());
    }
    Ok(())
}
#[cfg(not(unix))]
fn private_capture_directory(_path: &Path) -> Result<(), String> {
    Err("private captures are not implemented on this OS".into())
}

#[cfg(test)]
mod tests {
    use super::validate_action;
    use serde_json::json;
    #[test]
    fn selection_modes_match_native_contract_and_do_not_default_bad_types() {
        for mode in ["all", "text", "cursor_before", "cursor_after"] {
            assert!(validate_action(
                "select_text",
                &json!({"element_token":"observed","mode":mode,"text":"needle"})
            )
            .is_ok());
        }
        for mode in [json!("range"), json!("unknown"), json!(null), json!(7)] {
            assert!(validate_action(
                "select_text",
                &json!({"element_token":"observed","mode":mode,"text":"needle"})
            )
            .is_err());
        }
        assert!(validate_action(
            "select_text",
            &json!({"element_token":"observed","mode":"text","text":""})
        )
        .is_err());
    }
    #[test]
    fn element_focus_requires_an_observed_token_and_cannot_override_its_window() {
        assert!(validate_action("focus_element", &json!({"element_token":"observed"})).is_ok());
        assert!(validate_action("focus_element", &json!({})).is_err());
        assert!(validate_action("focus_element", &json!({"element_token":null})).is_err());
        assert!(validate_action(
            "focus_element",
            &json!({"element_token":"observed","window_id":999})
        )
        .is_err());
    }
}

fn read_image(path: &Path) -> Result<(String, String), String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("capture is not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("capture exceeds the 4 MiB image transport limit".into());
    }
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else {
        return Err("capture is not PNG or JPEG".into());
    };
    Ok((
        mime.into(),
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}
