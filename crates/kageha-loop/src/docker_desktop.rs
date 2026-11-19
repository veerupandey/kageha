//! A chat-owned Linux display. Only Docker CLI stdio crosses the container boundary.
use crate::webui::Database;
use kageha_contracts::execution::Control;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{atomic::AtomicBool, Mutex},
    time::{Duration, Instant},
};
use tiny_http::{Method, Request};

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_linux_desktops(
        chat TEXT PRIMARY KEY, name TEXT UNIQUE NOT NULL, selected INTEGER DEFAULT 0,
        user_control INTEGER DEFAULT 0, agent_access INTEGER DEFAULT 0,
        user_frame TEXT, agent_frame TEXT);
        CREATE TABLE IF NOT EXISTS ui_machine_bindings(
        chat TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, backup TEXT);
        CREATE TABLE IF NOT EXISTS ui_machine_backups(chat TEXT NOT NULL, name TEXT NOT NULL UNIQUE);
        CREATE TABLE IF NOT EXISTS ui_machine_provisions(chat TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE);
        CREATE TABLE IF NOT EXISTS ui_x11_desktops(
        chat TEXT PRIMARY KEY, name TEXT NOT NULL, user_control INTEGER NOT NULL DEFAULT 0,
        agent_access INTEGER NOT NULL DEFAULT 1, user_frame TEXT, agent_frame TEXT)",
    )
}

// Serialize captures, input and provider changes within a chat without holding SQLite during Docker I/O.
static LOCKS: [Mutex<()>; 32] = [const { Mutex::new(()) }; 32];
pub(crate) fn lock(chat: &str) -> std::sync::MutexGuard<'static, ()> {
    let mut hash = DefaultHasher::new();
    chat.hash(&mut hash);
    LOCKS[hash.finish() as usize % LOCKS.len()]
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn database(db: &Database) -> std::sync::MutexGuard<'_, Connection> {
    db.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn selected(db: &Database, chat: &str) -> io::Result<bool> {
    database(db)
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_linux_desktops WHERE chat=? AND selected=1)",
            [chat],
            |r| r.get(0),
        )
        .map_err(io::Error::other)
}

pub(crate) fn binding(db: &Connection, chat: &str) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT m.name FROM ui_machine_bindings m JOIN ui_linux_desktops d
         ON d.chat=m.chat AND d.name=m.name WHERE m.chat=? AND d.selected=1",
        [chat], |row| row.get(0),
    ).optional()
}

pub(crate) fn user_driving(db: &Connection, chat: &str) -> rusqlite::Result<Option<bool>> {
    if let Some(user) = db.query_row(
        "SELECT d.user_control FROM ui_machine_bindings m JOIN ui_linux_desktops d
         ON d.chat=m.chat AND d.name=m.name WHERE m.chat=? AND d.selected=1",
        [chat], |row| row.get(0),
    ).optional()? { return Ok(Some(user)); }
    db.query_row("SELECT user_control FROM ui_x11_desktops WHERE chat=?", [chat], |row| row.get(0)).optional()
}

pub(crate) fn change_driver(db: &Connection, chat: &str, user: bool) -> io::Result<()> {
    crate::web_browser::clear_control(db, chat)?;
    db.execute("UPDATE ui_linux_desktops SET user_control=?,user_frame=NULL,agent_frame=NULL WHERE chat=?",
        params![user, chat]).map_err(io::Error::other)?;
    db.execute("UPDATE ui_x11_desktops SET user_control=?,user_frame=NULL,agent_frame=NULL WHERE chat=?",
        params![user, chat]).map_err(io::Error::other)?;
    Ok(())
}

pub(crate) fn validate(db: &Database, chat: &str, name: &str) -> io::Result<()> {
    if binding(&database(db), chat).map_err(io::Error::other)?.as_deref() != Some(name) {
        return Err(io::Error::other("The Docker execution target changed. Reopen the machine before running this task."));
    }
    Ok(())
}

pub(crate) fn busy(db: &Connection, chat: &str) -> rusqlite::Result<bool> {
    db.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM native_chat_turns WHERE chat_id=?1 AND status IN {}
          UNION ALL SELECT 1 FROM ui_jobs WHERE parent_chat=?1 AND state IN ('queued','running','waiting_input'))",
          crate::ui_turn_resume::LIVE_STATUSES),
        [chat], |row| row.get(0),
    )
}

const IMAGE: &str = "kageha-desktop:machine-v3";
const IMAGE_BUILD: &str = "docker build -f docker/desktop/Dockerfile -t kageha-desktop:machine-v3 .";

fn image() -> String {
    std::env::var("KAGEHA_DESKTOP_IMAGE").unwrap_or_else(|_| IMAGE.into())
}

fn check_image(image: &str) -> io::Result<()> {
    let version = docker(&["image", "inspect", "--format", r#"{{index .Config.Labels "kageha.desktop.version"}}"#, image], 5, &AtomicBool::new(false));
    if !version.is_ok_and(|value| value.trim_ascii() == b"3") {
        return Err(io::Error::other(format!("Docker image {image} is missing or incompatible. Build the workstation image with {IMAGE_BUILD}.")));
    }
    Ok(())
}

fn desktop_row(db: &Connection, chat: &str) -> rusqlite::Result<Option<(String, bool)>> {
    db.query_row("SELECT name,selected FROM ui_linux_desktops WHERE chat=?", [chat],
        |row| Ok((row.get(0)?, row.get(1)?))).optional()
}

pub(crate) fn private_directory(db: &Database, chat: &str, child: &str) -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let path = crate::ui_uploads::root(db, chat)?.join(child);
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    crate::browser_client::private(&path)?;
    if !std::fs::symlink_metadata(&path)?.is_dir() {
        return Err(io::Error::other("Invalid Docker workspace directory"));
    }
    Ok(path)
}

fn mount(path: &Path, target: &str) -> io::Result<String> {
    let source = path.to_str().filter(|s| !s.contains([',', '"', '\n', '\r']))
        .ok_or_else(|| io::Error::other("Docker mount path contains unsupported characters"))?;
    Ok(format!("type=bind,source={source},target={target}"))
}

fn container_status(name: &str) -> io::Result<String> {
    let status = docker(&["inspect", "--format", "{{.State.Status}}", name], 4, &AtomicBool::new(false))?;
    Ok(String::from_utf8_lossy(&status).trim().into())
}

fn container_version(name: &str) -> io::Result<String> {
    let result = docker(&["inspect","--format",r#"{{index .Config.Labels "kageha.desktop.version"}}"#,name],4,&AtomicBool::new(false))?;
    Ok(String::from_utf8_lossy(result.trim_ascii()).into_owned())
}

pub(crate) fn require_running(name: &str) -> io::Result<()> {
    if container_status(name)? != "running" {
        return Err(io::Error::other("The Docker workstation is stopped or paused. Reconnect or resume it before running a task."));
    }
    Ok(())
}

fn start(name: &str) -> io::Result<()> {
    match container_status(name)?.as_str() {
        "running" => Ok(()),
        "paused" => Err(io::Error::other("Resume the Docker workstation before starting a task")),
        _ => docker(&["start", name], 18, &AtomicBool::new(false)).map(|_| ()),
    }
}

fn wait_ready(name: &str) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        let health = docker(&["inspect", "--format", "{{.State.Health.Status}}", name], 4, &AtomicBool::new(false))?;
        match health.trim_ascii() {
            b"healthy" => return Ok(()),
            b"starting" => std::thread::sleep(Duration::from_millis(250)),
            _ => break,
        }
    }
    let readiness = services(name).unwrap_or_default();
    let failed: Vec<_> = ["display", "shell", "chromium"].into_iter().filter(|service| !readiness.iter().any(|ready| ready == service)).collect();
    Err(io::Error::other(format!("Docker workstation did not become ready. Unavailable services: {}. Check container logs and workspace permissions.", failed.join(", "))))
}

fn services(name: &str) -> io::Result<Vec<String>> {
    let profile = r#"xdotool getdisplaygeometry >/dev/null 2>&1 && printf 'display\n'
printf '%s\n' '{"op":"ping"}' | socat -T 1 -t 1 STDIO UNIX-CONNECT:/tmp/kageha-shell.sock 2>/dev/null | grep -q '"ok":true' && printf 'shell\n'
curl -fsS --max-time 2 http://127.0.0.1:9222/json/version >/dev/null 2>&1 && printf 'chromium\n'
exit 0"#;
    let output = docker(&["exec", name, "sh", "-c", profile], 4, &AtomicBool::new(false))?;
    Ok(String::from_utf8_lossy(&output).lines().map(str::to_owned).collect())
}

fn pending_machine(db: &Database, chat: &str) -> io::Result<String> {
    let conn = database(db);
    let name = format!("kageha-linux-{}", uuid::Uuid::new_v4().simple());
    conn.execute("INSERT OR IGNORE INTO ui_machine_provisions(chat,name) VALUES(?,?)", params![chat,name]).map_err(io::Error::other)?;
    conn.query_row("SELECT name FROM ui_machine_provisions WHERE chat=?", [chat], |row| row.get(0)).map_err(io::Error::other)
}

fn owned_pending(chat: &str, name: &str) -> io::Result<bool> {
    let filter = format!("name=^{name}$");
    let cancel = AtomicBool::new(false);
    let ids = docker(&["ps", "-aq", "--filter", &filter], 4, &cancel)?;
    if ids.trim_ascii().is_empty() { return Ok(false); }
    let owner = docker(&["inspect", "--format", r#"{{index .Config.Labels "kageha.desktop.chat"}}"#, name], 4, &cancel)?;
    if owner.trim_ascii() != chat.as_bytes() { return Err(io::Error::other("Docker container name is occupied by a different owner")); }
    Ok(true)
}

fn stop(name: &str) -> io::Result<()> {
    let cancel = AtomicBool::new(false);
    if container_status(name)? == "paused" {
        docker(&["unpause", name], 5, &cancel)?;
    }
    docker(&["stop", "--time", "3", name], 12, &cancel)?;
    crate::machine_io::release(name);
    Ok(())
}

fn new_machine(db: &Database, chat: &str, name: &str) -> io::Result<()> {
    let image = image();
    check_image(&image)?;
    let workspace = crate::ui_workspace::open_for_chat(db, chat)?;
    let shell_state = private_directory(db, chat, "machine-shell")?;
    // The guest shell opens the task workspace with a separate manifest and shell state.
    kageha_workspace::task::TaskWorkspace::open_with_state(workspace.root(), &shell_state)
        .map_err(io::Error::other)?;
    let transfer = private_directory(db, chat, "machine-browser-transfer")?;
    let work_mount = mount(workspace.root(), "/work")?;
    let state_mount = mount(&shell_state, "/machine-state")?;
    let transfer_mount = mount(&transfer, "/browser-transfer")?;
    let owner = format!("kageha.desktop.chat={chat}");
    let cancel = AtomicBool::new(false);
    let mut args = vec!["create", "--name", name, "--label", "kageha.native.desktop=1", "--label", &owner, "--init",
        "--restart=unless-stopped", "--cap-drop=ALL", "--security-opt=no-new-privileges",
        "--pids-limit=512", "--memory=2g", "--cpus=2", "--shm-size=512m",
        "--mount", &work_mount, "--mount", &state_mount, "--mount", &transfer_mount,
        "--tmpfs", "/work/.kageha:ro,mode=0555", "--workdir", "/work"];
    let user;
    let home;
    if cfg!(target_os = "linux") {
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        if uid == 0 { return Err(io::Error::other("Run Kageha as a non-root user to start a Docker workstation on Linux")); }
        user = format!("{uid}:{gid}");
        home = mount(&private_directory(db, chat, "machine-home")?, "/home/desktop")?;
        args.extend(["--user", user.as_str(), "--env", "HOME=/home/desktop", "--mount", home.as_str()]);
    }
    args.push(&image);
    docker(&args, 15, &cancel)?;
    Ok(())
}

/// Called while holding the chat lock, before a turn is admitted.
pub(crate) fn activate(db: &Database, chat: &str, upgrade: bool) -> io::Result<String> {
    let (existing, machine) = {
        let connection = database(db);
        if busy(&connection, chat).map_err(io::Error::other)? {
            return Err(io::Error::other("Stop the chat and its workers before changing the execution target"));
        }
        if crate::ui_session_lifecycle::status(&connection, chat).map_err(io::Error::other)?["status"] != "open" {
            return Err(io::Error::other("Open the chat before starting its Docker workstation"));
        }
        if connection.query_row("SELECT EXISTS(SELECT 1 FROM ui_jobs WHERE child_chat=?)",
            [chat], |row| row.get::<_, bool>(0)).map_err(io::Error::other)? {
            return Err(io::Error::other("Worker chats cannot select a Docker workstation"));
        }
        if crate::ui_store::get(&connection, "remote-session", chat).map_err(io::Error::other)?.is_some_and(|row| row["active"] == true) {
            return Err(io::Error::other("Return the chat from its SSH host before selecting Docker"));
        }
        let machine: Option<String> = connection.query_row(
            "SELECT name FROM ui_machine_bindings WHERE chat=?", [chat], |row| row.get(0)).optional().map_err(io::Error::other)?;
        (desktop_row(&connection, chat).map_err(io::Error::other)?, machine)
    };
    if let Some(name) = machine.filter(|name| !upgrade || container_version(name).is_ok_and(|version| version == "3")) {
        start(&name)?;
        wait_ready(&name)?;
        let connection = database(db);
        let was_selected = binding(&connection,chat).map_err(io::Error::other)?.is_some();
        if !was_selected {
            crate::web_browser::clear_control(&connection,chat)?;
            crate::machine_io::unregister(chat)?;
            connection.execute("DELETE FROM ui_x11_desktops WHERE chat=?",[chat]).map_err(io::Error::other)?;
            connection.execute("UPDATE ui_linux_desktops SET selected=1,user_frame=NULL,agent_frame=NULL WHERE chat=?", [chat]).map_err(io::Error::other)?;
        }
        return Ok(name);
    }
    if existing.is_some() && !upgrade {
        return Err(io::Error::other("This chat has a legacy Linux desktop. Choose Upgrade Linux desktop to use the full Docker workstation."));
    }
    let name = pending_machine(db, chat)?;
    if !owned_pending(chat, &name)? { new_machine(db, chat, &name)?; }
    let previous = existing.as_ref().map(|(name, _)| name.as_str());
    let committed: io::Result<()> = (|| {
        start(&name)?;
        wait_ready(&name)?;
        if let Some(previous) = previous {
            stop(previous)?;
        }
        let mut connection = database(db);
        let tx = connection.transaction().map_err(io::Error::other)?;
        tx.execute("INSERT OR IGNORE INTO ui_machine_backups(chat,name) SELECT chat,backup FROM ui_machine_bindings WHERE chat=? AND backup IS NOT NULL",[chat]).map_err(io::Error::other)?;
        tx.execute("INSERT INTO ui_machine_bindings(chat,name,backup) VALUES(?,?,?) ON CONFLICT(chat) DO UPDATE SET name=excluded.name,backup=excluded.backup", params![chat,name,previous]).map_err(io::Error::other)?;
        tx.execute("INSERT INTO ui_linux_desktops(chat,name,selected,agent_access) VALUES(?,?,1,1)
            ON CONFLICT(chat) DO UPDATE SET name=excluded.name,selected=1,user_control=0,
            agent_access=1,user_frame=NULL,agent_frame=NULL", params![chat,name]).map_err(io::Error::other)?;
        crate::web_browser::clear_control(&tx, chat)?;
        tx.execute("DELETE FROM ui_x11_desktops WHERE chat=?", [chat]).map_err(io::Error::other)?;
        tx.execute("DELETE FROM ui_machine_provisions WHERE chat=? AND name=?", params![chat,name]).map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
        Ok(())
    })();
    if committed.is_err() {
        if owned_pending(chat, &name).unwrap_or(false) {
            let _ = docker(&["rm", "-f", &name], 12, &AtomicBool::new(false));
        }
        if let Some((previous, true)) = existing {
            let _ = start(&previous);
        }
    }
    committed.map(|()| name)
}

/// Selected target and live readiness are separate. Reads never start Docker.
pub(crate) fn machine_state(db: &Database, chat: &str, update: Option<&Value>) -> io::Result<Value> {
    let _guard = lock(chat);
    if let Some(args) = update {
        if args["upgrade"] == true {
            activate(db, chat, true)?;
        } else if let Some(active) = args["active"].as_bool() {
            if active {
                activate(db, chat, false)?;
            } else {
                select(db, chat, "local_mac")?;
            }
        } else if let Some(paused) = args["paused"].as_bool() {
            if busy(&database(db), chat).map_err(io::Error::other)? {
                return Err(io::Error::other("Stop the chat before changing the Docker workstation state"));
            }
            let name = binding(&database(db), chat).map_err(io::Error::other)?.ok_or_else(|| io::Error::other("Select a Docker workstation first"))?;
            let state = container_status(&name)?;
            if (state == "paused") != paused {
                docker(&[if paused { "pause" } else { "unpause" }, &name], 8, &AtomicBool::new(false))?;
                database(db).execute("UPDATE ui_linux_desktops SET user_frame=NULL,agent_frame=NULL WHERE chat=?", [chat]).map_err(io::Error::other)?;
            }
        } else {
            return Err(io::Error::other("Supply active, paused, or upgrade"));
        }
    }
    let (name, existing, compatible) = {
        let connection = database(db);
        let exists = connection.query_row("SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)", [chat], |row| row.get::<_, bool>(0)).map_err(io::Error::other)?;
        if !exists {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Chat not found"));
        }
        let name = binding(&connection, chat).map_err(io::Error::other)?;
        let compatible = connection.query_row("SELECT EXISTS(SELECT 1 FROM ui_machine_bindings WHERE chat=?)", [chat], |row| row.get::<_, bool>(0)).map_err(io::Error::other)?;
        (name, desktop_row(&connection, chat).map_err(io::Error::other)?.map(|(name,_)| name), compatible)
    };
    let version = existing.as_deref().and_then(|name|container_version(name).ok());
    let upgrade_required = existing.is_some() && (!compatible || version.as_deref().is_some_and(|value|value!="3"));
    let Some(name) = name else {
        return Ok(json!({"available":installed(),"active":false,"connected":false,"upgrade_required":upgrade_required}));
    };
    let status = match container_status(&name) {
        Ok(status) => status,
        Err(error) => return Ok(json!({"available":installed(),"active":true,"connected":false,
            "blocked":true,"runtime":{"status":"error","error":error.to_string()}})),
    };
    let connected = status == "running";
    let readiness = if connected { services(&name).unwrap_or_default() } else { vec![] };
    let display = readiness.iter().any(|service| service == "display");
    let shell = readiness.iter().any(|service| service == "shell");
    let browser = readiness.iter().any(|service| service == "chromium") && crate::browser_client::configured();
    let ready = display && shell && browser;
    Ok(json!({"available":true,"active":true,"connected":connected,"provider":"docker_desktop","upgrade_required":upgrade_required,
        "blocked":!ready && status!="paused",
        "runtime":{"status":status,"paused":status=="paused","desktopReady":connected,
                   "shellReady":shell,"browserReady":browser,"desktopReady":display,
                   "error":if connected && !ready { Some(format!("Docker services not ready: display {display}, shell {shell}, browser {browser}")) } else { None },
                   "image":image(),"imageVersion":version,"networkEnabled":true},
        "watch":crate::machine_workspace::watch(db,chat)?}))
}

pub(crate) fn admin(db: &Database, chat: &str, command: &str) -> io::Result<Value> {
    let parts: Vec<_> = command.split_whitespace().collect();
    let message = match parts.as_slice() {
        ["/machine"] | ["/machine", "help" | "-h" | "--help" | "?"] =>
            "Use /machine <task> to run a task in Docker. Use /machine status, /machine doctor, or /machine stop.".into(),
        ["/machine", "status"] if chat.is_empty() =>
            if installed() { "Docker CLI is available. Select a chat to check its workstation.".into() }
            else { "Docker CLI is unavailable.".into() },
        ["/machine", "status"] => {
            let status = machine_state(db, chat, None)?;
            if status["upgrade_required"] == true {
                "The chat has a legacy Linux desktop. Use Upgrade Linux desktop in the execution menu.".into()
            } else if status["active"] == true {
                format!("Docker workstation is {}.", status["runtime"]["status"].as_str().unwrap_or("unavailable"))
            } else {
                "The Docker workstation is off for this chat.".into()
            }
        }
        ["/machine", "doctor"] => {
            let version = docker(&["version", "--format", "{{.Server.Version}}"], 6, &AtomicBool::new(false))?;
            check_image(&image())?;
            format!("Docker {} and workstation image {} are available. Rust browser service is {}.",
                String::from_utf8_lossy(version.trim_ascii()), image(),
                if crate::browser_client::configured() { "configured" } else { "unconfigured" })
        }
        ["/machine", "stop"] if !chat.is_empty() => {
            machine_state(db, chat, Some(&json!({"active":false})))?;
            "Docker workstation stopped. This chat uses local execution.".into()
        }
        ["/machine", "stop"] => return Err(io::Error::new(io::ErrorKind::InvalidInput, "Select a chat before stopping its workstation")),
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "Send a Docker task in chat as /machine <task>")),
    };
    Ok(json!({"ok":true,"message":message}))
}

pub(crate) fn computer_admin(db: &Database, chat: &str, command: &str) -> io::Result<Value> {
    let message: String = match command.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["/computer"] | ["/computer", "help" | "?" | "-h" | "--help"] =>
            "Open Computer to see the desktop for the selected execution target. Send /computer followed by a task, or use /computer status.".into(),
        ["/computer", "status" | "doctor"] if chat.is_empty() =>
            "Select a chat to check its desktop readiness.".into(),
        ["/computer", "status" | "doctor"] => {
            let _guard = lock(chat);
            let remote = crate::ui_remote::selection(db,chat).map_err(|(_,error)|io::Error::other(error))?;
            let state = if selected(db,chat)? { current(db,chat)?.state(chat) }
                else if let Some(desktop) = linux_desktop(db,chat,&remote,false)? { desktop.state(chat) }
                else if remote.is_null() { crate::computer_host::status(chat) }
                else { json!({"error":"The selected SSH host is configured for shell and files. Enable Remote desktop in Settings to use its Linux display."}) };
            if let Some(detail) = state["error"].as_str() { detail.into() }
            else if state["connected"] == true { "The selected desktop is connected.".into() }
            else if state["available"] == true { "Open Computer to start or reconnect the selected desktop.".into() }
            else { "Desktop control is unavailable for the selected execution target.".into() }
        }
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput,"Use /computer status, or send /computer with a desktop task")),
    };
    Ok(json!({"ok":true,"message":message}))
}

pub(crate) fn installed() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join("docker").is_file()))
}

fn docker(args: &[&str], seconds: u64, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    let control = Control {
        cancelled: cancel,
        deadline: Some(Instant::now() + Duration::from_secs(seconds)),
    };
    let out = kageha_workspace::process::run(
        Command::new("docker").args(args),
        vec![],
        &control,
        5 << 20,
    )?;
    if !out.success {
        return Err(io::Error::other(format!(
            "Docker {} failed. Check Docker Desktop and the chat's Linux container.",
            args[0]
        )));
    }
    Ok(out.bytes)
}

struct Desktop {
    name: String,
    user: bool,
    access: bool,
    session: Option<crate::linux_session::Session>,
}

fn current(db: &Database, chat: &str) -> io::Result<Desktop> {
    database(db).query_row("SELECT name,user_control,agent_access FROM ui_linux_desktops WHERE chat=? AND selected=1",[chat],
        |r|Ok(Desktop{name:r.get(0)?,user:r.get(1)?,access:r.get(2)?,session:None})).optional().map_err(io::Error::other)?
        .ok_or_else(||io::Error::other("The selected desktop changed. Refresh the Computer panel."))
}

impl Desktop {
    fn provider(&self) -> &'static str {
        self.session.as_ref().map(crate::linux_session::Session::provider).unwrap_or("docker_desktop")
    }

    fn table(&self) -> &'static str {
        if self.session.is_some() { "ui_x11_desktops" } else { "ui_linux_desktops" }
    }

    fn state(&self, chat: &str) -> Value {
        let (epoch, error) = match self.session.as_ref() {
            Some(session) => match session.running(false, &AtomicBool::new(false)) {
                Ok(running) => (Some(running.epoch), None),
                Err(_) => (None, Some("Start or reconnect the Linux desktop. Check the Linux session helper if it does not start.")),
            },
            None => (container_status(&self.name).ok().filter(|state| state == "running").and_then(|_| self.generation(&AtomicBool::new(false)).ok()), None),
        };
        json!({"provider":self.provider(),"runtime":"rust","app":"Linux Desktop","sessionId":chat,
            "available":true,"connected":epoch.is_some(),"previewAvailable":epoch.is_some(),"canControl":self.user,
            "serviceEpoch":epoch,"containerName":if self.session.is_none(){Some(self.name.as_str())}else{None},
            "lockedBy":if self.user {"user"} else {"agent"},"inputAuthorized":self.access,"error":error})
    }

    fn generation(&self, cancel: &AtomicBool) -> io::Result<String> {
        if let Some(session) = &self.session { return Ok(session.running(false,cancel)?.epoch); }
        let value = docker(&["inspect","--format","{{.Id}}/{{.State.StartedAt}}",&self.name], 4, cancel)?;
        Ok(String::from_utf8_lossy(value.trim_ascii()).into_owned())
    }
}

fn linux_desktop(db: &Database, chat: &str, remote: &Value, remember: bool) -> io::Result<Option<Desktop>> {
    if crate::linux_session::mode(remote) != Some("desktop") { return Ok(None); }
    let session = crate::linux_session::for_chat(db, chat, remote, remember)?.expect("desktop mode");
    let db = database(db);
    if remember {
        remember_display(&db, chat, &session.key)?;
    }
    let (user, access) = db.query_row("SELECT user_control,agent_access FROM ui_x11_desktops WHERE chat=? AND name=?", params![chat,session.key],
        |row| Ok((row.get(0)?,row.get(1)?))).optional().map_err(io::Error::other)?.unwrap_or((false,true));
    Ok(Some(Desktop { name:session.key.clone(),user,access,session:Some(session) }))
}

pub(crate) fn remember_display(db: &Connection, chat: &str, key: &str) -> io::Result<()> {
    db.execute("INSERT INTO ui_x11_desktops(chat,name) VALUES(?,?) ON CONFLICT(chat) DO UPDATE SET
                name=excluded.name,user_control=0,agent_access=1,user_frame=NULL,agent_frame=NULL WHERE name!=excluded.name",
        params![chat,key]).map_err(io::Error::other)?;
    Ok(())
}

fn select(db: &Database, chat: &str, provider: &str) -> io::Result<Value> {
    if provider == "local_mac" {
        if selected(db, chat)? {
            if busy(&database(db), chat).map_err(io::Error::other)? {
                return Err(io::Error::other("Stop the chat and its workers before changing the execution target"));
            }
            let desk = current(db, chat)?;
            stop(&desk.name)?;
            crate::machine_io::unregister(chat)?;
            let connection = database(db);
            change_driver(&connection, chat, false)?;
            connection.execute("UPDATE ui_linux_desktops SET selected=0 WHERE chat=?", [chat]).map_err(io::Error::other)?;
        }
        return Ok(crate::computer_host::status(chat));
    }
    if provider != "docker_desktop" {
        return Err(io::Error::other("Unknown computer provider"));
    }
    let existing = desktop_row(&database(db), chat).map_err(io::Error::other)?;
    let compatible = database(db).query_row("SELECT EXISTS(SELECT 1 FROM ui_machine_bindings WHERE chat=?)", [chat], |row| row.get::<_, bool>(0)).map_err(io::Error::other)?;
    if let Some((name, _)) = existing.filter(|_| !compatible) {
        {
            let connection = database(db);
            if busy(&connection, chat).map_err(io::Error::other)? || crate::ui_store::get(&connection, "remote-session", chat).map_err(io::Error::other)?.is_some_and(|row| row["active"] == true) {
                return Err(io::Error::other("Return the chat to local execution before changing its legacy desktop"));
            }
        }
        start(&name)?;
        database(db).execute("UPDATE ui_linux_desktops SET selected=1 WHERE chat=?", [chat]).map_err(io::Error::other)?;
    } else {
        activate(db, chat, false)?;
    }
    Ok(current(db, chat)?.state(chat))
}

fn capture(
    db: &Database,
    chat: &str,
    d: &Desktop,
    owner: &str,
    cancel: &AtomicBool,
) -> io::Result<Value> {
    let generation = if let Some(session) = &d.session { session.running(owner != "panel", cancel)?.epoch } else { d.generation(cancel)? };
    let bytes = if let Some(session) = &d.session { session.operation("frame", &generation, &[], cancel, 4 * 1024 * 1024)? } else { docker(
        &[
            "exec",
            &d.name,
            "sh",
            "-c",
            "scrot -o -z /tmp/kageha-frame.jpg && cat /tmp/kageha-frame.jpg",
        ],
        7,
        cancel,
    )? };
    let (width, height) = image::ImageReader::new(io::Cursor::new(&bytes))
        .with_guessed_format()?
        .into_dimensions()
        .map_err(io::Error::other)?;
    let frame = format!(
        "{}:{}:{}:{}:{}:{}",
        uuid::Uuid::new_v4(),
        crate::ui_store::now() as u64,
        width,
        height,
        owner,
        generation
    );
    // A poll can finish just before the browser sends input from its displayed frame.
    // Keep the displayed and arriving manual frames until either one is consumed.
    let assignments = if owner == "panel" {
        "user_frame=json_array(?,json_extract(user_frame,'$[0]'))"
    } else {
        "agent_frame=?"
    };
    database(db)
        .execute(&format!("UPDATE {} SET {assignments} WHERE chat=? AND name=?", d.table()), params![frame, chat,d.name])
        .map_err(io::Error::other)?;
    Ok(
        json!({"status":"ok","frame_id":frame,"observation_id":frame,"result":{"width":width,"height":height,"image":{"mime_type":"image/jpeg","data":data_encoding::BASE64.encode(&bytes)}}}),
    )
}

fn action(
    d: &Desktop,
    args: &Value,
    width: u32,
    height: u32,
    epoch: &str,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let invalid = || io::Error::other("Invalid Linux desktop action or arguments");
    let coord = |k: &str, limit: u32| {
        args[k]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v < f64::from(limit))
            .map(|v| (v as u32).to_string())
            .ok_or_else(invalid)
    };
    let mut words = vec![];
    match args["action"].as_str().unwrap_or("") {
        "terminal" => {
            if let Some(session) = &d.session { return session.operation("terminal", epoch, &[], cancel, 4096).map(|_| ()); }
            return docker(&["exec","-d",&d.name,"xterm","-T","Kageha Terminal"],10,cancel).map(|_| ());
        }
        "launch" => {
            let app = args["app"].as_str().filter(|name| name.len() <= 80
                && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte,b'_' | b'-' | b'.')))
                .ok_or_else(invalid)?;
            if let Some(session) = &d.session { return session.operation("launch", epoch, &[app], cancel, 4096).map(|_| ()); }
            return docker(&["exec","-d",&d.name,app],10,cancel).map(|_| ());
        }
        "click" => words.extend([
            "mousemove".into(),
            coord("x", width)?,
            coord("y", height)?,
            "click".into(),
            match args["button"].as_str().unwrap_or("left") {
                "left" => "1",
                "right" => "3",
                "middle" => "2",
                _ => return Err(invalid()),
            }
            .into(),
        ]),
        "drag" => words.extend([
            "mousemove".into(),
            coord("from_x", width)?,
            coord("from_y", height)?,
            "mousedown".into(),
            "1".into(),
            "mousemove".into(),
            coord("to_x", width)?,
            coord("to_y", height)?,
            "mouseup".into(),
            "1".into(),
        ]),
        "scroll" => words.extend([
            "click".into(),
            "--repeat".into(),
            args["amount"]
                .as_u64()
                .unwrap_or(1)
                .clamp(1, 12)
                .to_string(),
            match args["direction"].as_str() {
                Some("up") => "4",
                Some("down") => "5",
                Some("left") => "6",
                Some("right") => "7",
                _ => return Err(invalid()),
            }
            .into(),
        ]),
        "type" => words.extend([
            "type".into(),
            "--clearmodifiers".into(),
            "--delay".into(),
            "0".into(),
            "--".into(),
            args["text"]
                .as_str()
                .filter(|s| s.len() <= 20000)
                .ok_or_else(invalid)?
                .into(),
        ]),
        "key" => {
            let key = args["key"]
                .as_str()
                .filter(|s| {
                    !s.is_empty()
                        && s.len() <= 64
                        && s.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'_'))
                })
                .ok_or_else(invalid)?;
            let keys = key
                .split('+')
                .map(|k| match k {
                    "cmd" => "ctrl",
                    "return" => "Return",
                    "escape" => "Escape",
                    "backspace" => "BackSpace",
                    "tab" => "Tab",
                    "left" => "Left",
                    "right" => "Right",
                    "up" => "Up",
                    "down" => "Down",
                    "delete" => "Delete",
                    "pageup" => "Prior",
                    "pagedown" => "Next",
                    "home" => "Home",
                    "end" => "End",
                    other => other,
                })
                .collect::<Vec<_>>();
            words.extend(["key".into(), "--clearmodifiers".into(), keys.join("+")]);
        }
        _ => return Err(invalid()),
    }
    if let Some(session) = &d.session {
        let words: Vec<_> = words.iter().map(String::as_str).collect();
        return session.operation("input", epoch, &words, cancel, 4096).map(|_| ());
    }
    let mut cmd = vec!["exec", &d.name, "xdotool"];
    cmd.extend(words.iter().map(String::as_str));
    docker(&cmd, 10, cancel).map(|_| ())
}

fn input(
    db: &Database,
    chat: &str,
    d: &Desktop,
    owner: &str,
    frame: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> io::Result<Value> {
    let panel = owner == "panel";
    if panel != d.user || (!panel && !d.access) {
        return Err(io::Error::other(
            "Control belongs to another driver or agent access is disabled",
        ));
    }
    let parts: Vec<_> = frame.splitn(6, ':').collect();
    if parts.len() != 6
        || parts[4] != owner
        || (crate::ui_store::now() as u64).saturating_sub(parts[1].parse().unwrap_or(0)) > 30
        || parts[5] != d.generation(cancel)?
    {
        return Err(io::Error::other(
            "Observe the Linux desktop again before input",
        ));
    }
    let predicate = if panel {
        "EXISTS(SELECT 1 FROM json_each(user_frame) WHERE value=?)"
    } else {
        "agent_frame=?"
    };
    let n = database(db).execute(&format!("UPDATE {} SET user_frame=NULL,agent_frame=NULL WHERE chat=? AND name=? AND {predicate}",d.table()),params![chat,d.name,frame]).map_err(io::Error::other)?;
    if n != 1 {
        return Err(io::Error::other(
            "The desktop frame changed or was already used. Observe again.",
        ));
    }
    action(
        d,
        args,
        parts[2].parse().unwrap_or(0),
        parts[3].parse().unwrap_or(0),
        parts[5],
        cancel,
    )?;
    Ok(json!({"status":"ok","provider":d.provider()}))
}

pub(crate) fn handle(mut request: Request, db: &Database, chat: &str, op: &str) -> io::Result<()> {
    let _guard = lock(chat);
    let expected = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Kageha-Computer"))
        .map(|h| h.value.as_str().to_owned())
        .unwrap_or_default();
    let remote = crate::ui_remote::selection(db, chat).map_err(|(_, error)| io::Error::other(error))?;
    let docker_selected = selected(db, chat)?;
    let linux = !docker_selected && crate::linux_session::mode(&remote) == Some("desktop");
    if !remote.is_null() && !linux {
        let error = "Desktop control is unavailable on the selected SSH destination";
        if request.method() == &Method::Get && op == "state" {
            crate::server::response(request, 200, json!({"provider":"ssh","available":false,"connected":false,"error":error}));
        } else {
            crate::server::response(request, 409, json!({"error":error}));
        }
        return Ok(());
    }
    if op != "provider" && !docker_selected && !linux {
        if matches!(expected.as_str(), "docker_desktop" | "linux_host" | "ssh_desktop") {
            crate::server::response(
                request,
                409,
                json!({"error":"The Linux desktop is no longer selected"}),
            );
            return Ok(());
        }
        drop(_guard);
        return crate::computer_host::handle(request, db, chat, op);
    }
    let result = (|| {
        if op == "provider" && request.method() == &Method::Post {
            let body = crate::webui::body(&mut request)?;
            if !remote.is_null() { return Err(io::Error::other("Change the execution target before selecting another desktop")); }
            if body["provider"] == "linux_host" || body["provider"] == "local_mac" && crate::linux_session::local_mode() == Some("desktop") {
                select(db, chat, "local_mac")?;
                return Ok(linux_desktop(db, chat, &remote, false)?.ok_or_else(||io::Error::other("Linux host desktop is disabled"))?.state(chat));
            }
            return select(db, chat, body["provider"].as_str().unwrap_or(""));
        }
        let d = if linux { linux_desktop(db, chat, &remote, request.method() == &Method::Post)?.expect("desktop mode") } else { current(db, chat)? };
        if !expected.is_empty() && expected != d.provider() {
            return Err(io::Error::other("Desktop selection changed"));
        }
        match (request.method().clone(), op) {
            (Method::Get, "state") => Ok(d.state(chat)),
            (Method::Get, "preview") => capture(db, chat, &d, "panel", &AtomicBool::new(false)),
            (Method::Post, "lease" | "access") => {
                let body = crate::webui::body(&mut request)?;
                let (column, on) = if op == "access" {
                    ("agent_access", body["allow"].as_bool())
                } else {
                    (
                        "user_control",
                        match body["action"].as_str() {
                            Some("take") => Some(true),
                            Some("return") => Some(false),
                            _ => None,
                        },
                    )
                };
                let on = on.ok_or_else(|| io::Error::other("Invalid desktop control change"))?;
                {
                    let connection = database(db);
                    if column == "user_control" {
                        crate::web_browser::clear_control(&connection, chat)?;
                        connection.execute(&format!("UPDATE {} SET user_control=?,user_frame=NULL,agent_frame=NULL WHERE chat=? AND name=?",d.table()),
                            params![on,chat,d.name]).map_err(io::Error::other)?;
                    } else {
                        connection.execute(&format!("UPDATE {} SET agent_access=?,user_frame=NULL,agent_frame=NULL WHERE chat=? AND name=?",d.table()),
                            params![on,chat,d.name]).map_err(io::Error::other)?;
                    }
                }
                let refreshed = if linux { linux_desktop(db,chat,&remote,false)?.expect("desktop mode") } else { current(db,chat)? };
                Ok(refreshed.state(chat))
            }
            (Method::Post, "action") => {
                let body = crate::webui::body(&mut request)?;
                input(
                    db,
                    chat,
                    &d,
                    "panel",
                    body["frame_id"].as_str().unwrap_or(""),
                    &body,
                    &AtomicBool::new(false),
                )
            }
            (Method::Post, "refresh") => {
                if let Some(session) = &d.session { session.running(true,&AtomicBool::new(false))?; }
                else { start(&d.name)?; crate::machine_io::release(&d.name); }
                crate::machine_io::unregister(chat)?;
                let connection = database(db);
                crate::web_browser::clear_control(&connection,chat)?;
                connection.execute(&format!("UPDATE {} SET user_frame=NULL,agent_frame=NULL WHERE chat=?",d.table()), [chat]).map_err(io::Error::other)?;
                drop(connection);
                Ok(d.state(chat))
            },
            _ => Err(io::Error::other(
                "Unsupported operation on the Linux desktop",
            )),
        }
    })();
    match result {
        Ok(v) => crate::server::response(request, 200, v),
        Err(e) => crate::server::response(request, 409, json!({"error":e.to_string()})),
    }
    Ok(())
}

pub(crate) fn remove(c: &Connection, chat: &str) -> io::Result<()> {
    crate::machine_io::unregister(chat)?;
    let name: Option<String> = c
        .query_row(
            "SELECT name FROM ui_linux_desktops WHERE chat=?",
            [chat],
            |r| r.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    let backup: Option<String> = c.query_row("SELECT backup FROM ui_machine_bindings WHERE chat=?", [chat],
        |row| row.get(0)).optional().map_err(io::Error::other)?.flatten();
    if let Some(pending) = c.query_row("SELECT name FROM ui_machine_provisions WHERE chat=?", [chat], |row| row.get::<_, String>(0)).optional().map_err(io::Error::other)? {
        if owned_pending(chat, &pending)? {
            docker(&["rm", "-f", &pending], 12, &AtomicBool::new(false))?;
        }
        c.execute("DELETE FROM ui_machine_provisions WHERE chat=?", [chat]).map_err(io::Error::other)?;
    }
    let historical = {
        let mut query = c.prepare("SELECT name FROM ui_machine_backups WHERE chat=?").map_err(io::Error::other)?;
        let rows = query.query_map([chat],|row| row.get::<_, String>(0)).map_err(io::Error::other)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(io::Error::other)?
    };
    for name in [name, backup].into_iter().flatten().chain(historical) {
        crate::machine_io::release(&name);
        let cancel = AtomicBool::new(false);
        let ids = docker(
            &["ps", "-aq", "--filter", &format!("name=^{name}$")],
            5,
            &cancel,
        )?;
        if !ids.is_empty() {
            docker(&["rm", "-f", &name], 12, &cancel)?;
        }
    }
    c.execute("DELETE FROM ui_machine_bindings WHERE chat=?", [chat]).map_err(io::Error::other)?;
    c.execute("DELETE FROM ui_machine_backups WHERE chat=?", [chat]).map_err(io::Error::other)?;
    c.execute("DELETE FROM ui_x11_desktops WHERE chat=?", [chat]).map_err(io::Error::other)?;
    c.execute("DELETE FROM ui_linux_desktops WHERE chat=?", [chat]).map_err(io::Error::other)?;
    Ok(())
}

pub(crate) fn tool(host: &crate::ui_tools::Tools<'_>, call: &Value) -> io::Result<String> {
    let (db, chat, turn) = (host.db, host.chat, host.turn);
    let guard = lock(chat);
    let workstation = || -> io::Result<Desktop> {
        if host.machine.is_some() || selected(db,chat)? { current(db,chat) }
        else { linux_desktop(db,chat,&host.remote,true)?.ok_or_else(|| io::Error::other("Linux desktop is unavailable")) }
    };
    let d = workstation()?;
    let args = &call["arguments"];
    let result=match call["name"].as_str(){
        Some("computer_info"|"computer_permissions")=>d.state(chat),
        Some("computer_observe"|"computer_inspect")=>{
            let mut v=capture(db,chat,&d,turn,host.cancel)?;
            let image=v["result"].as_object_mut().unwrap().remove("image").unwrap();
            let bytes = data_encoding::BASE64.decode(image["data"].as_str().unwrap_or("").as_bytes()).map_err(io::Error::other)?;
            let route = crate::ui_vision::route(db, host.providers.first(), host.role_providers.planner.first(), host.media_preferences["vision"].as_str());
            drop(guard);
            v["screen"] = json!(crate::ui_vision::with_vision(route, host.cancel, |vision| Ok(crate::ui_vision::read_image(db, "Linux desktop.jpg", &bytes, vision, None, &host.attachments)))?);
            v
        }
        Some("computer_execute")=>{
            if d.user || !d.access {return Err(io::Error::other("Agent control is disabled on this Linux desktop; use the Computer panel to enable it or return control."));}
            let frame=args["observation_id"].as_str().unwrap_or("");
            let mut action_args=args["args"].clone();
            if !action_args.is_object(){return Err(io::Error::other("Action args must be an object"));}
            action_args["action"]=json!(match args["method"].as_str(){Some("type_text")=>"type",Some("press_key")=>"key",Some(name)=>name,None=>""});
            let scope=json!({"provider":d.provider(),"session":d.name,"observation_id":frame,"action":action_args});
            drop(guard);
            if !crate::ui_approvals::require(db,chat,turn,"computer_execute","Send input to the observed Linux desktop",&scope,host.cancel)? {return Err(io::Error::other("Linux desktop input declined"));}
            let _guard=lock(chat);
            let latest=workstation()?;
            crate::ui_effects::run(db,turn,call,&scope,|| {
                let mut result = input(db,chat,&latest,turn,frame,&action_args,host.cancel)?;
                result["receipt"] = json!({"state":"verified","request_id":call["id"],"action":"computer_input",
                    "provider":latest.provider(),"target":latest.name,"observation_id":frame});
                Ok(result)
            })?
        }
        _=>return Err(io::Error::other("The Computer panel is using the Linux desktop. Use computer_observe for its current screen.")),
    };
    Ok(result.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Run against the same image as the panel; never touch an existing chat container.
    #[test]
    #[ignore = "requires a local Docker engine and desktop image"]
    fn linux_desktop_lifecycle_and_frame_ownership() {
        let chat = "linux-display-test";
        let fixture = crate::audited_session_tests::Fixture::for_chat(chat);
        let db = &fixture.db;
        struct Cleanup<'a>(&'a Database, &'a str);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = remove(&database(self.0), self.1);
            }
        }
        let _cleanup = Cleanup(db, chat);
        let cancel = AtomicBool::new(false);
        select(db, chat, "docker_desktop").unwrap();
        let desk = current(db, chat).unwrap();
        let mut shot = None;
        for _ in 0..40 {
            if let Ok(frame) = capture(db, chat, &desk, "panel", &cancel) {
                shot = Some(frame);
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let frame = shot.unwrap();
        assert_eq!(frame["result"]["width"], 1440);
        let id = frame["frame_id"].as_str().unwrap();
        let key = json!({"action":"key","key":"ctrl+l"});
        assert!(input(db, chat, &desk, "panel", id, &key, &cancel).is_err());
        database(db)
            .execute(
                "UPDATE ui_linux_desktops SET user_control=1 WHERE chat=?",
                [chat],
            )
            .unwrap();
        let manual = current(db, chat).unwrap();
        capture(db, chat, &manual, "panel", &cancel).unwrap();
        input(db, chat, &manual, "panel", id, &key, &cancel).unwrap();
        assert!(input(db, chat, &manual, "panel", id, &key, &cancel).is_err());
        select(db, chat, "local_mac").unwrap();
        assert!(!selected(db, chat).unwrap());
        select(db, chat, "docker_desktop").unwrap();
        assert_eq!(current(db, chat).unwrap().name, desk.name);
    }
}
