//! Terminal presentation of the same session used by the web application.
use crate::{identifier, read_token, Packet, SessionClient};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
const HELP:&str="Kageha shared chat\n\nUsage: kageha-chat [--url URL] [--token-file FILE] [--session ID | --new]\n       [--message TEXT | --stdin | --interactive | --follow]\n       [--request-id ID] [--after-sequence N] [--json]\n       [--open-computer | --panel-url] [--ui-url URL]\n\nThe CLI uses the existing Rust server and the UI's session, tools and approvals.\nWithout --session it creates a new shared chat. Pipe a prompt or use --message.\nA terminal starts interactive chat. --follow is read-only; --request-id follows\nthat existing request without resending it. Ctrl-C/quit detaches, not cancels.\n\nInteractive commands:\n  /computer [open|status|take|agent|enable|disable]\n  /approve ID | /deny ID     Decide the exact current approval, once\n  /history | /session | /help | /quit\n\nAuth: private --token-file, KAGEHA_SERVER_TOKEN_FILE, or KAGEHA_SERVER_TOKEN.\nServer: --url or KAGEHA_SERVER_URL (default http://127.0.0.1:8788).\nUI: --ui-url or KAGEHA_UI_URL; links never contain a token.\nOnly read-only event reconnection is automatic. Mutations are never retried.\n";
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(262144)
        .collect()
}
#[derive(Default)]
struct Options {
    url: String,
    ui: Option<String>,
    token_file: Option<String>,
    session: Option<String>,
    message: Option<String>,
    request: Option<String>,
    after: Option<u64>,
    new: bool,
    stdin: bool,
    interactive: bool,
    follow: bool,
    json: bool,
    open: bool,
    panel: bool,
    help: bool,
}
impl Options {
    fn parse(args: Vec<String>) -> io::Result<Self> {
        let mut o = Self {
            url: std::env::var("KAGEHA_SERVER_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8788".into()),
            ui: std::env::var("KAGEHA_UI_URL").ok(),
            token_file: std::env::var("KAGEHA_SERVER_TOKEN_FILE").ok(),
            ..Default::default()
        };
        let mut args = args.into_iter();
        let mut seen = std::collections::HashSet::new();
        while let Some(arg) = args.next() {
            if !seen.insert(arg.clone()) {
                return Err(io::Error::other("Duplicate option"));
            }
            let mut value = || {
                args.next()
                    .ok_or_else(|| io::Error::other(format!("Missing value for {arg}")))
            };
            match arg.as_str() {
                "--url" => o.url = value()?,
                "--ui-url" => o.ui = Some(value()?),
                "--token-file" => o.token_file = Some(value()?),
                "--session" => o.session = Some(value()?),
                "--message" => o.message = Some(value()?),
                "--request-id" => o.request = Some(value()?),
                "--after-sequence" => {
                    o.after = Some(
                        value()?
                            .parse()
                            .map_err(|_| io::Error::other("Invalid cursor"))?,
                    )
                }
                "--new" => o.new = true,
                "--stdin" => o.stdin = true,
                "--interactive" => o.interactive = true,
                "--follow" => o.follow = true,
                "--json" => o.json = true,
                "--open-computer" => o.open = true,
                "--panel-url" => o.panel = true,
                "--help" | "-h" => o.help = true,
                _ => {
                    return Err(io::Error::other(format!(
                        "Unknown option {arg}. Use --help."
                    )))
                }
            }
        }
        if o.help {
            return Ok(o);
        }
        if o.new && o.session.is_some()
            || o.message.is_some() && o.stdin
            || o.follow && (o.message.is_some() || o.stdin || o.new || o.interactive)
            || o.after.is_some_and(|v| v > i64::MAX as u64)
        {
            return Err(io::Error::other("Conflicting options"));
        }
        if let Some(id) = &o.session {
            identifier(id)?;
        }
        if let Some(id) = &o.request {
            identifier(id)?;
        }
        if o.follow && o.session.is_none() {
            return Err(io::Error::other(
                "--follow requires --session; it never creates or starts work",
            ));
        }
        if o.request.is_some() && o.message.is_none() && !o.stdin && o.session.is_none() {
            return Err(io::Error::other("Following a request requires --session"));
        }
        Ok(o)
    }
}
fn output(text: &str) {
    let mut stdout = io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{text}").and_then(|_| stdout.flush()) {
        // A closed output consumer detaches; it must not cancel or retry work.
        std::process::exit(if error.kind() == io::ErrorKind::BrokenPipe {
            0
        } else {
            1
        });
    }
}
fn emit(value: Value, json_output: bool) {
    let text = if json_output {
        Some(value.to_string())
    } else {
        match value["type"].as_str() {
            Some("session") => Some(format!(
                "Session: {}\nComputer: {}",
                value["session_id"].as_str().unwrap_or(""),
                value["computer_url"].as_str().unwrap_or("")
            )),
            Some("request") => Some(format!(
                "Request: {}",
                value["request_id"].as_str().unwrap_or("")
            )),
            Some("notice") => Some(clean(value["message"].as_str().unwrap_or(""))),
            Some("snapshot") => None,
            Some("event") => render_frame(&value["frame"]),
            _ => Some(clean(&value.to_string())),
        }
    };
    if let Some(text) = text {
        output(&text);
    }
}
fn render_frame(frame: &Value) -> Option<String> {
    let payload = &frame["payload"];
    match frame["kind"].as_str() {
        Some("tool_card") => {
            let card = &frame["tool_card"];
            Some(format!(
                "[{}] {} ({})",
                clean(card["status"].as_str().unwrap_or("tool")),
                clean(card["name"].as_str().unwrap_or("tool")),
                clean(card["id"].as_str().unwrap_or(""))
            ))
        }
        Some("approval_required") => Some(format!(
            "Approval required: {}\n/approve {} or /deny {}",
            clean(payload["detail"].as_str().unwrap_or("Review this action")),
            clean(payload["approval_id"].as_str().unwrap_or("")),
            clean(payload["approval_id"].as_str().unwrap_or(""))
        )),
        Some("done") => Some(format!(
            "{}\n[{}]",
            clean(payload["message"].as_str().unwrap_or("")),
            clean(payload["status"].as_str().unwrap_or("finished"))
        )),
        Some("status") => Some(clean(frame["label"].as_str().unwrap_or("Working…"))),
        _ => None,
    }
}
fn open_panel(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("/usr/bin/open")
        .arg(url)
        .status()?;
    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("xdg-open").arg(url).status()?;
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .status()?;
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err(io::Error::other(
        "Open the printed Computer link in your browser",
    ));
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    if !status.success() {
        return Err(io::Error::other("Could not open the Computer link"));
    }
    Ok(())
}
enum Input {
    Line(String),
    Packet(Packet),
    Notice(String),
    Failure(String),
    Eof,
}
struct Stop(Arc<AtomicBool>);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst)
    }
}
fn feed(
    client: SessionClient,
    session: String,
    after: Option<u64>,
    tx: mpsc::SyncSender<Input>,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut cursor = after.unwrap_or(0);
        let mut discard_history = after.is_none();
        let mut backoff = 250;
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let result = (|| -> io::Result<()> {
                let mut stream = client.feed(&session, cursor)?;
                loop {
                    let mut packet = stream.next_packet()?;
                    if stop.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    if packet.event == "snapshot" && discard_history {
                        cursor = packet.data["sequence"].as_u64().ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "Missing snapshot cursor")
                        })?;
                        discard_history = false;
                    }
                    if packet.event == "update" {
                        if let Some(frames) = packet.data["events"].as_array_mut() {
                            frames.retain(|frame| {
                                frame["sequence"].as_u64().is_some_and(|v| v > cursor)
                            });
                            if frames.is_empty() {
                                continue;
                            }
                            cursor = frames.last().unwrap()["sequence"].as_u64().unwrap();
                        }
                    }
                    if tx.send(Input::Packet(packet)).is_err() {
                        return Ok(());
                    }
                    backoff = 250;
                }
            })();
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if let Err(error) = result {
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::PermissionDenied
                ) {
                    let _ = tx.send(Input::Failure(error.to_string()));
                    return;
                }
                if tx.send(Input::Notice(format!("Event connection interrupted; reconnecting read-only after sequence {cursor}. No input is being resent."))).is_err(){return}
            }
            std::thread::sleep(Duration::from_millis(backoff));
            backoff = (backoff * 2).min(2000);
        }
    });
}
fn successful_status(status: &str) -> bool {
    matches!(status, "done" | "completed" | "success")
}
fn terminal_status(status: &str) -> bool {
    matches!(
        status,
        "done"
            | "completed"
            | "success"
            | "failed"
            | "cancelled"
            | "interrupted"
            | "stalled"
            | "error"
            | "unverified"
    )
}
fn snapshot_result(packet: &Value, turn: &str) -> Option<Value> {
    let live = &packet["live"];
    let record = live["turns"]
        .as_array()?
        .iter()
        .find(|v| v["turn_id"] == turn)?;
    if !terminal_status(record["status"].as_str().unwrap_or("")) {
        return None;
    }
    let answer = live["messages"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .rev()
                .find(|v| v["turn_id"] == turn && v["role"] == "assistant")
        })
        .map(|v| v["text"].clone())
        .unwrap_or(json!(""));
    Some(
        json!({"kind":"done","turn_id":turn,"payload":{"status":record["status"],"message":answer}}),
    )
}
fn history(client: &SessionClient, session: &str, json_output: bool) -> io::Result<()> {
    let snapshot = client.session(session)?;
    for message in snapshot["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        if json_output {
            emit(json!({"type":"history","message":message}), true)
        } else {
            output(&format!(
                "{}: {}",
                clean(message["role"].as_str().unwrap_or("")),
                clean(message["text"].as_str().unwrap_or(""))
            ))
        }
    }
    Ok(())
}
fn send(
    client: &SessionClient,
    session: &str,
    message: &str,
    request: Option<&str>,
    json_output: bool,
) -> io::Result<(String, u64)> {
    let id = request
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    emit(
        json!({"type":"request","session_id":session,"request_id":id}),
        json_output,
    );
    let admitted = client.admit(session, &id, message).map_err(|error| {
        io::Error::other(format!(
            "{error} Follow without resending: --session {session} --request-id {id} --follow"
        ))
    })?;
    let turn = identifier(
        admitted["turn_id"]
            .as_str()
            .ok_or_else(|| io::Error::other("Missing admitted turn"))?,
    )?
    .to_owned();
    let cursor = admitted["after_sequence"].as_u64().ok_or_else(|| {
        io::Error::other("Missing admission cursor; follow the original request without resending")
    })?;
    Ok((turn, cursor))
}
pub fn run(args: Vec<String>) -> io::Result<()> {
    let mut options = Options::parse(args)?;
    if options.help {
        output(HELP);
        return Ok(());
    }
    let token = if let Some(path) = &options.token_file {
        read_token(std::path::Path::new(path))?
    } else {
        std::env::var("KAGEHA_SERVER_TOKEN").map_err(|_|io::Error::other("Set KAGEHA_SERVER_TOKEN_FILE or pass --token-file. The CLI does not start a separate local agent."))?
    };
    let client = SessionClient::new(&options.url, &token)?;
    let session = if let Some(session) = options.session.clone() {
        client.session(&session)?;
        session
    } else {
        client.create()?
    };
    let panel = client.panel_url(&session, options.ui.as_deref())?;
    emit(
        json!({"type":"session","session_id":session,"computer_url":panel}),
        options.json,
    );
    if options.open {
        open_panel(&panel)?;
    }
    let interactive = options.interactive
        || io::stdin().is_terminal()
            && options.message.is_none()
            && !options.stdin
            && !options.follow
            && !options.panel
            && !options.open;
    if options.stdin
        || !io::stdin().is_terminal()
            && !interactive
            && options.message.is_none()
            && !options.follow
            && !options.panel
            && !options.open
            && options.request.is_none()
    {
        let mut prompt = String::new();
        io::stdin().take(262145).read_to_string(&mut prompt)?;
        options.message = Some(prompt);
    }
    let mut waiting = if interactive && options.message.is_none() && options.request.is_none() {
        client.session(&session)?["active_turn"]["turn_id"]
            .as_str()
            .map(str::to_owned)
    } else {
        None
    };
    let mut last_approval: Option<String> = None;
    let mut cursor = options.after;
    if let Some(message) = &options.message {
        let (turn, after) = send(
            &client,
            &session,
            message,
            options.request.as_deref(),
            options.json,
        )?;
        waiting = Some(turn);
        cursor = Some(after);
    } else if let Some(request) = &options.request {
        let receipt = client.receipt(&session, request)?;
        waiting = Some(
            identifier(
                receipt["turn"]["turn_id"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("Request has no turn"))?,
            )?
            .into(),
        );
        cursor = Some(cursor.unwrap_or(0));
    }
    if !interactive && !options.follow && waiting.is_none() {
        return Ok(());
    }
    if interactive {
        emit(
            json!({"type":"notice","message":"Shared session attached. /computer opens its live panel. /help lists controls; /quit detaches without stopping work."}),
            options.json,
        );
        history(&client, &session, options.json)?;
    }
    let (tx, rx) = mpsc::sync_channel(16);
    let stop = Stop(Arc::new(AtomicBool::new(false)));
    feed(
        client.clone(),
        session.clone(),
        cursor,
        tx.clone(),
        stop.0.clone(),
    );
    if interactive {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut reader = io::stdin().lock();
            loop {
                let mut line = String::new();
                let result = Read::by_ref(&mut reader).take(262145).read_line(&mut line);
                match result {
                    Ok(0) => break,
                    Ok(_) if line.len() <= 262144 => {
                        if tx.send(Input::Line(line)).is_err() {
                            return;
                        }
                    }
                    _ => {
                        let _ = tx.send(Input::Failure(
                            "Input line exceeds 262144 bytes or is invalid UTF-8".into(),
                        ));
                        return;
                    }
                }
            }
            let _ = tx.send(Input::Eof);
        });
    }
    drop(tx);
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Input::Packet(packet)) => {
                let frames = if packet.event == "snapshot" {
                    if interactive && waiting.is_none() {
                        waiting = packet.data["live"]["active_turn"]["turn_id"]
                            .as_str()
                            .map(str::to_owned);
                    }
                    if let Some(id) =
                        packet.data["live"]["pending_approval"]["approval_id"].as_str()
                    {
                        if last_approval.as_deref() != Some(id) {
                            emit(
                                json!({"type":"notice","message":format!("Approval required: {}\n/approve {} or /deny {}", clean(packet.data["live"]["pending_approval"]["detail"].as_str().unwrap_or("Review the pending action")),clean(id),clean(id))}),
                                options.json,
                            );
                            last_approval = Some(id.into());
                        }
                    }
                    emit(
                        json!({"type":"snapshot","session_id":session,"snapshot":packet.data}),
                        options.json,
                    );
                    waiting
                        .as_ref()
                        .and_then(|turn| snapshot_result(&packet.data, turn))
                        .into_iter()
                        .collect::<Vec<_>>()
                } else {
                    packet.data["events"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                };
                for frame in frames {
                    if frame["kind"] == "approval_required" {
                        last_approval = frame["payload"]["approval_id"].as_str().map(str::to_owned);
                    }
                    if interactive && waiting.is_none() && frame["kind"] == "status" {
                        waiting = frame["turn_id"].as_str().map(str::to_owned);
                    }
                    let done = frame["kind"] == "done"
                        && waiting
                            .as_ref()
                            .is_some_and(|turn| frame["turn_id"] == turn.as_str());
                    emit(
                        json!({"type":"event","session_id":session,"frame":frame}),
                        options.json,
                    );
                    if done {
                        waiting = None;
                        if !interactive {
                            return if successful_status(
                                frame["payload"]["status"].as_str().unwrap_or(""),
                            ) {
                                Ok(())
                            } else {
                                Err(io::Error::other("Shared turn did not complete successfully; inspect it in the UI"))
                            };
                        }
                    }
                }
            }
            Ok(Input::Line(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "/quit" {
                    return Ok(());
                }
                let result = (|| -> io::Result<()> {
                    if line == "/help" {
                        emit(json!({"type":"notice","message":HELP}), options.json);
                    } else if line == "/history" {
                        history(&client, &session, options.json)?;
                    } else if line == "/session" {
                        emit(
                            json!({"type":"session","session_id":session,"computer_url":panel}),
                            options.json,
                        );
                    } else if let Some(id) = line.strip_prefix("/approve ") {
                        client.approval(&session, id.trim(), true)?;
                        emit(
                            json!({"type":"notice","message":"Approved this action once."}),
                            options.json,
                        );
                    } else if let Some(id) = line.strip_prefix("/deny ") {
                        client.approval(&session, id.trim(), false)?;
                        emit(
                            json!({"type":"notice","message":"Denied this action."}),
                            options.json,
                        );
                    } else if line == "/computer" || line.starts_with("/computer ") {
                        let action = line.strip_prefix("/computer").unwrap().trim();
                        let state = match action {
                            "" | "open" => {
                                open_panel(&panel)?;
                                json!({"computer_url":panel})
                            }
                            "status" => client.computer(&session, "state", None)?,
                            "take" => client.computer(
                                &session,
                                "lease",
                                Some(&json!({"action":"take"})),
                            )?,
                            "agent" => client.computer(
                                &session,
                                "lease",
                                Some(&json!({"action":"return"})),
                            )?,
                            "enable" => {
                                client.computer(&session, "access", Some(&json!({"allow":true})))?
                            }
                            "disable" => client.computer(
                                &session,
                                "access",
                                Some(&json!({"allow":false})),
                            )?,
                            _ => {
                                return Err(io::Error::other(
                                    "Use /computer open, status, take, agent, enable or disable",
                                ))
                            }
                        };
                        emit(json!({"type":"computer","state":state}), options.json);
                    } else if line.starts_with('/') {
                        return Err(io::Error::other("Unknown command. Use /help."));
                    } else {
                        if waiting.is_some() {
                            return Err(io::Error::other("This CLI's turn is still running. Approve it or use the Computer panel; no second turn was sent."));
                        }
                        waiting = Some(send(&client, &session, line, None, options.json)?.0);
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    emit(
                        json!({"type":"notice","message":error.to_string()}),
                        options.json,
                    );
                }
            }
            Ok(Input::Notice(message)) => {
                emit(json!({"type":"notice","message":message}), options.json)
            }
            Ok(Input::Failure(message)) => return Err(io::Error::other(message)),
            Ok(Input::Eof) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err(io::Error::other("Session feed stopped")),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_only_follow_requires_existing_session() {
        assert!(Options::parse(vec!["--follow".into()]).is_err());
        assert!(Options::parse(vec!["--follow".into(), "--session".into(), "chat".into()]).is_ok());
        assert!(Options::parse(vec![
            "--follow".into(),
            "--session".into(),
            "chat".into(),
            "--interactive".into(),
        ])
        .is_err());
    }
    #[test]
    fn terminal_output_removes_escape_sequences() {
        assert!(!clean("\u{1b}[31mtest\u{7}").contains('\u{1b}'));
    }
    #[test]
    fn completed_snapshot_returns_only_requested_answer() {
        let packet = json!({"live":{"turns":[{"turn_id":"first","status":"done"}],"messages":[{"turn_id":"first","role":"assistant","text":"original"},{"turn_id":"other","role":"assistant","text":"different"}]}});
        assert_eq!(
            snapshot_result(&packet, "first").unwrap()["payload"]["message"],
            "original"
        );
    }
    #[test]
    fn webui_success_and_unverified_snapshots_are_terminal() {
        for status in ["success", "unverified"] {
            let packet = json!({"live":{"turns":[{"turn_id":"turn","status":status}],"messages":[{"turn_id":"turn","role":"assistant","text":"saved answer"}]}});
            assert_eq!(
                snapshot_result(&packet, "turn").unwrap()["payload"]["status"],
                status
            );
        }
        assert!(successful_status("success"));
        assert!(!successful_status("unverified"));
    }
}
