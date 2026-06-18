//! Adapter conformance is runnable without the harness or service registry.
use kageha_contracts::*;
use kageha_memory::{http::HttpMemory, sqlite::SqliteMemory};
use std::{collections::BTreeSet, sync::Arc};
fn access(actor: &str) -> MemoryAccess {
    MemoryAccess {
        namespace: "workspace-a".into(),
        actor: actor.into(),
    }
}
fn put(id: &str, body: &str) -> PutMemory {
    PutMemory {
        request_id: format!("{id}-{body}"),
        access: access("agent-a"),
        id: id.into(),
        kind: "note".into(),
        body: body.into(),
        metadata: Default::default(),
        acl: Default::default(),
        expected_revision: None,
    }
}
fn database(root: &tempfile::TempDir, name: &str) -> Arc<dyn MemoryPort> {
    Arc::new(SqliteMemory::open(&root.path().join(name)).unwrap())
}
#[tokio::test]
async fn storage_and_http_share_access_revision_and_idempotency_contract() {
    let root = tempfile::tempdir().unwrap();
    let storage = database(&root, "memory.sqlite");
    let token = "test-operator-token-with-at-least-32-bytes";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router =
        kageha_memory::http::router(database(&root, "remote.sqlite"), token.into()).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let http = HttpMemory::new(&endpoint, token.into()).unwrap();
    let response = reqwest::get(format!("{endpoint}/v1/info")).await.unwrap();
    assert_eq!(response.status(), 401);
    for port in [storage, Arc::new(http) as Arc<dyn MemoryPort>] {
        let mut request = put("retained", "portable value");
        request.acl = BTreeSet::from(["reader".into()]);
        let saved = port.put(request.clone()).await.unwrap();
        assert_eq!(port.put(request.clone()).await.unwrap(), saved);
        assert!(port
            .get(&access("stranger"), "retained")
            .await
            .unwrap()
            .is_none());
        assert!(port
            .get(&access("reader"), "retained")
            .await
            .unwrap()
            .is_some());
        request.body = "wrong request reuse".into();
        assert!(matches!(
            port.put(request).await.unwrap_err().code,
            ErrorCode::Conflict
        ));
        assert_eq!(
            port.search(&access("reader"), "portable", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let snapshot = port.snapshot().await.unwrap();
        assert_eq!(snapshot.records.len(), 1);
        let mut update = put("retained", "updated portable value");
        update.expected_revision = Some(saved.revision + 1);
        assert!(matches!(
            port.put(update.clone()).await.unwrap_err().code,
            ErrorCode::Conflict
        ));
        update.expected_revision = Some(saved.revision);
        let updated = port.put(update).await.unwrap();
        assert!(updated.revision > saved.revision);
        let denied = DeleteMemory {
            request_id: "reader-delete".into(),
            access: access("reader"),
            id: "retained".into(),
            expected_revision: None,
        };
        assert!(matches!(
            port.delete(denied).await.unwrap_err().code,
            ErrorCode::Unauthorized
        ));
        port.put(put("another", "second record")).await.unwrap();
        let page = port.list(&access("agent-a"), None, 1).await.unwrap();
        assert_eq!(page.records.len(), 1);
        assert!(page.next.is_some());
        let next = port.list(&access("agent-a"), page.next, 1).await.unwrap();
        assert_eq!(next.records[0].id, "retained");
        let deleted = port
            .delete(DeleteMemory {
                request_id: "owner-delete".into(),
                access: access("agent-a"),
                id: "retained".into(),
                expected_revision: Some(updated.revision),
            })
            .await
            .unwrap();
        assert!(deleted.tombstone);
        assert_eq!(deleted.provenance, saved.provenance);
        let exported = port.snapshot().await.unwrap();
        assert!(exported
            .receipts
            .iter()
            .filter(|r| r.record.id == "retained")
            .all(|r| r.record.body.is_empty() && r.record.metadata.is_empty()));
        let mut replay = put("retained", "portable value");
        replay.acl = BTreeSet::from(["reader".into()]);
        assert!(port.put(replay).await.unwrap().tombstone);
        assert!(port
            .get(&access("agent-a"), "retained")
            .await
            .unwrap()
            .is_none());
        assert!(port
            .search(&access("agent-a"), "portable", 10)
            .await
            .unwrap()
            .is_empty());
    }
    server.abort();
}
#[tokio::test]
async fn backend_fencing_rejects_old_clients_even_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fenced.sqlite");
    let raw: Arc<dyn MemoryPort> = Arc::new(SqliteMemory::open(&path).unwrap());
    let first = raw.claim_fence("controller-a", 0, "claim-a").await.unwrap();
    let old = raw.with_fence(first.clone()).unwrap();
    old.put(put("one", "confirmed")).await.unwrap();
    assert!(matches!(
        raw.put(put("unfenced", "denied")).await.unwrap_err().code,
        ErrorCode::StaleGeneration
    ));
    raw.retire_fence(&first).await.unwrap();
    assert!(matches!(
        old.put(put("retired", "denied")).await.unwrap_err().code,
        ErrorCode::StaleGeneration
    ));
    drop(old);
    drop(raw);
    let raw: Arc<dyn MemoryPort> = Arc::new(SqliteMemory::open(&path).unwrap());
    assert!(raw.fence_status().await.unwrap().unwrap().retired);
    let second = raw
        .claim_fence("controller-b", first.epoch, "claim-b")
        .await
        .unwrap();
    assert_eq!(
        raw.claim_fence("controller-b", first.epoch, "claim-b")
            .await
            .unwrap(),
        second
    );
    raw.with_fence(second.clone())
        .unwrap()
        .put(put("two", "new controller"))
        .await
        .unwrap();
    assert!(matches!(
        raw.with_fence(first)
            .unwrap()
            .put(put("late", "denied"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleGeneration
    ));
    assert_eq!(raw.snapshot().await.unwrap().records.len(), 2);
}
#[tokio::test]
async fn http_fencing_rejects_a_stale_or_missing_writer_context() {
    let root = tempfile::tempdir().unwrap();
    let token = "http-writer-test-token-at-least-thirty-two-bytes";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = kageha_memory::http::router(database(&root, "http.sqlite"), token.into()).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = HttpMemory::new(&endpoint, token.into()).unwrap();
    let fence = client
        .claim_fence("owner-a", 0, "http-claim-a")
        .await
        .unwrap();
    let old = client.with_fence(fence.clone()).unwrap();
    old.put(put("first", "confirmed")).await.unwrap();
    let next = client
        .claim_fence("owner-b", fence.epoch, "http-claim-b")
        .await
        .unwrap();
    assert!(matches!(
        old.put(put("late", "must fail")).await.unwrap_err().code,
        ErrorCode::StaleGeneration
    ));
    assert!(matches!(
        client
            .import(client.snapshot().await.unwrap())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleGeneration
    ));
    client
        .with_fence(next)
        .unwrap()
        .put(put("second", "new owner"))
        .await
        .unwrap();
    assert_eq!(client.snapshot().await.unwrap().records.len(), 2);
    server.abort();
}
#[tokio::test]
async fn malformed_success_response_does_not_claim_a_write_was_rejected() {
    let root = tempfile::tempdir().unwrap();
    let backend = database(&root, "response.sqlite");
    let port = backend.clone();
    let router = axum::Router::new().route(
        "/v1/calls",
        axum::routing::post(move |axum::Json(call): axum::Json<MemoryCall>| {
            let port = port.clone();
            async move {
                if let MemoryCall::Put { request } = call {
                    port.put(request).await.unwrap();
                }
                "invalid-json-after-commit"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = HttpMemory::new(
        &endpoint,
        "invalid-response-test-token-at-least-thirty-two-bytes".into(),
    )
    .unwrap();
    let result = client.put(put("committed", "response lost")).await;
    assert!(matches!(result.unwrap_err().code, ErrorCode::Uncertain));
    assert_eq!(backend.snapshot().await.unwrap().records.len(), 1);
    server.abort();
}
