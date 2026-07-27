use kageha_contracts::execution::Control;
use kageha_integrations::{Config, HttpClient};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
fn server(response: Vec<u8>) -> (String, mpsc::Receiver<Vec<u8>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    let join = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let end = loop {
            let mut byte = [0];
            if stream.read(&mut byte).unwrap() == 0 {
                return;
            }
            bytes.push(byte[0]);
            if bytes.ends_with(b"\r\n\r\n") {
                break bytes.len();
            }
        };
        let header = std::str::from_utf8(&bytes).unwrap();
        let n = header
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        bytes.resize(end + n, 0);
        stream.read_exact(&mut bytes[end..]).unwrap();
        let _ = tx.send(bytes);
        let _ = stream.write_all(&response);
    });
    (url, rx, join)
}
fn control<'a>(flag: &'a AtomicBool) -> Control<'a> {
    Control {
        cancelled: flag,
        deadline: Some(Instant::now() + Duration::from_secs(2)),
    }
}
#[test]
fn binary_body_and_negative_status_are_preserved_without_retry() {
    let (url, rx, join) = server(
        b"HTTP/1.1 400 Bad Request\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope".to_vec(),
    );
    let client = HttpClient::new(Config::default()).unwrap();
    let bytes = vec![0, 255, 10, 13, 42];
    let response = client
        .request(
            &url,
            "POST",
            &[("Content-Type".into(), "application/octet-stream".into())],
            bytes.clone(),
            &control(&AtomicBool::new(false)),
            Duration::from_secs(2),
            64,
        )
        .unwrap();
    assert_eq!(response.status, 400);
    assert_eq!(response.bytes, b"nope");
    assert!(rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .ends_with(&bytes));
    join.join().unwrap();
}
#[test]
fn exposes_only_authentication_challenges() {
    let (url, _, join) = server(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer resource_metadata=\"https://example.test/meta\"\r\nWWW-Authenticate: Basic realm=\"other\"\r\nSet-Cookie: secret=canary\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec());
    let response = HttpClient::new(Config::default()).unwrap().request(&url,"GET",&[],vec![],&control(&AtomicBool::new(false)),Duration::from_secs(2),64).unwrap();
    assert_eq!(response.status,401);
    assert_eq!(response.www_authenticate,vec!["Bearer resource_metadata=\"https://example.test/meta\"","Basic realm=\"other\""]);
    assert!(!format!("{response:?}").contains("canary"));
    join.join().unwrap();
}
#[test]
fn redirects_are_returned_without_forwarding_the_request() {
    let(url,rx,join)=server(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec());
    let response = HttpClient::new(Config::default())
        .unwrap()
        .request(
            &url,
            "POST",
            &[],
            b"private".to_vec(),
            &control(&AtomicBool::new(false)),
            Duration::from_secs(2),
            64,
        )
        .unwrap();
    assert_eq!(response.status, 302);
    assert!(rx.recv().unwrap().ends_with(b"private"));
    join.join().unwrap();
}
#[test]
fn response_limits_are_enforced() {
    let (url, _, join) = server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n01234567890123456789"
            .to_vec(),
    );
    let error = HttpClient::new(Config::default())
        .unwrap()
        .request(
            &url,
            "GET",
            &[],
            vec![],
            &control(&AtomicBool::new(false)),
            Duration::from_secs(2),
            8,
        )
        .unwrap_err();
    assert!(error.to_string().contains("limit"));
    join.join().unwrap();
}
#[test]
fn credentials_and_header_injection_are_rejected_before_network() {
    let client = HttpClient::new(Config::default()).unwrap();
    for headers in [
        vec![("Authorization".into(), "Bearer private-value".into())],
        vec![("X-Test".into(), "unsafe\r\nOther: value".into())],
    ] {
        let error = client
            .request(
                "http://127.0.0.1:9",
                "GET",
                &headers,
                vec![],
                &control(&AtomicBool::new(false)),
                Duration::from_secs(1),
                64,
            )
            .unwrap_err();
        assert!(!error.to_string().contains("private-value"));
        assert!(!error.to_string().contains("transport failed"));
    }
    assert!(kageha_integrations::valid_url("https://user:secret@example.test").is_err());
    assert!(HttpClient::new(Config {
        ca_bundle_pem: Some(b"invalid certificate".to_vec()),
        ..Config::default()
    })
    .is_err());
}
#[test]
fn cancellation_closes_in_flight_http_without_a_helper_process() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let flag = Arc::new(AtomicBool::new(false));
    let other = flag.clone();
    let join = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        other.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(100));
    });
    let client = HttpClient::new(Config::default()).unwrap();
    let start = Instant::now();
    let error = client
        .request(
            &url,
            "POST",
            &[],
            vec![],
            &Control {
                cancelled: &flag,
                deadline: None,
            },
            Duration::from_secs(2),
            64,
        )
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(start.elapsed() < Duration::from_secs(1));
    join.join().unwrap();
}

#[test]
fn blocking_pool_credential_refresh_can_use_the_shared_transport() {
    let (url, _, join) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        tokio::task::spawn_blocking(move || {
            let client = HttpClient::new(Config::default()).unwrap();
            let response = client
                .request(
                    &url,
                    "POST",
                    &[],
                    vec![],
                    &control(&AtomicBool::new(false)),
                    Duration::from_secs(2),
                    64,
                )
                .unwrap();
            assert_eq!(response.bytes, b"ok");
        })
        .await
        .unwrap();
    });
    join.join().unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn async_consumers_do_not_need_a_blocking_runtime_or_panic_on_drop() {
    let (url, _, join) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec());
    let client = HttpClient::new(Config::default()).unwrap();
    let response = client
        .request_async(
            &url,
            "GET",
            &[],
            vec![],
            &control(&AtomicBool::new(false)),
            Duration::from_secs(2),
            64,
        )
        .await
        .unwrap();
    assert_eq!(response.bytes, b"ok");
    drop(client);
    join.join().unwrap();
}

#[test]
fn explicit_proxy_routing_and_no_proxy_are_preserved() {
    use kageha_integrations::{ProxyScope, ProxySetting};
    let (proxy, rx, join) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec());
    let client = HttpClient::new(Config {
        proxies: vec![ProxySetting {
            scope: ProxyScope::Http,
            endpoint: proxy,
            bypass: None,
        }],
        ..Config::default()
    })
    .unwrap();
    let response = client
        .request(
            "http://unresolvable.invalid/proof",
            "GET",
            &[],
            vec![],
            &control(&AtomicBool::new(false)),
            Duration::from_secs(2),
            64,
        )
        .unwrap();
    assert_eq!(response.bytes, b"ok");
    assert!(rx
        .recv()
        .unwrap()
        .starts_with(b"GET http://unresolvable.invalid/proof "));
    join.join().unwrap();
    let (url, _, join) =
        server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec());
    let client = HttpClient::new(Config {
        proxies: vec![ProxySetting {
            scope: ProxyScope::All,
            endpoint: "http://127.0.0.1:9".into(),
            bypass: Some("127.0.0.1".into()),
        }],
        ..Config::default()
    })
    .unwrap();
    assert_eq!(
        client
            .request(
                &url,
                "GET",
                &[],
                vec![],
                &control(&AtomicBool::new(false)),
                Duration::from_secs(2),
                64
            )
            .unwrap()
            .bytes,
        b"ok"
    );
    join.join().unwrap();
}
