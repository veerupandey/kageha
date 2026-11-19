//! Retries and chain fallback of one model call (design section 10, Requirements 10.3
//! and 14.5), against a scripted local HTTP server. Waits are injected and recorded,
//! except in the cancellation test.
use kageha_contracts::execution::{model_failure, Control, ModelFailureKind, ModelPort, ModelRequest, Role};
use kageha_models::{HttpModel, ModelActivity, Provider};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Answers one connection per scripted response, in order, and returns the `model`
/// field of each request it received.
fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut models = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            models.push(body["model"].as_str().unwrap_or_default().to_owned());
            socket.write_all(response.as_bytes()).unwrap();
        }
        models
    });
    (url, server)
}

fn reply(status: &str, headers: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
}
fn ok(text: &str) -> String {
    let body = json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string();
    reply("200 OK", "", "application/json", &body)
}
fn error(status: &str, headers: &str) -> String {
    reply(status, headers, "application/json", r#"{"error":{"code":"temporary"}}"#)
}

/// A chain of `models` on the scripted server, with recorded activity and waits.
struct Chain {
    model: HttpModel,
    events: Arc<Mutex<Vec<ModelActivity>>>,
    waits: Arc<Mutex<Vec<Duration>>>,
}
fn chain(url: &str, models: &[&str], record_waits: bool) -> Chain {
    let events = Arc::new(Mutex::new(Vec::new()));
    let waits = Arc::new(Mutex::new(Vec::new()));
    let (observed, recorded) = (events.clone(), waits.clone());
    let providers = models
        .iter()
        .map(|name| Provider { url: url.into(), key: String::new(), model: (*name).into(), protocol: "openai_compat".into() })
        .collect();
    let mut model = HttpModel::new(providers)
        .unwrap()
        .with_activity(Some(Arc::new(move |event| observed.lock().unwrap().push(event))));
    if record_waits {
        model = model.with_wait(Arc::new(move |delay, control: &Control<'_>| {
            recorded.lock().unwrap().push(delay);
            control.check()
        }));
    }
    Chain { model, events, waits }
}
fn ask(model: &HttpModel, role: Role, cancelled: &AtomicBool) -> io::Result<Value> {
    let request = ModelRequest { messages: vec![json!({"role":"user","content":"Continue"})], tools: vec![], role };
    // No call deadline: a wait longer than the time left would be skipped for the next model.
    let control = Control { cancelled, deadline: None };
    model.complete(request, &control).map(|reply| reply.content)
}
fn fallbacks(events: &Mutex<Vec<ModelActivity>>) -> Vec<(Role, String, String, &'static str)> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ModelActivity::Fallback { role, from, to, cause } => Some((*role, from.clone(), to.clone(), *cause)),
            _ => None,
        })
        .collect()
}
fn within(delay: Duration, base: u64) -> bool {
    let base = Duration::from_secs(base);
    delay >= base && delay <= base + base / 4
}

#[test]
fn server_errors_retry_the_same_model_with_backoff() {
    let (url, server) = serve(vec![error("503 Unavailable", ""), error("502 Bad Gateway", ""), ok("Recovered")]);
    let chain = chain(&url, &["primary", "backup"], true);
    let reply = ask(&chain.model, Role::Executor, &AtomicBool::new(false)).unwrap();
    assert_eq!(reply, "Recovered");
    assert_eq!(server.join().unwrap(), ["primary"; 3]);
    let waits = chain.waits.lock().unwrap().clone();
    assert!(waits.len() == 2 && within(waits[0], 1) && within(waits[1], 4), "{waits:?}");
    assert!(fallbacks(&chain.events).is_empty());
}

#[test]
fn rate_limits_honor_retry_after_then_fall_back_along_the_chain() {
    let (url, server) = serve(vec![
        error("429 Too Many Requests", "Retry-After: 30\r\n"),
        error("429 Too Many Requests", "Retry-After: 2\r\n"),
        error("429 Too Many Requests", "Retry-After: 600\r\n"),
        error("429 Too Many Requests", "Retry-After: 30\r\n"),
        ok("From backup"),
    ]);
    let chain = chain(&url, &["primary", "backup"], true);
    let reply = ask(&chain.model, Role::Planner, &AtomicBool::new(false)).unwrap();
    assert_eq!(reply, "From backup");
    assert_eq!(server.join().unwrap(), ["primary", "primary", "primary", "primary", "backup"]);
    // Retry-After when it is longer than the backoff, capped at 60 s.
    let waits = chain.waits.lock().unwrap().clone();
    assert_eq!(waits.len(), 3, "{waits:?}");
    assert_eq!(waits[0], Duration::from_secs(30));
    assert!(within(waits[1], 4), "{waits:?}");
    assert_eq!(waits[2], Duration::from_secs(60));
    assert_eq!(fallbacks(&chain.events), [(Role::Planner, "primary".into(), "backup".into(), "rate_limited")]);
}

#[test]
fn authentication_failure_falls_back_at_once() {
    let (url, server) = serve(vec![error("401 Unauthorized", ""), ok("From backup")]);
    let chain = chain(&url, &["primary", "backup"], true);
    assert_eq!(ask(&chain.model, Role::Reviewer, &AtomicBool::new(false)).unwrap(), "From backup");
    assert_eq!(server.join().unwrap(), ["primary", "backup"]);
    assert!(chain.waits.lock().unwrap().is_empty());
    assert_eq!(fallbacks(&chain.events), [(Role::Reviewer, "primary".into(), "backup".into(), "auth")]);
}

#[test]
fn a_partially_streamed_tool_call_is_not_retried_or_sent_elsewhere() {
    let stream = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call\",\"function\":{\"name\":\"write\",\"arguments\":\"{\"}}]}}]}\n\n";
    let (url, server) = serve(vec![reply("200 OK", "", "text/event-stream", stream)]);
    let chain = chain(&url, &["primary", "backup"], true);
    let error = ask(&chain.model, Role::Executor, &AtomicBool::new(false)).unwrap_err();
    let failure = model_failure(&error).unwrap();
    assert_eq!(failure.kind, ModelFailureKind::InterruptedStream);
    assert!(failure.partial_tool_calls);
    assert_eq!(server.join().unwrap(), ["primary"]);
    assert!(chain.waits.lock().unwrap().is_empty() && fallbacks(&chain.events).is_empty());
}

#[test]
fn a_context_limit_surfaces_typed_for_compaction() {
    let body = r#"{"error":{"code":"context_length_exceeded"}}"#;
    let (url, server) = serve(vec![reply("400 Bad Request", "", "application/json", body)]);
    let chain = chain(&url, &["primary", "backup"], true);
    let error = ask(&chain.model, Role::Executor, &AtomicBool::new(false)).unwrap_err();
    assert_eq!(model_failure(&error).unwrap().kind, ModelFailureKind::ContextLimit);
    assert_eq!(server.join().unwrap(), ["primary"]);
    assert!(fallbacks(&chain.events).is_empty());
}

#[test]
fn cancellation_stops_the_wait_between_attempts() {
    let (url, server) = serve(vec![error("503 Unavailable", "Retry-After: 30\r\n")]);
    let chain = chain(&url, &["primary", "backup"], false);
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Release);
    });
    let started = Instant::now();
    let error = ask(&chain.model, Role::Executor, &cancelled).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(started.elapsed() < Duration::from_secs(5));
    canceller.join().unwrap();
    assert_eq!(server.join().unwrap(), ["primary"]);
    assert!(chain.events.lock().unwrap().iter().any(|event| matches!(event, ModelActivity::Backoff { seconds: 30 })));
    assert!(fallbacks(&chain.events).is_empty());
}
