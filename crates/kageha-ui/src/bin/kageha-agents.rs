//! Agent tree, inspection and explicit controls over the shared WebUI API.
use kageha_ui::agents_terminal::{clean, now, terminal, AgentsView};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const HELP:&str="Kageha agents\n\nUsage: kageha-agents --session ID --token-file FILE [--url URL]\n       [--follow | --interactive] [--plain]\n       [--agent ID --inspect | --message TEXT | --cancel | --resume --confirm]\n       [--agent ID --approve APPROVAL_ID | --deny APPROVAL_ID]\n\nInteractive commands:\n  list                       Show agent tree\n  use NUMBER|ID              Select an agent\n  inspect [NUMBER|ID]        Read its assignment, tools and result\n  message TEXT               Steer the selected agent at its next checkpoint\n  cancel [NUMBER|ID]         Request cancellation\n  resume [NUMBER|ID]         Show safe resume requirements\n  resume confirm            Confirm a new attempt for the selected agent\n  approve ID | deny ID      Decide one exact pending approval, once\n  back | help | quit\n\nFollowing/inspection never starts work. Controls are sent only on explicit input.\nCtrl-C or quit exits without stopping agents. Piped output never contains redraws.\nNO_COLOR, TERM=dumb and --plain also disable redraws. Mutations are never retried.\n";
#[derive(Clone)]
struct Client {
    base: String,
    session: String,
    token: Arc<String>,
    http: reqwest::blocking::Client,
}
impl Client {
    fn new(url: &str, session: String, file: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let meta = std::fs::symlink_metadata(file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("Token file must be private (chmod 600)".into());
            }
        }
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 8192 {
            return Err("Invalid token file".into());
        }
        let token = std::fs::read_to_string(file)?.trim().to_owned();
        if token.is_empty() || token.chars().any(char::is_whitespace) {
            return Err("Invalid token".into());
        }
        let parsed = reqwest::Url::parse(url)?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !matches!(parsed.scheme(), "http" | "https")
            || parsed.scheme() == "http"
                && !matches!(
                    parsed.host_str(),
                    Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
                )
        {
            return Err("Use HTTPS outside loopback".into());
        }
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(45))
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            base: url.trim_end_matches('/').into(),
            session,
            token: Arc::new(token),
            http,
        })
    }
    fn request(&self, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let request = if let Some(body) = body {
            self.http.post(format!("{}{path}", self.base)).json(body)
        } else {
            self.http.get(format!("{}{path}", self.base))
        }
        .bearer_auth(self.token.as_str());
        let response=request.send().map_err(|_|if body.is_some(){"Control outcome is unknown. Inspect the original agent before retrying; no command was repeated.".to_owned()}else{"Agent status is unavailable. No command was sent.".into()})?;
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read agent response".to_owned())?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Agent response exceeds limit".into());
        }
        let packet: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid agent response".to_owned())?;
        if !status.is_success() {
            return Err(format!(
                "{}: {}",
                status,
                clean(packet["error"].as_str().unwrap_or("Control rejected"), 512)
            ));
        }
        Ok(packet)
    }
    fn refresh(&self, view: &mut AgentsView) -> Result<(), String> {
        let rows = self.request(&format!("/api/jobs?session_id={}", self.session), None)?;
        view.apply("snapshot", &json!({"workers":rows["jobs"]}));
        Ok(())
    }
    fn control(
        &self,
        view: &AgentsView,
        id: &str,
        action: &Action,
        confirmed: bool,
    ) -> Result<String, String> {
        if !valid_id(id) {
            return Err("Invalid agent identity".into());
        }
        let worker = view
            .workers
            .get(id)
            .ok_or("Select an agent in this session first")?;
        let state = worker["state"].as_str().unwrap_or("");
        let turn = worker["progress"]["turn_id"].clone();
        let revision = worker["revision"].clone();
        if !matches!(action, Action::Inspect) && turn.as_str().is_none_or(|id| !valid_id(id)) {
            return Err("A current agent turn is required before sending controls. Refresh and inspect the agent.".into());
        }
        if matches!(action, Action::Resume) && revision.as_u64().is_none() {
            return Err("A current worker revision is required for safe resume.".into());
        }
        let (route, payload) = match action {
            Action::Message(text) => {
                if text.trim().is_empty() || text.len() > 16384 {
                    return Err("Message must contain 1 to 16384 bytes".into());
                }
                if terminal(state) {
                    return Err(
                        "This agent is stopped; inspect its result or resume safely first.".into(),
                    );
                }
                (
                    "message",
                    json!({"message":text,"message_id":request_id(),"expected_turn_id":turn}),
                )
            }
            Action::Cancel => {
                if terminal(state) {
                    return Err("This agent is already stopped.".into());
                }
                ("cancel", json!({"expected_turn_id":turn}))
            }
            Action::Resume => {
                if worker["can_resume"] != true && worker["resume"]["allowed"] != true {
                    return Err(clean(
                        worker["resume_blocker"]
                            .as_str()
                            .or_else(|| worker["resume"]["reason"].as_str())
                            .unwrap_or(
                                "Safe resume is unavailable. Inspect the original receipts.",
                            ),
                        512,
                    ));
                }
                if !confirmed {
                    return Err("Resume requires explicit confirmation. Use `resume confirm` or --resume --confirm. A new attempt may perform new work.".into());
                }
                (
                    "resume",
                    json!({"confirmed":true,"client_request_id":request_id(),"expected_turn_id":turn,"expected_revision":revision}),
                )
            }
            Action::Approval { approval, approved } => {
                let current = worker["question"]["approval_id"]
                    .as_str()
                    .or_else(|| worker["error"]["approval_id"].as_str());
                if current != Some(approval.as_str()) {
                    return Err("The approval changed or no longer belongs to this agent. Inspect it again.".into());
                }
                (
                    "approve",
                    json!({"approved":approved,"approval_id":approval,"scope":"once","expected_turn_id":turn}),
                )
            }
            Action::Inspect => return view.inspect(id, 100).ok_or("Agent unavailable".into()),
        };
        let result = self.request(&format!("/api/jobs/{id}/{route}"), Some(&payload))?;
        if result["expired"] == true {
            return Err("This approval expired; no new permission was granted.".into());
        }
        Ok(match action {
            Action::Message(_) => "Message queued for the next checkpoint.",
            Action::Cancel => "Cancellation requested. Waiting for confirmed state.",
            Action::Resume => "Resume admitted. Watching the new attempt.",
            Action::Approval { approved: true, .. } => "Approved this request once.",
            Action::Approval {
                approved: false, ..
            } => "Denied this request.",
            _ => "Control accepted.",
        }
        .into())
    }
}
fn request_id() -> String {
    format!(
        "cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
#[derive(Clone)]
enum Action {
    Inspect,
    Message(String),
    Cancel,
    Resume,
    Approval { approval: String, approved: bool },
}
enum Input {
    Packet(String, Value),
    Command(String),
    Error(String),
    Closed,
}
fn stream(client: Client, after: u64, send: mpsc::SyncSender<Input>, stop: Arc<AtomicBool>) {
    let mut cursor = after;
    let mut failures = 0;
    while !stop.load(Ordering::Relaxed) {
        let endpoint = format!(
            "{}/api/sessions/{}/live/stream",
            client.base, client.session
        );
        let response = client
            .http
            .get(endpoint)
            .query(&[("after_sequence", cursor)])
            .bearer_auth(client.token.as_str())
            .send();
        match response {
            Ok(response) => {
                if !response.status().is_success() {
                    let _ = send.send(Input::Error(format!(
                        "Agent feed returned {}. Check session and access token.",
                        response.status()
                    )));
                    return;
                }
                let mut reader = BufReader::new(response);
                let mut line = String::new();
                let mut event = String::new();
                let mut data = String::new();
                while !stop.load(Ordering::Relaxed) {
                    line.clear();
                    match reader
                        .by_ref()
                        .take(8 * 1024 * 1024 + 1)
                        .read_line(&mut line)
                    {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line.len() > 8 * 1024 * 1024 => {
                            let _ = send.send(Input::Error("Agent event exceeds limit".into()));
                            return;
                        }
                        _ => {}
                    }
                    if line.trim().is_empty() {
                        if !data.is_empty() {
                            let packet: Value = match serde_json::from_str(&data) {
                                Ok(v) => v,
                                Err(_) => {
                                    let _ = send.send(Input::Error("Invalid agent event".into()));
                                    return;
                                }
                            };
                            let next = packet["events"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|f| f["sequence"].as_u64())
                                .max()
                                .unwrap_or(cursor);
                            if send.send(Input::Packet(event.clone(), packet)).is_err() {
                                return;
                            }
                            cursor = cursor.max(next);
                            failures = 0;
                        }
                        event.clear();
                        data.clear();
                    } else if let Some(value) = line.strip_prefix("event:") {
                        event = value.trim().into()
                    } else if let Some(value) = line.strip_prefix("data:") {
                        if data.len() + value.len() > 8 * 1024 * 1024 {
                            let _ = send.send(Input::Error("Agent event exceeds limit".into()));
                            return;
                        }
                        if !data.is_empty() {
                            data.push('\n')
                        }
                        data.push_str(value.trim());
                    }
                }
            }
            Err(_) => {}
        }
        failures += 1;
        if failures >= 8 {
            let _ = send.send(Input::Error(
                "Agent feed disconnected. Reconnect to resume updates; agents keep running.".into(),
            ));
            return;
        }
        if send
            .send(Input::Error("Reconnecting to saved agent updates…".into()))
            .is_err()
        {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn selected(
    view: &AgentsView,
    selection: &Option<String>,
    query: Option<&str>,
) -> Result<String, String> {
    query
        .and_then(|q| view.resolve(q))
        .or_else(|| {
            if query.is_none() {
                selection.clone().filter(|id| view.workers.contains_key(id))
            } else {
                None
            }
        })
        .ok_or("Select an agent with `use NUMBER` or --agent ID first.".into())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut url = "http://127.0.0.1:8788".to_owned();
    let mut session = None;
    let mut token_file = None;
    let mut follow = false;
    let mut interactive = false;
    let mut plain = false;
    let mut selection = None;
    let mut action = None;
    let mut confirmed = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => url = args.next().ok_or("--url requires a value")?,
            "--session" => session = args.next(),
            "--token-file" => token_file = args.next(),
            "--follow" => follow = true,
            "--interactive" => {
                interactive = true;
                follow = true
            }
            "--plain" => plain = true,
            "--agent" => selection = args.next(),
            "--confirm" => confirmed = true,
            "--inspect" => {
                if action.is_some() {
                    return Err("Choose one control action".into());
                }
                action = Some(Action::Inspect)
            }
            "--message" => {
                if action.is_some() {
                    return Err("Choose one control action".into());
                }
                action = Some(Action::Message(
                    args.next().ok_or("--message requires text")?,
                ))
            }
            "--cancel" => {
                if action.is_some() {
                    return Err("Choose one control action".into());
                }
                action = Some(Action::Cancel)
            }
            "--resume" => {
                if action.is_some() {
                    return Err("Choose one control action".into());
                }
                action = Some(Action::Resume)
            }
            "--approve" | "--deny" => {
                if action.is_some() {
                    return Err("Choose one control action".into());
                }
                action = Some(Action::Approval {
                    approval: args.next().ok_or("Approval ID required")?,
                    approved: arg == "--approve",
                })
            }
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            _ => return Err("Unknown argument; use --help".into()),
        }
    }
    let session = session
        .filter(|s: &String| valid_id(s))
        .ok_or("A valid --session is required")?;
    let file = token_file.ok_or("--token-file is required")?;
    let client = Client::new(&url, session, &file)?;
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(88);
    let redraw = io::stdout().is_terminal()
        && !interactive
        && !plain
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").unwrap_or_default() != "dumb";
    let mut view = AgentsView::default();
    if let Some(action) = &action {
        client.refresh(&mut view)?;
        let id = selected(&view, &selection, selection.as_deref())?;
        println!("{}", client.control(&view, &id, action, confirmed)?);
        if !follow {
            return Ok(());
        }
        selection = Some(id);
    }
    let (send, receive) = mpsc::sync_channel(128);
    let stop = Arc::new(AtomicBool::new(false));
    let feed_client = client.clone();
    let feed_send = send.clone();
    let feed_stop = stop.clone();
    std::thread::spawn(move || stream(feed_client, 0, feed_send, feed_stop));
    if interactive {
        println!("Kageha agents · interactive\nType help for commands. Select an agent before sending controls.\nUpdates never submit work. quit leaves agents running.\n");
        let input_send = send.clone();
        std::thread::spawn(move || {
            let mut reader = io::stdin().lock();
            loop {
                let mut line = String::new();
                match reader.by_ref().take(16386).read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = input_send.send(Input::Closed);
                        break;
                    }
                    Ok(_) if line.len() > 16384 => {
                        let _ = input_send.send(Input::Error("Command exceeds 16 KiB".into()));
                        break;
                    }
                    _ => {
                        if input_send
                            .send(Input::Command(line.trim().to_owned()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
    }
    drop(send);
    let mut lines = 0;
    let mut last_render = String::new();
    let mut ready = false;
    loop {
        let input = match receive.recv_timeout(Duration::from_secs(1)) {
            Ok(input) => input,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let mut changed = false;
        match input {
            Input::Packet(event, packet) => {
                changed = view.apply(&event, &packet);
                if event == "snapshot" {
                    ready = true;
                    if let Some(query) = selection.clone() {
                        selection = view.resolve(&query);
                    }
                }
                if !follow && event == "snapshot" {
                    print!(
                        "{}",
                        view.render_selected(width, selection.as_deref(), now())
                    );
                    stop.store(true, Ordering::Relaxed);
                    return Ok(());
                }
            }
            Input::Error(message) => {
                eprintln!("{}", clean(&message, 1024));
                if message.contains("returned")
                    || message.contains("disconnected")
                    || message.contains("exceeds")
                    || message.contains("Invalid agent")
                {
                    stop.store(true, Ordering::Relaxed);
                    return Err(message.into());
                }
            }
            Input::Closed => {
                stop.store(true, Ordering::Relaxed);
                return Ok(());
            }
            Input::Command(command) => {
                if command.is_empty() {
                    continue;
                }
                let (verb, rest) = command.split_once(' ').unwrap_or((&command, ""));
                let rest = rest.trim();
                if matches!(verb, "quit" | "exit" | "q") {
                    stop.store(true, Ordering::Relaxed);
                    return Ok(());
                }
                if verb == "help" {
                    println!("{HELP}");
                    continue;
                }
                if !ready {
                    eprintln!("Waiting for the initial agent snapshot. No command was sent.");
                    continue;
                }
                if verb == "list" {
                    changed = true;
                    last_render.clear();
                } else if verb == "back" {
                    selection = None;
                    changed = true;
                } else if matches!(verb, "use" | "select") {
                    match view.resolve(rest) {
                        Some(id) => {
                            selection = Some(id);
                            changed = true
                        }
                        None => eprintln!(
                            "Agent not found or ID is ambiguous. Use list to see numbers."
                        ),
                    }
                } else {
                    // Refresh authority/status just before explicit controls. Do not replay a failed command.
                    if let Err(error) = client.refresh(&mut view) {
                        eprintln!("{error}");
                        continue;
                    }
                    let requested = if matches!(verb, "inspect" | "cancel" | "stop" | "resume")
                        && !rest.is_empty()
                        && rest != "confirm"
                    {
                        Some(rest)
                    } else {
                        None
                    };
                    let id = match selected(&view, &selection, requested) {
                        Ok(id) => id,
                        Err(error) => {
                            eprintln!("{error}");
                            continue;
                        }
                    };
                    selection = Some(id.clone());
                    let command_action = match verb {
                        "inspect" => Action::Inspect,
                        "message" | "steer" => Action::Message(rest.into()),
                        "cancel" | "stop" => Action::Cancel,
                        "resume" => Action::Resume,
                        "approve" | "deny" => Action::Approval {
                            approval: rest.into(),
                            approved: verb == "approve",
                        },
                        _ => {
                            eprintln!("Unknown command. Type help.");
                            continue;
                        }
                    };
                    match client.control(
                        &view,
                        &id,
                        &command_action,
                        verb == "resume" && rest == "confirm",
                    ) {
                        Ok(message) => println!("\n{message}\n"),
                        Err(error) => eprintln!("{}", clean(&error, 2048)),
                    }
                    changed = true;
                    last_render.clear();
                }
            }
        }
        if changed {
            let text = view.render_selected(width, selection.as_deref(), now());
            if text != last_render {
                let mut out = io::stdout().lock();
                if redraw && lines > 0 {
                    write!(out, "\x1b[{lines}A\x1b[J")?
                }
                write!(out, "{text}")?;
                if interactive {
                    writeln!(
                        out,
                        "Commands: use · inspect · message · cancel · resume · approve · quit"
                    )?;
                }
                out.flush()?;
                lines = text.lines().count();
                last_render = text;
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    Ok(())
}
