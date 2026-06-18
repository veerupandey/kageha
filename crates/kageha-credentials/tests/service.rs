#![cfg(all(feature = "service", feature = "client"))]
use kageha_contracts::execution::Control;
use kageha_credentials::{
    keys::SuppliedKey,
    repository::SqliteRepository,
    service::{self, ClientIdentity},
    *,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    sync::{atomic::AtomicBool, Arc},
};
fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|v| v.to_string()).collect()
}
fn grants() -> Vec<Grant> {
    vec![Grant {
        principal: "adapter".into(),
        namespace: "models".into(),
        ids: set(&["primary"]),
        purposes: set(&["model"]),
        destinations: set(&["https://provider.example"]),
        permissions: [
            Permission::Write,
            Permission::Resolve,
            Permission::Metadata,
            Permission::Revoke,
        ]
        .into_iter()
        .collect(),
    }]
}
fn access() -> CredentialAccess {
    CredentialAccess {
        principal: "ignored-body-principal".into(),
        purpose: "model".into(),
        destination: "https://provider.example".into(),
    }
}
fn reference() -> CredentialRef {
    CredentialRef {
        namespace: "models".into(),
        id: "primary".into(),
    }
}
async fn vault() -> Arc<Vault> {
    let f = AtomicBool::new(false);
    Arc::new(
        Vault::open(
            Arc::new(SqliteRepository::in_memory().unwrap()),
            Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
            "key",
            grants(),
            &Control {
                cancelled: &f,
                deadline: None,
            },
        )
        .await
        .unwrap(),
    )
}
#[tokio::test]
async fn a_locked_key_protector_is_unavailable_not_a_client_authentication_failure() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let key = dir.path().join("mounted-key");
    std::fs::write(&key, [7u8; 32]).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let flag = AtomicBool::new(false);
    let store = Arc::new(
        Vault::open(
            Arc::new(SqliteRepository::in_memory().unwrap()),
            Arc::new(keys::FileKeys(
                [("key".into(), key.clone())].into_iter().collect(),
            )),
            "key",
            grants(),
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .await
        .unwrap(),
    );
    std::fs::remove_file(&key).unwrap();
    let token = b"disposable-locked-key-token-123456789012345";
    let router = service::router(
        store,
        vec![ClientIdentity {
            principal: "adapter".into(),
            authentication_generation: 1,
            management: false,
            local_token_sha256: Some(format!("{:x}", Sha256::digest(token))),
            uid: Some(unsafe { libc::geteuid() }),
            certificate_sha256: None,
        }],
    )
    .unwrap();
    let socket = dir.path().join("locked.sock");
    let path = socket.clone();
    let host = tokio::spawn(async move { service::serve_unix(&path, router).await });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let http = reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .unwrap();
    let response = http
        .post("http://localhost/v1/info")
        .bearer_auth(std::str::from_utf8(token).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!key.exists());
    host.abort();
}
async fn flow(client: &dyn CredentialStore, a: CredentialAccess) {
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let operation = client
        .put(
            &a,
            CredentialWrite {
                operation_id: "write-one".into(),
                reference: reference(),
                expected_revision: 0,
                kind: "api_key".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(b"SERVICE-CANARY".to_vec()),
            },
            &c,
        )
        .await
        .unwrap();
    assert_eq!(operation.metadata.revision, 1);
    let lease = client.resolve(&a, &reference(), &c).await.unwrap();
    assert_eq!(lease.secret.expose(), b"SERVICE-CANARY");
    client.validate_lease(&a, &lease, &c).await.unwrap();
    let mut wrong = a.clone();
    wrong.destination = "https://wrong.example".into();
    assert!(client.resolve(&wrong, &reference(), &c).await.is_err());
    let conflicting = CredentialWrite {
        operation_id: "write-one".into(),
        reference: reference(),
        expected_revision: 0,
        kind: "api_key".into(),
        expires_at_ms: None,
        secret: SecretBytes::new(b"different".to_vec()),
    };
    assert!(client.put(&a, conflicting, &c).await.is_err());
    let cancelled = AtomicBool::new(true);
    assert!(client
        .resolve(
            &a,
            &reference(),
            &Control {
                cancelled: &cancelled,
                deadline: None
            }
        )
        .await
        .is_err());
    client
        .revoke(&a, &reference(), 1, "revoke", false, &c)
        .await
        .unwrap();
    assert!(client.validate_lease(&a, &lease, &c).await.is_err());
    assert!(client.resolve(&a, &reference(), &c).await.is_err());
    assert_eq!(
        client
            .operation(&a, &reference(), "write-one", &c)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .revision,
        1
    );
}
#[tokio::test]
async fn embedded_uses_the_same_transport_conformance_flow() {
    let v = vault().await;
    let mut a = access();
    a.principal = "adapter".into();
    flow(v.as_ref(), a).await;
}
#[tokio::test]
async fn unix_real_transport_authentication_and_same_ports() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("credentials.sock");
    let token = b"disposable-credential-client-token-123456789";
    let identities = vec![ClientIdentity {
        principal: "adapter".into(),
        authentication_generation: 1,
        management: false,
        local_token_sha256: Some(format!("{:x}", Sha256::digest(token))),
        uid: Some(unsafe { libc::geteuid() }),
        certificate_sha256: None,
    }];
    let router = service::router(vault().await, identities).unwrap();
    let path = socket.clone();
    let server = tokio::spawn(async move { service::serve_unix(&path, router).await });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let client = client::Client::unix(&socket, SecretBytes::new(token.to_vec())).unwrap();
    flow(&client, access()).await;
    let malformed=reqwest::Client::builder().unix_socket(socket.clone()).build().unwrap().post("http://localhost/v1/put").bearer_auth(std::str::from_utf8(token).unwrap()).json(&serde_json::json!({"reference":reference(),"purpose":"model","destination":"https://provider.example","operation_id":"bad","expected_revision":"ERROR-CANARY-SECRET","kind":"api_key","expires_at_ms":null,"secret_base64":""})).send().await.unwrap();
    assert_eq!(malformed.headers()["cache-control"], "no-store");
    assert!(!malformed
        .text()
        .await
        .unwrap()
        .contains("ERROR-CANARY-SECRET"));
    let bad = client::Client::unix(&socket, SecretBytes::new(b"wrong-token".to_vec())).unwrap();
    let f = AtomicBool::new(false);
    assert!(bad
        .resolve(
            &access(),
            &reference(),
            &Control {
                cancelled: &f,
                deadline: None
            }
        )
        .await
        .is_err());
    server.abort();
}
#[tokio::test]
async fn hosted_mutual_tls_and_certificate_identity() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    fn openssl(args: &[&str]) {
        assert!(Command::new("/usr/bin/openssl")
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("TLS fixture requires /usr/bin/openssl")
            .success());
    }
    let p = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
    std::fs::write(p("ca.cnf"),"[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=Credential Test CA\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n").unwrap();
    std::fs::write(p("leaf.cnf"),"[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth,clientAuth\n").unwrap();
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "1",
        "-keyout",
        &p("ca.key"),
        "-out",
        &p("ca.pem"),
        "-config",
        &p("ca.cnf"),
    ]);
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        &p("key.pem"),
        "-out",
        &p("leaf.csr"),
        "-config",
        &p("leaf.cnf"),
    ]);
    openssl(&[
        "x509",
        "-req",
        "-in",
        &p("leaf.csr"),
        "-CA",
        &p("ca.pem"),
        "-CAkey",
        &p("ca.key"),
        "-CAcreateserial",
        "-days",
        "1",
        "-out",
        &p("cert.pem"),
        "-extfile",
        &p("leaf.cnf"),
        "-extensions",
        "ext",
    ]);
    let cert_pem = std::fs::read(p("cert.pem")).unwrap();
    let key_pem = std::fs::read(p("key.pem")).unwrap();
    let ca_pem = std::fs::read(p("ca.pem")).unwrap();
    let certificates = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let fingerprint = format!("{:x}", Sha256::digest(certificates[0].as_ref()));
    let private = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .unwrap()
        .unwrap();
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca_pem.as_slice()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let router = service::router(
        vault().await,
        vec![ClientIdentity {
            principal: "adapter".into(),
            authentication_generation: 1,
            management: false,
            local_token_sha256: None,
            uid: None,
            certificate_sha256: Some(fingerprint),
        }],
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let server = tokio::spawn(async move {
        service::serve_tls(&address.to_string(), router, certificates, private, roots).await
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let mut identity = cert_pem.clone();
    identity.extend(&key_pem);
    let client = client::Client::hosted(
        &format!("https://localhost:{}", address.port()),
        &identity,
        &ca_pem,
    )
    .unwrap();
    flow(&client, access()).await;
    let unauthenticated = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(&ca_pem).unwrap())
        .build()
        .unwrap();
    assert!(unauthenticated
        .get(format!("https://localhost:{}/v1/info", address.port()))
        .send()
        .await
        .is_err());
    server.abort();
}
#[tokio::test]
async fn explicit_rate_limits_fail_before_mutation_and_responses_do_not_cache() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("limit.sock");
    let token = b"disposable-rate-token-123456789012345678";
    let identity = ClientIdentity {
        principal: "adapter".into(),
        authentication_generation: 1,
        management: false,
        local_token_sha256: Some(format!("{:x}", Sha256::digest(token))),
        uid: Some(unsafe { libc::geteuid() }),
        certificate_sha256: None,
    };
    let vault = vault().await;
    let router = service::router_with_options(
        vault.clone(),
        vec![identity],
        service::ServiceOptions {
            requests_per_minute: 1,
        },
    )
    .unwrap();
    let path = socket.clone();
    let host = tokio::spawn(async move { service::serve_unix(&path, router).await });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let http = reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .unwrap();
    let first = http
        .post("http://localhost/v1/info")
        .bearer_auth(std::str::from_utf8(token).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);
    let response=http.post("http://localhost/v1/put").bearer_auth(std::str::from_utf8(token).unwrap()).json(&serde_json::json!({"reference":reference(),"purpose":"model","destination":"https://provider.example","operation_id":"not-admitted","expected_revision":0,"kind":"api_key","expires_at_ms":null,"secret_base64":"c2VjcmV0"})).send().await.unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let f = AtomicBool::new(false);
    let mut a = access();
    a.principal = "adapter".into();
    assert!(vault
        .operation(
            &a,
            &reference(),
            "not-admitted",
            &Control {
                cancelled: &f,
                deadline: None
            }
        )
        .await
        .unwrap()
        .is_none());
    host.abort();
}
#[tokio::test]
async fn cancelled_publication_queries_the_committed_operation_instead_of_replaying() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("publication.sock");
    let token = b"disposable-publication-token-123456789012345";
    let committed = Arc::new(tokio::sync::Notify::new());
    let notice = committed.clone();
    let router = service::router(
        vault().await,
        vec![ClientIdentity {
            principal: "adapter".into(),
            authentication_generation: 1,
            management: false,
            local_token_sha256: Some(format!("{:x}", Sha256::digest(token))),
            uid: Some(unsafe { libc::geteuid() }),
            certificate_sha256: None,
        }],
    )
    .unwrap()
    .layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let notice = notice.clone();
            async move {
                let mutation = req.uri().path() == "/v1/put";
                let response = next.run(req).await;
                if mutation {
                    notice.notify_one();
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                response
            }
        },
    ));
    let path = socket.clone();
    let server = tokio::spawn(async move { service::serve_unix(&path, router).await });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let client = client::Client::unix(&socket, SecretBytes::new(token.to_vec())).unwrap();
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let a = access();
    let pending = client.put(
        &a,
        CredentialWrite {
            operation_id: "durable-before-publication".into(),
            reference: reference(),
            expected_revision: 0,
            kind: "api_key".into(),
            expires_at_ms: None,
            secret: SecretBytes::new(b"PUBLICATION-CANARY".to_vec()),
        },
        &c,
    );
    let cancel = async {
        committed.notified().await;
        f.store(true, std::sync::atomic::Ordering::Release);
    };
    let (result, _) = tokio::join!(pending, cancel);
    assert_eq!(
        result.unwrap_err().code,
        kageha_contracts::ErrorCode::Uncertain
    );
    let fresh = AtomicBool::new(false);
    let recovered = client
        .operation(
            &a,
            &reference(),
            "durable-before-publication",
            &Control {
                cancelled: &fresh,
                deadline: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.metadata.revision, 1);
    server.abort();
}
