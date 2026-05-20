use kageha_runtime::{execution::*, turn::Start};
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
struct Script {
    replies: RefCell<Vec<AssistantMessage>>,
    requests: RefCell<Vec<ModelRequest>>,
}
impl Script {
    fn new(replies: Vec<Value>) -> Self {
        Self {
            replies: RefCell::new(
                replies
                    .into_iter()
                    .map(|r| serde_json::from_value(r).unwrap())
                    .rev()
                    .collect(),
            ),
            requests: RefCell::new(Vec::new()),
        }
    }
}
impl ModelPort for Script {
    fn complete(&self, request: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
        self.requests.borrow_mut().push(request);
        self.replies
            .borrow_mut()
            .pop()
            .ok_or_else(|| io::Error::other("unexpected model call"))
    }
}
#[derive(Default)]
struct Tools {
    calls: RefCell<Vec<String>>,
}
impl ToolPort for Tools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![json!({"function":{"name":"echo"}})])
    }
    fn execute(&self, call: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        self.calls.borrow_mut().push(call.id.clone());
        Ok(if call.arguments["fail"] == true {
            ToolReply::failure("declared failure")
        } else {
            ToolReply::success("Tool failed: reference text".into())
        })
    }
}
fn start() -> Start {
    Start {
        version: 1,
        max_model_calls: 4,
        messages: vec![json!({"role":"user","content":"work"})],
    }
}

#[test]
fn premature_final_is_rejected_internally_until_evidence_is_recorded() {
    struct CheckedTools {
        done: std::cell::Cell<bool>,
    }
    impl ToolPort for CheckedTools {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![json!({"function":{"name":"verify"}})])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.done.set(true);
            Ok(ToolReply::success("verified artifact".into()))
        }
        fn completion_check(&self, _: &str, _: &Control<'_>) -> io::Result<CompletionCheck> {
            Ok(if self.done.get() {
                CompletionCheck {
                    status: CompletionStatus::Ready,
                    instruction: String::new(),
                    evidence: vec!["original-artifact".into()],
                    ..Default::default()
                }
            } else {
                CompletionCheck {
                    status: CompletionStatus::Continue,
                    instruction: "Verify the required artifact before completion".into(),
                    evidence: vec![],
                    ..Default::default()
                }
            })
        }
    }
    let model = Script::new(vec![
        json!({"content":"Done prematurely"}),
        json!({"content":null,"tool_calls":[{"id":"verify-one","name":"verify","arguments":{}}]}),
        json!({"content":"Artifact verified"}),
    ]);
    let tools = CheckedTools {
        done: std::cell::Cell::new(false),
    };
    let cancel = AtomicBool::new(false);
    let mut completion = Value::Null;
    assert_eq!(
        execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None
            },
            |frame| {
                if frame["kind"] == "task_completion" {
                    completion = frame.clone();
                }
                Ok(())
            }
        )
        .unwrap(),
        "Artifact verified"
    );
    assert_eq!(completion["status"], "verified");
    let requests = model.requests.borrow();
    assert!(requests[1]
        .messages
        .iter()
        .any(|message| message["role"] == "system"
            && message["content"] == "Verify the required artifact before completion"));
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|message| message["role"] == "user")
            .count(),
        1
    );
}

#[test]
fn persistent_segments_require_fresh_evidence_and_survive_checkpoint_reload() {
    struct Persistent {
        state: RefCell<Value>,
        calls: RefCell<Vec<String>>,
    }
    impl ToolPort for Persistent {
        fn persistence_policy(&self) -> io::Result<Option<PersistencePolicy>> {
            Ok(Some(PersistencePolicy {
                enabled: true,
                max_model_calls: 8,
                max_duration_seconds: 7200,
            }))
        }
        fn recovery_checkpoint(&self) -> io::Result<Value> {
            Ok(self.state.borrow().clone())
        }
        fn save_recovery(&self, state: &Value) -> io::Result<()> {
            *self.state.borrow_mut() = state.clone();
            Ok(())
        }
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![json!({"function":{"name":"read"}})])
        }
        fn execute(&self, call: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.calls.borrow_mut().push(call.id.clone());
            Ok(ToolReply::success(format!(
                "evidence {}",
                call.arguments["path"]
            )))
        }
    }
    let call = |id, path| json!({"content":null,"tool_calls":[{"id":id,"name":"read","arguments":{"path":path}}]});
    let model = Script::new(vec![
        call("one", "a"),
        call("two", "b"),
        call("three", "c"),
        json!({"content":"Three verified sources"}),
    ]);
    let tools = Persistent {
        state: RefCell::new(Value::Null),
        calls: RefCell::new(vec![]),
    };
    let cancel = AtomicBool::new(false);
    let mut begin = start();
    begin.max_model_calls = 2;
    assert_eq!(
        execute_turn(
            begin,
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None
            },
            |_| Ok(())
        )
        .unwrap(),
        "Three verified sources"
    );
    assert_eq!(tools.calls.borrow().len(), 3);
    assert!(tools.state.borrow()["extensions"].as_u64().unwrap() > 0);
    let loaded =
        kageha_runtime::recovery::RecoveryState::load(tools.state.borrow().clone()).unwrap();
    assert_eq!(loaded.evidence.len(), 3);
}

#[test]
fn interrupted_model_recovery_retries_twice_and_never_uses_partial_tool_calls() {
    struct Broken {
        calls: std::cell::Cell<usize>,
        partial: bool,
    }
    impl ModelPort for Broken {
        fn complete(&self, _: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
            self.calls.set(self.calls.get() + 1);
            let mut failure =
                ModelFailure::new(ModelFailureKind::InterruptedStream, "stream ended");
            failure.partial_tool_calls = self.partial;
            Err(io::Error::other(failure))
        }
    }
    // Two retries from the same context, then the third failure ends the turn.
    for (partial, expected) in [(false, 3), (true, 1)] {
        let model = Broken {
            calls: std::cell::Cell::new(0),
            partial,
        };
        let cancel = AtomicBool::new(false);
        let tools = Tools::default();
        let error = execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None,
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(
            model_failure(&error).unwrap().kind,
            ModelFailureKind::InterruptedStream
        );
        assert_eq!(model.calls.get(), expected);
        assert!(tools.calls.borrow().is_empty());
    }
}

#[test]
fn only_adapter_authorized_reads_retry_with_a_distinct_attempt_receipt() {
    struct Read {
        calls: RefCell<Vec<String>>,
    }
    impl ToolPort for Read {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![json!({"function":{"name":"read"}})])
        }
        fn execute(&self, call: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            let mut calls = self.calls.borrow_mut();
            calls.push(call.id.clone());
            if calls.len() == 1 {
                Ok(ToolReply {
                    content: "Transient read failure".into(),
                    is_error: true,
                    outcome: Some(ToolOutcome {
                        kind: ToolOutcomeKind::Recoverable,
                        code: "read_transport".into(),
                        resource: "source".into(),
                        revision: String::new(),
                        recovery: Some(RecoveryAction::RetryRead),
                        evidence: vec![],
                    }),
                })
            } else {
                Ok(ToolReply::success("Readable source".into()))
            }
        }
    }
    let tools = Read {
        calls: RefCell::new(vec![]),
    };
    let cancelled = AtomicBool::new(false);
    let model = Script::new(vec![
        json!({"content":null,"tool_calls":[{"id":"original","name":"read","arguments":{}}]}),
        json!({"content":"Source recovered"}),
    ]);
    assert_eq!(
        execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancelled,
                deadline: None
            },
            |_| Ok(())
        )
        .unwrap(),
        "Source recovered"
    );
    let calls = tools.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], "original");
    assert_ne!(calls[0], calls[1]);
    assert!(calls[1].starts_with("read-retry:"));
    let requests = model.requests.borrow();
    assert!(requests[1]
        .messages
        .iter()
        .any(|message| message["role"] == "tool"
            && message["tool_call_id"] == "original"
            && message["content"] == "Readable source"));
}
fn calls() -> Value {
    json!({"content":null,"tool_calls":[{"id":"a","name":"echo","arguments":{}},{"id":"b","name":"echo","arguments":{"fail":true}}]})
}
#[test]
fn interchangeable_ports_preserve_order_identity_and_typed_outcomes() {
    for final_text in ["first implementation", "replacement implementation"] {
        let model = Script::new(vec![calls(), json!({"content":final_text})]);
        let tools = Tools::default();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            execute_turn(
                start(),
                &model,
                &tools,
                &Control {
                    cancelled: &cancel,
                    deadline: None
                },
                |_| Ok(())
            )
            .unwrap(),
            final_text
        );
        assert_eq!(*tools.calls.borrow(), vec!["a", "b"]);
        let req = model.requests.borrow();
        let replies: Vec<_> = req[1]
            .messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        assert_eq!(replies[0]["tool_call_id"], "a");
        assert_eq!(replies[0]["is_error"], false);
        assert_eq!(replies[1]["tool_call_id"], "b");
        assert_eq!(replies[1]["is_error"], true);
    }
}
#[test]
fn cancellation_after_one_tool_prevents_the_next_dispatch() {
    struct CancelTools<'a> {
        flag: &'a AtomicBool,
        calls: RefCell<Vec<String>>,
    }
    impl ToolPort for CancelTools<'_> {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![])
        }
        fn execute(&self, call: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.calls.borrow_mut().push(call.id.clone());
            self.flag.store(true, Ordering::Release);
            Ok(ToolReply::success("sent".into()))
        }
    }
    let cancel = AtomicBool::new(false);
    let tools = CancelTools {
        flag: &cancel,
        calls: RefCell::new(vec![]),
    };
    let model = Script::new(vec![calls()]);
    assert_eq!(
        execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None
            },
            |_| Ok(())
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(*tools.calls.borrow(), vec!["a"]);
}
#[test]
fn expired_deadline_never_calls_model_or_tools() {
    let cancel = AtomicBool::new(false);
    let model = Script::new(vec![]);
    let tools = Tools::default();
    assert_eq!(
        execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: Some(Instant::now() - Duration::from_millis(1))
            },
            |_| Ok(())
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(model.requests.borrow().is_empty());
    assert!(tools.calls.borrow().is_empty());
}

#[test]
fn structured_connector_blockers_stop_query_churn_after_independent_progress() {
    struct BlockedTools {
        calls: RefCell<Vec<String>>,
    }
    impl ToolPort for BlockedTools {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![
                json!({"function":{"name":"integration_read"}}),
                json!({"function":{"name":"read_file"}}),
            ])
        }
        fn execute(&self, call: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.calls.borrow_mut().push(call.name.clone());
            Ok(ToolReply::success(if call.name == "integration_read" {
                json!({"connector_outcome":{"connector":"gmail","state":"disconnected","revision":3,"agents":["main"]}}).to_string()
            } else {
                "Independent preparation evidence".into()
            }))
        }
    }
    let call = |id, name, args| json!({"content":null,"tool_calls":[{"id":id,"name":name,"arguments":args}]});
    let model = Script::new(vec![
        call(
            "search-one",
            "integration_read",
            json!({"query":"Seequent"}),
        ),
        call("notes", "read_file", json!({"path":"notes.txt"})),
        call(
            "search-two",
            "integration_read",
            json!({"query":"interview"}),
        ),
        json!({"content":"Preparation completed from notes; connect Gmail to verify the invitation."}),
    ]);
    let tools = BlockedTools {
        calls: RefCell::new(vec![]),
    };
    let cancel = AtomicBool::new(false);
    let mut begin = start();
    begin.max_model_calls = 6;
    let answer = execute_turn(
        begin,
        &model,
        &tools,
        &Control {
            cancelled: &cancel,
            deadline: None,
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(answer.contains("connect Gmail"));
    assert_eq!(
        *tools.calls.borrow(),
        vec!["integration_read", "read_file", "integration_read"]
    );
    assert!(!model.requests.borrow().last().unwrap().tools.is_empty());
}
#[test]
fn exhausted_public_source_preserves_other_sources_and_independent_tools() {
    struct ResearchTools {
        calls: RefCell<Vec<String>>,
    }
    impl ToolPort for ResearchTools {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![
                json!({"function":{"name":"web_fetch"}}),
                json!({"function":{"name":"read_file"}}),
            ])
        }
        fn execute(&self, call: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.calls.borrow_mut().push(call.id.clone());
            Ok(if call.arguments["url"] == "https://example.com/large" {
                ToolReply::failure(
                    json!({"error_code":"research_recovery_exhausted",
                    "url":call.arguments["url"],"retryable":false})
                    .to_string(),
                )
            } else {
                ToolReply::success("Independent evidence".into())
            })
        }
    }
    let call = |id, name, args| json!({"content":null,"tool_calls":[{"id":id,"name":name,"arguments":args}]});
    let model = Script::new(vec![
        call(
            "large-one",
            "web_fetch",
            json!({"url":"https://example.com/large"}),
        ),
        call(
            "large-two",
            "web_fetch",
            json!({"url":"https://example.com/large"}),
        ),
        call(
            "other-source",
            "web_fetch",
            json!({"url":"https://example.com/other"}),
        ),
        call("notes", "read_file", json!({"path":"notes.txt"})),
        json!({"content":"Continued with another source and the workspace notes."}),
    ]);
    let tools = ResearchTools {
        calls: RefCell::new(vec![]),
    };
    let cancel = AtomicBool::new(false);
    let mut begin = start();
    begin.max_model_calls = 6;
    let answer = execute_turn(
        begin,
        &model,
        &tools,
        &Control {
            cancelled: &cancel,
            deadline: None,
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(answer.contains("another source"));
    assert_eq!(
        *tools.calls.borrow(),
        vec!["large-one", "large-two", "other-source", "notes"]
    );
    let requests = model.requests.borrow();
    assert_eq!(requests[2].tools.len(), 2);
    assert!(requests[2]
        .messages
        .iter()
        .any(|message| message["content"].as_str().is_some_and(
            |text| text.contains("exhausted") && text.contains("https://example.com/large")
        )));
    assert!(
        !requests[2].messages.iter().any(|message| message["content"]
            .as_str()
            .is_some_and(|text| text.contains("Conclude now")))
    );
}

#[test]
fn steering_discards_an_obsolete_tool_reply_before_dispatch() {
    struct SteeringTools {
        checkpoints: RefCell<usize>,
        tools: Tools,
    }
    impl ToolPort for SteeringTools {
        fn discover(&self) -> io::Result<Vec<Value>> {
            self.tools.discover()
        }
        fn execute(&self, c: &ToolCall, ctl: &Control<'_>) -> io::Result<ToolReply> {
            self.tools.execute(c, ctl)
        }
        fn checkpoint(&self) -> io::Result<Vec<String>> {
            let mut n = self.checkpoints.borrow_mut();
            *n += 1;
            Ok(if *n == 2 {
                vec!["new direction".into()]
            } else {
                vec![]
            })
        }
    }
    let model = Script::new(vec![calls(), json!({"content":"new result"})]);
    let tools = SteeringTools {
        checkpoints: RefCell::new(0),
        tools: Tools::default(),
    };
    let cancel = AtomicBool::new(false);
    assert_eq!(
        execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None
            },
            |_| Ok(())
        )
        .unwrap(),
        "new result"
    );
    assert!(tools.tools.calls.borrow().is_empty());
    assert_eq!(
        model.requests.borrow()[1].messages.last().unwrap()["content"],
        "new direction"
    );
}
#[test]
fn repeated_tool_ids_cannot_dispatch_twice() {
    let model = Script::new(vec![calls(), calls()]);
    let tools = Tools::default();
    let cancel = AtomicBool::new(false);
    assert!(execute_turn(
        start(),
        &model,
        &tools,
        &Control {
            cancelled: &cancel,
            deadline: None
        },
        |_| Ok(())
    )
    .is_err());
    assert_eq!(tools.calls.borrow().len(), 2);
}
#[test]
fn invalid_calls_and_oversized_batches_are_rejected_before_tools() {
    for invalid in [
        json!({"content":null,"tool_calls":(0..17).map(|n|json!({"id":format!("call-{n}"),"name":"echo","arguments":{}})).collect::<Vec<_>>()}),
        json!({"content":null,"tool_calls":[{"id":"a","name":"echo","arguments":[]}]}),
        json!({"content":null,"tool_calls":[{"id":"a","name":"echo","arguments":{}},{"id":"a","name":"echo","arguments":{}}]}),
    ] {
        let model = Script::new(vec![invalid]);
        let tools = Tools::default();
        let cancel = AtomicBool::new(false);
        assert!(execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None
            },
            |_| Ok(())
        )
        .is_err());
        assert!(tools.calls.borrow().is_empty());
    }
}

#[test]
fn repeated_failed_actions_are_suppressed_without_disabling_independent_work() {
    let failed_call = |id| json!({"content":null,"tool_calls":[{"id":id,"name":"echo","arguments":{"fail":true}}]});
    let model = Script::new(vec![
        failed_call("a"),
        failed_call("b"),
        failed_call("c"),
        json!({"content":null,"tool_calls":[{"id":"d","name":"echo","arguments":{"fail":false}}]}),
        json!({"content":"Independent work finished; the original operation remains blocked."}),
    ]);
    let tools = Tools::default();
    let cancel = AtomicBool::new(false);
    let mut begin = start();
    begin.max_model_calls = 6;
    let answer = execute_turn(
        begin,
        &model,
        &tools,
        &Control {
            cancelled: &cancel,
            deadline: None,
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(answer.contains("remains blocked"));
    assert_eq!(*tools.calls.borrow(), vec!["a", "b", "d"]);
    let requests = model.requests.borrow();
    assert!(!requests[2].tools.is_empty());
    assert!(requests[3].messages.iter().any(|message| message["content"]
        .as_str()
        .is_some_and(|text| text.contains("not dispatched again"))));
}

#[test]
fn last_model_call_can_summarize_completed_work_without_tools() {
    let model = Script::new(vec![
        calls(),
        json!({"content":"One action succeeded and one failed."}),
    ]);
    let tools = Tools::default();
    let cancel = AtomicBool::new(false);
    let mut turn = start();
    turn.max_model_calls = 2;
    let answer = execute_turn(
        turn,
        &model,
        &tools,
        &Control {
            cancelled: &cancel,
            deadline: None,
        },
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(answer, "One action succeeded and one failed.");
    assert_eq!(*tools.calls.borrow(), vec!["a", "b"]);
    // The final call keeps the turn's tool list (Requirement 11.4).
    let requests = model.requests.borrow();
    assert!(!requests[1].tools.is_empty());
    assert_eq!(requests[1].tools, requests[0].tools);
}

#[test]
fn human_control_blocker_wraps_up_without_another_tool_attempt() {
    struct HumanControl;
    impl ToolPort for HumanControl {
        fn discover(&self)->io::Result<Vec<Value>> {Ok(vec![json!({"function":{"name":"click"}})])}
        fn execute(&self,_:&ToolCall,_:&Control<'_>)->io::Result<ToolReply> {
            Ok(ToolReply {content:json!({"summary":"Return browser control, then continue the saved task."}).to_string(),is_error:true,
                outcome:Some(ToolOutcome {kind:ToolOutcomeKind::Blocked,code:"browser_user_control".into(),resource:"browser".into(),revision:"user".into(),recovery:Some(RecoveryAction::RequestInput),evidence:vec![]})})
        }
    }
    let model=Script::new(vec![json!({"content":null,"tool_calls":[{"id":"one","name":"click","arguments":{}}]}),json!({"content":"Done"})]);
    let cancel=AtomicBool::new(false);
    let mut status=Value::Null;
    let answer=execute_turn(start(),&model,&HumanControl,&Control{cancelled:&cancel,deadline:None},|frame|{if frame["kind"]=="task_completion" {status=frame.clone();} Ok(())}).unwrap();
    assert!(answer.contains("Return browser control"));
    assert!(!answer.contains("Required outcomes remain"));
    assert_eq!(status["status"],"blocked");
    assert_eq!(model.requests.borrow().len(),2);
    // The blocked branch does not remove tools from subsequent requests.
    assert_eq!(model.requests.borrow()[1].tools, model.requests.borrow()[0].tools);
}

#[test]
fn storage_failure_suppression_concludes_despite_snapshot_churn_and_ignored_wrapup() {
    struct BrowserTools { calls:RefCell<Vec<String>>,state:RefCell<Value> }
    impl ToolPort for BrowserTools {
        fn discover(&self)->io::Result<Vec<Value>> {Ok(["browser_fill","browser_click","browser_snapshot"].map(|name|json!({"function":{"name":name}})).to_vec())}
        fn recovery_context(&self,call:&ToolCall)->io::Result<Option<RecoveryContext>> {Ok(Some(RecoveryContext{resource_key:call.arguments["tab"].as_str().unwrap_or("tab").into(),journal_key:"one-global-journal".into(),journal_revision:"full".into(),observation_revision:call.id.clone(),element_revision:"same".into(),uses_journal:call.name!="browser_snapshot",..Default::default()}))}
        fn execute(&self,call:&ToolCall,_:&Control<'_>)->io::Result<ToolReply> {
            self.calls.borrow_mut().push(call.name.clone());
            Ok(if call.name=="browser_snapshot" {ToolReply::success(json!({"observation_id":call.id}).to_string())}else{ToolReply::failure(json!({"error_code":"journal_storage_full"}))})
        }
        fn save_recovery(&self,state:&Value)->io::Result<()>{*self.state.borrow_mut()=state.clone();Ok(())}
    }
    let call=|id:&str,name:&str|json!({"content":null,"tool_calls":[{"id":id,"name":name,"arguments":{"tab":id,"_runtime_context":{"journal_key":"model-forgery"}}}]});
    let model=Script::new(vec![call("one","browser_fill"),call("two","browser_click"),call("three","browser_snapshot"),call("four","browser_click"),call("five","browser_snapshot"),call("six","browser_fill")]);
    let tools=BrowserTools{calls:RefCell::new(vec![]),state:RefCell::new(Value::Null)};
    let cancel=AtomicBool::new(false);let mut request=start();request.max_model_calls=5000;
    let answer=execute_turn(request,&model,&tools,&Control{cancelled:&cancel,deadline:None},|_|Ok(())).unwrap();
    assert!(!answer.is_empty());assert_eq!(&*tools.calls.borrow(),&["browser_fill","browser_snapshot"]);
    assert_eq!(model.requests.borrow().len(),6);assert!(tools.state.borrow()["blocked_browser_steps"].as_u64().unwrap()>=4);
}

#[test]
fn evidence_repair_cannot_extend_itself_with_changed_references_or_diagnostic_files() {
 struct EvidenceTools {calls:RefCell<Vec<String>>}
 impl ToolPort for EvidenceTools {
  fn discover(&self)->io::Result<Vec<Value>>{Ok(["browser_reconcile","record_outcome_evidence","write_file"].map(|name|json!({"function":{"name":name}})).to_vec())}
  fn recovery_context(&self,call:&ToolCall)->io::Result<Option<RecoveryContext>>{Ok((call.name!="write_file").then(||RecoveryContext{resource_key:"one-submission".into(),element_revision:"same-proof".into(),evidence_scope:true,..Default::default()}))}
  fn execute(&self,call:&ToolCall,_:&Control<'_>)->io::Result<ToolReply>{self.calls.borrow_mut().push(call.name.clone());Ok(if call.name=="write_file"{ToolReply::success("created report".into())}else{ToolReply::failure(json!({"error_code":if call.name=="browser_reconcile"{"postcondition_unobserved"}else{"invalid_outcome_evidence"}}))})}
 }
 let step=|id:&str,name:&str|json!({"content":null,"tool_calls":[{"id":id,"name":name,"arguments":{"reference":id,"path":format!("outputs/{id}.md")}}]});
 let model=Script::new(vec![step("a","record_outcome_evidence"),step("b","browser_reconcile"),step("c","record_outcome_evidence"),step("d","write_file"),step("e","write_file"),step("f","write_file"),step("g","browser_reconcile")]);
 let tools=EvidenceTools{calls:RefCell::new(vec![])};let cancel=AtomicBool::new(false);let mut request=start();request.max_model_calls=5000;
 assert!(!execute_turn(request,&model,&tools,&Control{cancelled:&cancel,deadline:None},|_|Ok(())).unwrap().is_empty());
 assert_eq!(model.requests.borrow().len(),7);assert_eq!(&*tools.calls.borrow(),&["record_outcome_evidence","browser_reconcile","write_file","write_file","write_file"]);
}

#[test]
fn repeated_successful_browser_reads_are_stopped_before_dispatch() {
    struct ReadTools {calls:RefCell<Vec<String>>}
    impl ToolPort for ReadTools {
        fn discover(&self)->io::Result<Vec<Value>> {Ok(vec![json!({"function":{"name":"browser_inspect"}})])}
        fn recovery_context(&self,_:&ToolCall)->io::Result<Option<RecoveryContext>> {
            Ok(Some(RecoveryContext{resource_key:"field-map".into(),observation_scope:"same-tab-document".into(),..Default::default()}))
        }
        fn execute(&self,call:&ToolCall,_:&Control<'_>)->io::Result<ToolReply> {
            self.calls.borrow_mut().push(call.id.clone());
            Ok(ToolReply::success(json!({"observation_id":call.id,"controls":[{"ref":call.id,"name":"Email","value":""}]}).to_string()))
        }
    }
    let mut steps=Vec::new();
    for i in 0..9 {steps.push(json!({"content":null,"tool_calls":[{"id":i.to_string(),"name":"browser_inspect","arguments":{"selector":"input","observation_id":i.to_string(),"_runtime_context":{"observation_scope":i.to_string()}}}]}));}
    let model=Script::new(steps);let tools=ReadTools{calls:RefCell::new(vec![])};
    let cancel=AtomicBool::new(false);let mut request=start();request.max_model_calls=100;
    assert!(!execute_turn(request,&model,&tools,&Control{cancelled:&cancel,deadline:None},|_|Ok(())).unwrap().is_empty());
    assert_eq!(&*tools.calls.borrow(),&["0","1","2","3"]);
    assert!(model.requests.borrow().len()<10);
}

#[test]
fn acknowledged_noop_clicks_cannot_keep_a_read_and_retry_loop_alive() {
    struct BrowserTools {calls:RefCell<Vec<String>>}
    impl ToolPort for BrowserTools {
        fn discover(&self)->io::Result<Vec<Value>> {Ok(["browser_inspect","browser_click"].map(|name|json!({"function":{"name":name}})).to_vec())}
        fn recovery_context(&self,_:&ToolCall)->io::Result<Option<RecoveryContext>> {
            Ok(Some(RecoveryContext{resource_key:"login-toggle".into(),observation_scope:"same-document".into(),..Default::default()}))
        }
        fn execute(&self,call:&ToolCall,_:&Control<'_>)->io::Result<ToolReply> {
            self.calls.borrow_mut().push(call.name.clone());
            if call.name=="browser_inspect" {return Ok(ToolReply::success(json!({"observation_id":call.id,"text":"Sign in","controls":[{"ref":call.id,"name":"Log in here"}]}).to_string()));}
            let mut reply=ToolReply::success(json!({"receipt":{"request_id":call.id,"state":"verified","action":"click"}}).to_string());
            reply.outcome=Some(ToolOutcome{kind:ToolOutcomeKind::Success,code:"ok".into(),resource:"login-toggle".into(),revision:String::new(),recovery:None,evidence:vec![format!("receipt:{}",call.id)]});
            Ok(reply)
        }
    }
    let steps=(0..9).map(|i|json!({"content":null,"tool_calls":[
        {"id":format!("observe-{i}"),"name":"browser_inspect","arguments":{"selector":"#apply-section"}},
        {"id":format!("click-{i}"),"name":"browser_click","arguments":{"observation_id":format!("observe-{i}"),"target":"Log in here"}}
    ]})).collect();
    let model=Script::new(steps);let tools=BrowserTools{calls:RefCell::new(vec![])};let cancel=AtomicBool::new(false);
    let mut request=start();request.max_model_calls=100;
    assert!(!execute_turn(request,&model,&tools,&Control{cancelled:&cancel,deadline:None},|_|Ok(())).unwrap().is_empty());
    let calls=tools.calls.borrow();assert_eq!(calls.iter().filter(|c|*c=="browser_inspect").count(),4);
    assert!(calls.iter().filter(|c|*c=="browser_click").count()<9);
    assert!(model.requests.borrow().len()<10);
}
