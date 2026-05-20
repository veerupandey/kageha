use kageha_runtime::{execution::*, fanout::execute_parallel};
use serde_json::{json, Value};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
fn call(id: &str, resource: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: resource.into(),
        arguments: json!({}),
    }
}
#[test]
fn typed_uncertainty_fences_later_admission_in_serial_and_parallel_hosts() {
    struct Terminal(AtomicUsize);
    impl ToolPort for Terminal {
        fn discover(&self)->io::Result<Vec<Value>>{Ok(vec![])}
        fn execute(&self,_:&ToolCall,_:&Control<'_>)->io::Result<ToolReply>{
            self.0.fetch_add(1,Ordering::SeqCst);
            Ok(ToolReply{content:"Original receipt is unresolved".into(),is_error:true,
                outcome:Some(ToolOutcome{kind:ToolOutcomeKind::Uncertain,code:"receipt_pending".into(),resource:"shared".into(),revision:"1".into(),recovery:Some(RecoveryAction::InspectReceipt),evidence:vec![]})})
        }
    }
    let cancelled=AtomicBool::new(false);let control=Control{cancelled:&cancelled,deadline:None};
    let calls=vec![call("first","shared"),call("second","shared")];
    for parallel in [false,true] {
        let tools=Terminal(AtomicUsize::new(0));
        let error=if parallel {execute_parallel(&tools,&calls,&control,1)}else{tools.execute_batch(&calls,&control)}.unwrap_err();
        assert!(is_uncertain(&error));assert_eq!(tools.0.load(Ordering::SeqCst),1);
    }
}
#[derive(Default)]
struct Tools {
    trace: Mutex<Vec<String>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    release: AtomicBool,
}
impl ToolPort for Tools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![])
    }
    fn dispatch_access(&self, c: &ToolCall) -> io::Result<DispatchAccess> {
        Ok(if c.name == "exclusive" {
            DispatchAccess::Exclusive
        } else {
            DispatchAccess::Resource(c.name.clone())
        })
    }
    fn execute(&self, c: &ToolCall, ctl: &Control<'_>) -> io::Result<ToolReply> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.trace.lock().unwrap().push(format!("start:{}", c.id));
        if c.id == "slow" {
            while !self.release.load(Ordering::SeqCst) && ctl.check().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
        } else if c.id == "third" {
            self.release.store(true, Ordering::SeqCst);
        }
        self.trace.lock().unwrap().push(format!("end:{}", c.id));
        self.active.fetch_sub(1, Ordering::SeqCst);
        ctl.check()?;
        Ok(ToolReply::success(c.id.clone()))
    }
}
#[test]
fn rolling_fanout_respects_resources_barriers_and_model_result_order() {
    let tools = Tools::default();
    let flag = AtomicBool::new(false);
    let calls = [
        call("slow", "a"),
        call("fast", "b"),
        call("third", "b"),
        call("barrier", "exclusive"),
        call("last", "a"),
    ];
    let replies = execute_parallel(
        &tools,
        &calls,
        &Control {
            cancelled: &flag,
            deadline: Some(Instant::now() + Duration::from_secs(3)),
        },
        2,
    )
    .unwrap();
    assert_eq!(
        replies
            .iter()
            .map(|r| r.content.as_str())
            .collect::<Vec<_>>(),
        vec!["slow", "fast", "third", "barrier", "last"]
    );
    let trace = tools.trace.lock().unwrap();
    let pos = |s| trace.iter().position(|v| v == s).unwrap();
    assert!(pos("end:fast") < pos("start:third"));
    assert!(
        pos("start:third") < pos("end:slow"),
        "must refill without waiting for slow sibling"
    );
    assert!(pos("end:slow") < pos("start:barrier"));
    assert!(pos("end:third") < pos("start:barrier"));
    assert!(pos("end:barrier") < pos("start:last"));
    assert_eq!(tools.peak.load(Ordering::SeqCst), 2);
}
#[test]
fn unknown_adapters_stay_serial_and_invalid_batches_do_not_dispatch() {
    struct Serial(AtomicUsize);
    impl ToolPort for Serial {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolReply::success("ok".into()))
        }
    }
    let tools = Serial(AtomicUsize::new(0));
    let flag = AtomicBool::new(false);
    let ctl = Control {
        cancelled: &flag,
        deadline: None,
    };
    assert!(execute_parallel(&tools, &[call("same", "a"), call("same", "b")], &ctl, 4).is_err());
    assert!(execute_parallel(&tools, &[call("a", "a")], &ctl, 0).is_err());
    assert_eq!(tools.0.load(Ordering::SeqCst), 0);
    assert_eq!(
        tools.dispatch_access(&call("a", "a")).unwrap(),
        DispatchAccess::Exclusive
    );
}
#[test]
fn cancellation_drains_inflight_calls_and_stops_later_admission() {
    let tools = Tools::default();
    let flag = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            while tools.active.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            flag.store(true, Ordering::SeqCst);
        });
        assert_eq!(
            execute_parallel(
                &tools,
                &[call("slow", "a"), call("never", "a")],
                &Control {
                    cancelled: &flag,
                    deadline: Some(Instant::now() + Duration::from_secs(3))
                },
                2
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
    });
    assert_eq!(tools.active.load(Ordering::SeqCst), 0);
    assert!(!tools
        .trace
        .lock()
        .unwrap()
        .iter()
        .any(|v| v == "start:never"));
}
#[test]
fn deadline_does_not_admit_dependents() {
    let tools = Tools::default();
    let flag = AtomicBool::new(false);
    assert_eq!(
        execute_parallel(
            &tools,
            &[call("slow", "a"), call("never", "a")],
            &Control {
                cancelled: &flag,
                deadline: Some(Instant::now() + Duration::from_millis(40))
            },
            4
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(tools.active.load(Ordering::SeqCst), 0);
    assert_eq!(tools.trace.lock().unwrap().len(), 2);
}
#[test]
fn adapter_panics_return_uncertainty_without_replaying_the_call() {
    struct Panic;
    impl ToolPort for Panic {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            panic!("after dispatch")
        }
    }
    let flag = AtomicBool::new(false);
    let error = execute_parallel(
        &Panic,
        &[call("a", "a")],
        &Control {
            cancelled: &flag,
            deadline: None,
        },
        4,
    )
    .unwrap_err();
    assert!(is_uncertain(&error));
    assert!(error.to_string().contains("inspect its receipt"));
}

#[test]
fn terminal_policy_stop_prevents_admission_past_the_barrier() {
    struct Denied(AtomicUsize);
    impl ToolPort for Denied {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(kageha_contracts::policy::stop("denied"))
        }
    }
    let tools = Denied(AtomicUsize::new(0));
    let flag = AtomicBool::new(false);
    let error = execute_parallel(
        &tools,
        &[call("a", "a"), call("b", "b")],
        &Control {
            cancelled: &flag,
            deadline: None,
        },
        4,
    )
    .unwrap_err();
    assert!(kageha_contracts::policy::is_stop(&error));
    assert_eq!(tools.0.load(Ordering::SeqCst), 1);
}

#[test]
fn panic_fences_later_calls_instead_of_reusing_a_torn_adapter() {
    struct Torn(AtomicUsize);
    impl ToolPort for Torn {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("after mutation");
        }
    }
    let tools = Torn(AtomicUsize::new(0));
    let flag = AtomicBool::new(false);
    let result = execute_parallel(
        &tools,
        &[call("one", "same"), call("two", "same")],
        &Control {
            cancelled: &flag,
            deadline: None,
        },
        4,
    );
    assert_eq!(
        tools.0.load(Ordering::SeqCst),
        1,
        "panicked adapter must not admit the next call"
    );
    assert!(is_uncertain(&result.unwrap_err()));
}

#[test]
fn uncertain_adapter_outcomes_stop_admission_instead_of_becoming_retryable_tool_errors() {
    struct Uncertain(AtomicUsize);
    impl ToolPort for Uncertain {
        fn discover(&self) -> io::Result<Vec<Value>> { Ok(vec![]) }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(uncertain_stop("Mutation dispatched; receipt unavailable"))
        }
    }
    let tools=Uncertain(AtomicUsize::new(0));let flag=AtomicBool::new(false);
    let error=execute_parallel(&tools,&[call("first","target"),call("next","target")],&Control{cancelled:&flag,deadline:None},4).unwrap_err();
    assert!(is_uncertain(&error));assert_eq!(tools.0.load(Ordering::SeqCst),1);
}
