use kageha_contracts::{execution::*, *};
use kageha_memory::sqlite::SqliteMemory;
use kageha_services::Registry;
use kageha_tools::memory::MemoryTools;
use serde_json::{json, Value};
use std::{
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
fn context(session: &str, turn: &str) -> ToolContext {
    ToolContext {
        session: session.into(),
        turn: turn.into(),
    }
}
fn invoke(
    tools: &MemoryTools,
    ctx: &ToolContext,
    id: &str,
    name: &str,
    args: Value,
) -> std::io::Result<Value> {
    let call = ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    };
    let reply = tools.execute_in(
        ctx,
        &call,
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        },
    )?;
    Ok(serde_json::from_str(&reply.content)?)
}
#[test]
fn scoped_crud_revisions_and_replacement_keep_one_tool_adapter() {
    let dir = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = Arc::new(Registry::open(&dir.path().join("registry.sqlite")).unwrap());
    let first = Arc::new(SqliteMemory::open(&dir.path().join("first.sqlite")).unwrap());
    let next = Arc::new(SqliteMemory::open(&dir.path().join("next.sqlite")).unwrap());
    let destination = rt.block_on(async {
        registry.register("first".into(), first).await.unwrap();
        let d = registry.register("next".into(), next).await.unwrap();
        registry.initialize("first").await.unwrap();
        d
    });
    let tools = MemoryTools::sessions(registry.clone(), "workspace".into()).unwrap();
    let a = context("a", "turn-a");
    let b = context("b", "turn-b");
    let put = invoke(
        &tools,
        &a,
        "save",
        "memory_put",
        json!({"id":"note","body":"portable text"}),
    )
    .unwrap();
    assert_eq!(put["value"]["revision"], 1);
    assert_eq!(
        invoke(
            &tools,
            &a,
            "save",
            "memory_put",
            json!({"id":"note","body":"portable text"})
        )
        .unwrap()["value"]["revision"],
        1
    );
    assert!(invoke(
        &tools,
        &a,
        "bad",
        "memory_put",
        json!({"id":"note","body":"stale","expected_revision":0})
    )
    .is_err());
    assert!(
        invoke(&tools, &b, "get", "memory_get", json!({"id":"note"})).unwrap()["value"].is_null()
    );
    let call = ToolCall {
        id: "after".into(),
        name: "memory_put".into(),
        arguments: json!({"id":"after","body":"new"}),
    };
    let old = tools.binding(&a, &call).unwrap();
    rt.block_on(registry.replace("switch", "next", &destination.instance_id, 1))
        .unwrap();
    assert!(tools
        .execute_authorized_in(
            &a,
            &call,
            &old,
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: None
            }
        )
        .is_err());
    let result = invoke(&tools, &a, "get", "memory_get", json!({"id":"note"})).unwrap();
    assert_eq!(result["generation"], 2);
    assert_eq!(result["value"]["body"], "portable text");
    assert_eq!(
        invoke(
            &tools,
            &a,
            "search",
            "memory_search",
            json!({"query":"portable"})
        )
        .unwrap()["value"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        invoke(
            &tools,
            &a,
            "delete",
            "memory_delete",
            json!({"id":"note","expected_revision":1})
        )
        .unwrap()["value"]["tombstone"],
        true
    );
    assert!(
        invoke(&tools, &a, "gone", "memory_get", json!({"id":"note"})).unwrap()["value"].is_null()
    );
    for args in [
        json!({"id":"x","namespace":"other"}),
        json!({"id":"x","actor":"other"}),
        json!({"id":"x","destination":"next"}),
    ] {
        assert!(invoke(&tools, &a, "inject", "memory_get", args).is_err());
    }
}
struct Waiting {
    started: AtomicBool,
}
#[async_trait::async_trait]
impl LogicalMemoryPort for Waiting {
    fn binding(&self) -> Result<Binding> {
        Ok(Binding {
            name: "memory.default".into(),
            provider_id: "waiting".into(),
            generation: 1,
            required: memory_capabilities(),
        })
    }
    async fn call(&self, _: Option<u64>, _: MemoryCall) -> Result<Routed<Value>> {
        self.started
            .store(true, std::sync::atomic::Ordering::Release);
        std::future::pending().await
    }
}
#[test]
fn cancellation_and_deadlines_do_not_wait_for_a_provider() {
    let port = Arc::new(Waiting {
        started: AtomicBool::new(false),
    });
    let tools = MemoryTools::sessions(port.clone(), "workspace".into()).unwrap();
    let cancel = AtomicBool::new(false);
    let call = ToolCall {
        id: "get".into(),
        name: "memory_get".into(),
        arguments: json!({"id":"x"}),
    };
    let start = Instant::now();
    assert!(tools
        .execute_in(
            &context("a", "t"),
            &call,
            &Control {
                cancelled: &cancel,
                deadline: Some(Instant::now() + Duration::from_millis(50))
            }
        )
        .is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
    port.started.store(false,std::sync::atomic::Ordering::Release);
    std::thread::scope(|s| {
        s.spawn(|| {
            while !port.started.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::yield_now();
            }
            cancel.store(true, std::sync::atomic::Ordering::Release);
        });
        assert!(tools
            .execute_in(
                &context("a", "t"),
                &call,
                &Control {
                    cancelled: &cancel,
                    deadline: Some(Instant::now() + Duration::from_secs(1))
                }
            )
            .is_err());
    });
}
struct Unsupported;
#[async_trait::async_trait]
impl LogicalMemoryPort for Unsupported {
    fn binding(&self) -> Result<Binding> {
        Ok(Binding {
            name: "memory.default".into(),
            provider_id: "incomplete".into(),
            generation: 1,
            required: Default::default(),
        })
    }
    async fn call(&self, _: Option<u64>, _: MemoryCall) -> Result<Routed<Value>> {
        panic!("Missing capabilities must reject activation")
    }
}
#[test]
fn incomplete_memory_implementations_fail_activation() {
    assert!(MemoryTools::sessions(Arc::new(Unsupported), "workspace".into()).is_err());
}
struct Failed(ErrorCode);
#[async_trait::async_trait]
impl LogicalMemoryPort for Failed {
    fn binding(&self) -> Result<Binding> {
        Ok(Binding {
            name: "memory.default".into(),
            provider_id: "failed".into(),
            generation: 1,
            required: memory_capabilities(),
        })
    }
    async fn call(&self, _: Option<u64>, _: MemoryCall) -> Result<Routed<Value>> {
        Err(ServiceError::new(self.0.clone(), "fixture"))
    }
}
#[test]
fn mutation_transport_errors_are_uncertain_but_conflicts_and_reads_are_distinct() {
    let context = context("chat", "turn");
    let control = Control {
        cancelled: &AtomicBool::new(false),
        deadline: None,
    };
    let call = ToolCall {
        id: "one".into(),
        name: "memory_put".into(),
        arguments: json!({"id":"note","body":"text"}),
    };
    let unknown =
        MemoryTools::sessions(Arc::new(Failed(ErrorCode::Unavailable)), "workspace".into())
            .unwrap();
    assert!(is_uncertain(
        &unknown.execute_in(&context, &call, &control).unwrap_err()
    ));
    let rejected =
        MemoryTools::sessions(Arc::new(Failed(ErrorCode::Conflict)), "workspace".into()).unwrap();
    assert!(!is_uncertain(
        &rejected.execute_in(&context, &call, &control).unwrap_err()
    ));
    let read = ToolCall {
        name: "memory_get".into(),
        arguments: json!({"id":"note"}),
        ..call
    };
    assert!(!is_uncertain(
        &unknown.execute_in(&context, &read, &control).unwrap_err()
    ));
}
