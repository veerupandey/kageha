//! `GET /api/learning/metrics`, served by `server::handle` with its token check.
use crate::audited_session_tests::Fixture;
use kageha_learning::{iso_week, LearningPort, RequestSummary, SqliteLearning};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TOKEN: &str = "learning-metrics-test-token";
const WEEK: u64 = 7 * 24 * 3600 * 1000;

struct Api {
    server: Arc<tiny_http::Server>,
    _workspace: tempfile::TempDir,
}

impl Api {
    fn start(fixture: &Fixture) -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let (incoming, db, root) = (server.clone(), fixture.db.clone(), workspace.path().to_path_buf());
        let controls: crate::webui::Controls = Arc::new(Mutex::new(Default::default()));
        std::thread::spawn(move || {
            for request in incoming.incoming_requests() {
                let _ = crate::server::handle(request, &db, &controls, &root, TOKEN);
            }
        });
        Self { server, _workspace: workspace }
    }

    /// Sends `method path` with `token` (if any) and returns the status and JSON body.
    fn request(&self, method: &str, path: &str, token: Option<&str>) -> (u16, Value) {
        let address = self.server.server_addr().to_ip().unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let authorization = token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Content-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.split(' ').nth(1).unwrap().parse().unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn installed(fixture: &Fixture) -> (tempfile::TempDir, Arc<SqliteLearning>) {
    let directory = tempfile::tempdir().unwrap();
    crate::ui_learning::install(&fixture.db, &directory.path().join("learning.sqlite")).unwrap();
    (directory, crate::ui_learning::store(&fixture.db).unwrap())
}

fn close(store: &SqliteLearning, request: &str, task_type: &str, status: &str, calls: u64, cost: f64, cached: u64, ended: u64) {
    store.close_request(request, RequestSummary {
        agent: "main".into(), chat_id: "chat".into(), task_type: Some(task_type.into()), status: status.into(),
        model_calls: calls, cost_usd: cost, cached_tokens: cached, outcomes: Vec::new(), started: ended - 1000, ended,
    }).unwrap();
}

#[test]
fn the_metrics_route_requires_the_token() {
    let fixture = Fixture::new();
    let api = Api::start(&fixture);
    assert_eq!(api.request("GET", "/api/learning/metrics", None), (401, json!({"error":"auth_required"})));
    assert_eq!(api.request("GET", "/api/learning/metrics", Some("wrong")).0, 401);
    // Without a learning store the route answers, with nothing to report.
    let (status, disabled) = api.request("GET", "/api/learning/metrics", Some(TOKEN));
    assert_eq!(status, 200);
    assert_eq!((disabled["enabled"].as_bool(), disabled["weeks"].as_u64()), (Some(false), Some(12)));
    assert_eq!(disabled["metrics"], json!([]));
    assert_eq!(disabled["speed_metrics"], json!([]));
}

#[test]
fn the_metrics_route_reports_per_task_type_and_week() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    let now = now_ms();
    close(&store, "a", "pdf-reports", "verified", 4, 0.5, 100, now);
    close(&store, "b", "pdf-reports", "partial", 8, 1.5, 300, now);
    close(&store, "c", "emails", "verified", 2, 0.0, 0, now);
    // Twenty weeks ago: outside the default 12 weeks.
    close(&store, "d", "pdf-reports", "verified", 1, 0.0, 0, now - 20 * WEEK);
    let api = Api::start(&fixture);
    let week = iso_week(now);

    let (status, body) = api.request("GET", "/api/learning/metrics", Some(TOKEN));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["weeks"], 12);
    let metrics = body["metrics"].as_array().unwrap();
    assert_eq!(metrics.len(), 2, "{body}");
    assert_eq!(metrics[0]["task_type"], "emails");
    let pdf = &metrics[1];
    assert_eq!((pdf["task_type"].as_str(), pdf["week"].as_str()), (Some("pdf-reports"), Some(week.as_str())));
    assert_eq!((pdf["requests"].as_u64(), pdf["verified"].as_u64()), (Some(2), Some(1)));
    assert_eq!(pdf["verified_rate"], 0.5);
    assert_eq!(pdf["mean_model_calls"], 6.0);
    assert_eq!(pdf["mean_cost_usd"], 1.0);
    assert_eq!(pdf["mean_cached_tokens"], 200.0);
    let speed = body["speed_metrics"].as_array().unwrap().iter()
        .find(|item| item["task_type"] == "pdf-reports").unwrap();
    assert_eq!((speed["agent"].as_str(), speed["requests"].as_u64(), speed["elapsed_samples"].as_u64()), (Some("main"), Some(2), Some(1)));
    assert_eq!(speed["median_elapsed_ms"], 1000.0);
    assert_eq!(speed["p95_elapsed_ms"], 1000);
    assert_eq!(body["cost_available"], false);

    // `task_type` filters and `weeks` widens the window.
    let (status, body) = api.request("GET", "/api/learning/metrics?task_type=pdf-reports&weeks=104", Some(TOKEN));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["task_type"], "pdf-reports");
    let weeks: Vec<_> = body["metrics"].as_array().unwrap().iter().map(|m| m["week"].as_str().unwrap().to_owned()).collect();
    assert_eq!(weeks, [iso_week(now - 20 * WEEK), week]);

    for bad in ["weeks=0", "weeks=105", "weeks=many", "task_type=PDF%20reports"] {
        assert_eq!(api.request("GET", &format!("/api/learning/metrics?{bad}"), Some(TOKEN)).0, 400, "{bad}");
    }
    assert_eq!(api.request("POST", "/api/learning/metrics", Some(TOKEN)).0, 405);
}
