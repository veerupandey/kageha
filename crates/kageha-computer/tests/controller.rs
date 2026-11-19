use kageha_computer::{control::Controller, DesktopBackend};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone, Default)]
struct Fake {
    mutations: Arc<Mutex<usize>>,
    fail: bool,
    preflight: bool,
    activity: Option<Arc<Mutex<Value>>>,
}
impl DesktopBackend for Fake {
    fn input_activity(&mut self) -> Result<Option<Value>, String> {
        Ok(self.activity.as_ref().map(|v| v.lock().unwrap().clone()))
    }
    fn call(&mut self, method: &str, args: Value) -> Result<Value, String> {
        match method {
            "get_window_state" => Ok(
                json!({"ok":true,"pid":args["pid"],"window_id":args["window_id"],"elements":[{"element_token":"native-element:1","role":"AXTextField"}]}),
            ),
            "capture_window" => {
                if self.fail {
                    return Err("Screen Recording permission revoked".into());
                }
                std::fs::write(
                    args["screenshot_out_file"].as_str().unwrap(),
                    b"\xff\xd8\xfftest",
                )
                .unwrap();
                Ok(
                    json!({"ok":true,"pid":args["pid"],"window_id":args["window_id"],"screenshot_file_path":args["screenshot_out_file"],"width":1,"height":1}),
                )
            }
            "set_value" => {
                if self.preflight {
                    return Ok(
                        json!({"ok":false,"execution_phase":"not_dispatched","error":{"message":"disabled control"}}),
                    );
                }
                *self.mutations.lock().unwrap() += 1;
                if self.fail {
                    Err("lost OS reply".into())
                } else {
                    Ok(json!({"ok":true,"verified":true,"effect":"confirmed"}))
                }
            }
            "click" => {
                *self.mutations.lock().unwrap() += 1;
                Ok(json!({"ok":true,"verified":false,"status":"dispatched"}))
            }
            "check_permissions" => Ok(json!({"accessibility":false,"screen_recording":false})),
            _ => Err(format!("unexpected fake call {method}")),
        }
    }
}
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        static IDS: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "computer-control-{}-{}",
            std::process::id(),
            IDS.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
    fn journal(&self) -> PathBuf {
        self.0.join("actions.sqlite")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn request(service: &Controller<Fake>, op: &str, args: Value, id: &str, client: &str) -> Value {
    json!({"protocol":2,"service_epoch":service.epoch(),"workspace_id":"workspace","client_id":client,"request_id":id,"operation":op,"args":args})
}
fn call(service: &mut Controller<Fake>, op: &str, args: Value, id: &str) -> Value {
    let r = request(service, op, args, id, "agent");
    service.handle(r)
}
fn setup(service: &mut Controller<Fake>) -> (String, String) {
    service.set_authorizer(|_, _, _| Ok(()));
    let lease = call(service, "acquire", json!({"ttl_ms":60000}), "lease")["lease_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let observation = call(
        service,
        "observe",
        json!({"lease_id":lease,"pid":3,"window_id":4}),
        "observe",
    )["observation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    (lease, observation)
}
fn write(lease: &str, observation: &str) -> Value {
    json!({"lease_id":lease,"observation_id":observation,"action":{"method":"set_value","args":{"element_token":"native-element:1","value":"private-input-must-not-be-journaled"}}})
}
#[test]
fn duplicate_returns_receipt_without_dispatch_and_conflicting_id_is_rejected() {
    let directory = Directory::new();
    let fake = Fake::default();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let args = write(&lease, &observation);
    let first = call(&mut service, "execute", args.clone(), "write-1");
    assert_eq!(first["receipt"]["state"], "verified");
    assert_eq!(
        call(&mut service, "execute", args.clone(), "write-1"),
        first
    );
    assert_eq!(*mutations.lock().unwrap(), 1);
    let mut changed = args.clone();
    changed["action"]["args"]["value"] = json!("different");
    assert_eq!(
        call(&mut service, "execute", changed, "write-1")["error"]["code"],
        "request_id_conflict"
    );
    assert_eq!(
        call(&mut service, "execute", args, "write-2")["error"]["code"],
        "observation_stale"
    );
    let bytes = std::fs::read(directory.journal()).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("private-input-must-not-be-journaled"));
}
#[test]
fn another_actor_cannot_steal_lease_observation_or_receipt() {
    let directory = Directory::new();
    let fake = Fake::default();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let other = request(
        &service,
        "execute",
        write(&lease, &observation),
        "write",
        "other",
    );
    assert_eq!(service.handle(other)["error"]["code"], "lease_invalid");
    let other = request(
        &service,
        "acquire",
        json!({"ttl_ms":1000}),
        "steal",
        "other",
    );
    assert_eq!(service.handle(other)["error"]["code"], "desktop_busy");
    call(
        &mut service,
        "execute",
        write(&lease, &observation),
        "write",
    );
    let other = request(
        &service,
        "receipt",
        json!({"request_id":"write"}),
        "lookup",
        "other",
    );
    assert_eq!(service.handle(other)["error"]["code"], "receipt_not_found");
    assert_eq!(*mutations.lock().unwrap(), 1);
}
#[test]
fn host_takeover_invalidates_input_and_old_observation_after_reacquire() {
    let directory = Directory::new();
    let mut service = Controller::open(Fake::default(), &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    service.take_control();
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&lease, &observation),
            "write"
        )["error"]["code"],
        "lease_invalid"
    );
    let fresh = call(&mut service, "acquire", json!({"ttl_ms":1000}), "new-lease")["lease_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&fresh, &observation),
            "write"
        )["error"]["code"],
        "observation_stale"
    );
}
#[test]
fn backend_error_is_unknown_recoverable_and_never_replayed() {
    let directory = Directory::new();
    let fake = Fake {
        fail: true,
        ..Default::default()
    };
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let args = write(&lease, &observation);
    let reply = call(&mut service, "execute", args.clone(), "unknown");
    assert_eq!(reply["status"], "unknown");
    assert_eq!(reply["execution_phase"], "dispatch_started");
    assert_eq!(call(&mut service, "execute", args, "unknown"), reply);
    assert_eq!(*mutations.lock().unwrap(), 1);
    let old_epoch = service.epoch().to_owned();
    drop(service);
    let mut restarted = Controller::open(Fake::default(), &directory.journal()).unwrap();
    assert_ne!(restarted.epoch(), old_epoch);
    let mut lookup = request(
        &restarted,
        "receipt",
        json!({"request_id":"unknown"}),
        "lookup",
        "agent",
    );
    lookup["service_epoch"] = json!(old_epoch);
    assert_eq!(restarted.handle(lookup), reply);
    let mut stale = request(
        &restarted,
        "acquire",
        json!({"ttl_ms":1000}),
        "lease",
        "agent",
    );
    stale["service_epoch"] = json!(old_epoch);
    assert_eq!(
        restarted.handle(stale)["error"]["code"],
        "service_epoch_stale"
    );
}
#[test]
fn crash_reservation_cannot_be_replayed_after_restart() {
    let directory = Directory::new();
    let mut service = Controller::open(Fake::default(), &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let args = write(&lease, &observation);
    call(&mut service, "execute", args, "interrupted");
    drop(service);
    let db = rusqlite::Connection::open(directory.journal()).unwrap();
    db.execute(
        "UPDATE receipts SET response=NULL WHERE request_id='interrupted'",
        [],
    )
    .unwrap();
    drop(db);
    let mut service = Controller::open(Fake::default(), &directory.journal()).unwrap();
    let reply = call(
        &mut service,
        "receipt",
        json!({"request_id":"interrupted"}),
        "lookup",
    );
    assert_eq!(reply["receipt"]["state"], "unknown");
    assert_eq!(reply["retryable"], false);
}
#[test]
fn invalid_actions_do_not_consume_snapshot_or_dispatch() {
    let directory = Directory::new();
    let fake = Fake::default();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    for bad in [
        json!({"method":"set_value","args":{"element_token":"other","value":"text"}}),
        json!({"method":"set_value","args":{"element_token":"native-element:1","value":"text","session":"spoof"}}),
        json!({"method":"click","args":{"x":-1,"y":1}}),
        json!({"method":"press_key","args":{"key":[]}}),
    ] {
        let reply = call(
            &mut service,
            "execute",
            json!({"lease_id":lease,"observation_id":observation,"action":bad}),
            "bad",
        );
        assert_eq!(reply["execution_phase"], "not_dispatched");
    }
    assert_eq!(*mutations.lock().unwrap(), 0);
    assert_eq!(
        call(&mut service, "execute", write(&lease, &observation), "good")["receipt"]["state"],
        "verified"
    );
}
#[test]
fn private_journal_is_exclusive_and_rejects_insecure_or_symlink_storage() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = Directory::new();
    let service = Controller::open(Fake::default(), &directory.journal()).unwrap();
    assert!(Controller::open(Fake::default(), &directory.journal()).is_err());
    drop(service);
    let link = directory.0.join("link");
    symlink(directory.journal(), &link).unwrap();
    assert!(Controller::open(Fake::default(), &link).is_err());
    std::fs::set_permissions(directory.journal(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Controller::open(Fake::default(), &directory.journal()).is_err());
}
#[test]
fn scope_labels_cannot_collide_at_separator_boundaries() {
    let directory = Directory::new();
    let mut service = Controller::open(Fake::default(), &directory.journal()).unwrap();
    let mut first = request(&service, "acquire", json!({"ttl_ms":1000}), "lease", "b:c");
    first["workspace_id"] = json!("a");
    let lease = service.handle(first)["lease_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut other = request(
        &service,
        "release",
        json!({"lease_id":lease}),
        "release",
        "c",
    );
    other["workspace_id"] = json!("a:b");
    assert_eq!(service.handle(other)["error"]["code"], "lease_invalid");
}
#[test]
fn no_permission_probe_prompts_or_changes_control_lease() {
    let directory = Directory::new();
    let mut service = Controller::open(Fake::default(), &directory.journal()).unwrap();
    let permissions = call(&mut service, "permissions", json!({}), "permissions");
    assert_eq!(permissions["result"]["accessibility"], false);
    let (lease, observation) = setup(&mut service);
    let reply = call(
        &mut service,
        "execute",
        json!({"lease_id":lease,"observation_id":observation,"action":{"method":"click","args":{"element_token":"native-element:1"}}}),
        "click",
    );
    assert_eq!(reply["receipt"]["state"], "dispatched");
    assert_ne!(reply["receipt"]["state"], "verified");
}

#[test]
fn host_policy_receives_immutable_resolved_target_and_cannot_be_changed_by_rpc() {
    let directory = Directory::new();
    let fake = Fake::default();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let lease = call(&mut service, "acquire", json!({"ttl_ms":60000}), "lease")["lease_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let observation = call(
        &mut service,
        "observe",
        json!({"lease_id":lease,"pid":3,"window_id":4}),
        "observe",
    )["observation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&lease, &observation),
            "write"
        )["error"]["code"],
        "authorization_required"
    );
    assert_eq!(*mutations.lock().unwrap(), 0);
    service.set_authorizer(|scope, method, args| {
        assert_eq!(scope, "[\"workspace\",\"agent\"]");
        assert_eq!(method, "set_value");
        assert_eq!(args["pid"], 3);
        assert_eq!(args["window_id"], 4);
        assert!(args["session"]
            .as_str()
            .unwrap()
            .starts_with("control-computer-"));
        Err("host denied this destination".into())
    });
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&lease, &observation),
            "write"
        )["error"]["code"],
        "authorization_required"
    );
    assert_eq!(*mutations.lock().unwrap(), 0);
    assert_eq!(
        call(
            &mut service,
            "receipt",
            json!({"request_id":"write"}),
            "lookup"
        )["error"]["code"],
        "receipt_not_found"
    );
}
#[test]
fn expiry_does_not_reset_by_observing_and_reacquisition_requires_new_observation() {
    let directory = Directory::new();
    let mut service = Controller::open(Fake::default(), &directory.journal()).unwrap();
    service.set_authorizer(|_, _, _| Ok(()));
    let lease = call(&mut service, "acquire", json!({"ttl_ms":1}), "lease")["lease_id"]
        .as_str()
        .unwrap()
        .to_owned();
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert_eq!(
        call(
            &mut service,
            "observe",
            json!({"lease_id":lease,"pid":3,"window_id":4}),
            "observe"
        )["error"]["code"],
        "lease_invalid"
    );
    assert_eq!(
        call(
            &mut service,
            "renew",
            json!({"lease_id":lease,"ttl_ms":60000}),
            "renew"
        )["error"]["code"],
        "lease_invalid"
    );
    let (lease, observation) = setup(&mut service);
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&lease, &observation),
            "write"
        )["status"],
        "ok"
    );
}
#[test]
fn journal_failure_before_dispatch_sends_nothing_and_after_dispatch_stays_unknown() {
    let directory = Directory::new();
    let fake = Fake::default();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let db = rusqlite::Connection::open(directory.journal()).unwrap();
    db.execute_batch("CREATE TRIGGER deny_reservation BEFORE INSERT ON receipts BEGIN SELECT RAISE(FAIL,'disk failure'); END;").unwrap();
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&lease, &observation),
            "write"
        )["execution_phase"],
        "not_dispatched"
    );
    assert_eq!(*mutations.lock().unwrap(), 0);
    db.execute_batch("DROP TRIGGER deny_reservation; CREATE TRIGGER deny_completion BEFORE UPDATE ON receipts BEGIN SELECT RAISE(FAIL,'disk failure'); END;").unwrap();
    let reply = call(
        &mut service,
        "execute",
        write(&lease, &observation),
        "write",
    );
    assert_eq!(reply["status"], "unknown");
    assert_eq!(*mutations.lock().unwrap(), 1);
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&lease, &observation),
            "write"
        )["receipt"]["state"],
        "unknown"
    );
    assert_eq!(*mutations.lock().unwrap(), 1);
}
#[test]
fn journal_capacity_never_evicts_previous_deduplication_records() {
    let directory = Directory::new();
    let fake = Fake::default();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let db = rusqlite::Connection::open(directory.journal()).unwrap();
    db.execute_batch("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n<4096) INSERT INTO receipts(owner,request_id,intent,epoch) SELECT 'fixture',CAST(n AS TEXT),'reserved','old-epoch' FROM numbers;").unwrap();
    assert_eq!(
        call(
            &mut service,
            "execute",
            write(&lease, &observation),
            "write"
        )["error"]["code"],
        "journal_capacity"
    );
    assert_eq!(*mutations.lock().unwrap(), 0);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM receipts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4096
    );
}
#[test]
fn separate_journals_cannot_own_the_same_real_adapter_identity() {
    struct Device(Fake);
    impl DesktopBackend for Device {
        fn desktop_id(&self) -> Option<&str> {
            Some("deterministic-test-desktop")
        }
        fn call(&mut self, method: &str, args: Value) -> Result<Value, String> {
            self.0.call(method, args)
        }
    }
    let one = Directory::new();
    let two = Directory::new();
    let first = Controller::open(Device(Fake::default()), &one.journal()).unwrap();
    assert!(Controller::open(Device(Fake::default()), &two.journal()).is_err());
    drop(first);
    assert!(Controller::open(Device(Fake::default()), &two.journal()).is_ok());
}

#[test]
fn waiting_for_host_approval_cannot_extend_input_authority() {
    let directory = Directory::new();
    let fake = Fake::default();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let lease = call(&mut service, "acquire", json!({"ttl_ms":150}), "lease")["lease_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let observed = call(
        &mut service,
        "observe",
        json!({"lease_id":lease,"pid":3,"window_id":4}),
        "observe",
    );
    let observation = observed["observation_id"].as_str().unwrap().to_owned();
    service.set_authorizer(|_, _, _| {
        std::thread::sleep(std::time::Duration::from_millis(180));
        Ok(())
    });
    let reply = call(
        &mut service,
        "execute",
        write(&lease, &observation),
        "waiting-for-approval",
    );
    assert_eq!(reply["error"]["code"], "lease_invalid");
    assert_eq!(reply["execution_phase"], "not_dispatched");
    assert_eq!(*mutations.lock().unwrap(), 0);
    assert_eq!(
        call(
            &mut service,
            "receipt",
            json!({"request_id":"waiting-for-approval"}),
            "lookup"
        )["error"]["code"],
        "receipt_not_found"
    );
}

#[test]
fn native_preflight_refusal_is_durable_without_becoming_an_unknown_dispatch() {
    let directory = Directory::new();
    let fake = Fake {
        preflight: true,
        ..Default::default()
    };
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let args = write(&lease, &observation);
    let reply = call(&mut service, "execute", args.clone(), "preflight");
    assert_eq!(reply["status"], "error");
    assert_eq!(reply["receipt"]["state"], "not_dispatched");
    assert_eq!(reply["error"]["code"], "native_preflight_refused");
    assert_eq!(reply["execution_phase"], "not_dispatched");
    assert_eq!(call(&mut service, "execute", args, "preflight"), reply);
    assert_eq!(
        call(
            &mut service,
            "receipt",
            json!({"request_id":"preflight"}),
            "lookup"
        ),
        reply
    );
    assert_eq!(*mutations.lock().unwrap(), 0);
}

#[test]
fn preview_is_read_only_survives_input_takeover_and_cannot_cross_owners() {
    let directory = Directory::new();
    let backend = Fake::default();
    let mutations = backend.mutations.clone();
    let mut service = Controller::open(backend, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let current = call(
        &mut service,
        "observe",
        json!({"lease_id":lease,"pid":42,"window_id":7}),
        "preview-observe",
    );
    let token = current["preview_id"].as_str().unwrap();
    let frame = call(
        &mut service,
        "preview",
        json!({"preview_id":token}),
        "frame",
    );
    assert_eq!(frame["status"], "ok");
    assert!(frame["result"]["image"]["data"].as_str().is_some());
    let foreign = request(
        &service,
        "preview",
        json!({"preview_id":token}),
        "foreign",
        "other",
    );
    assert_eq!(service.handle(foreign)["error"]["code"], "preview_expired");
    let input = call(
        &mut service,
        "execute",
        json!({"lease_id":lease,"observation_id":current["observation_id"],"action":{"method":"set_value","args":{"element_token":"native-element:1","value":"edited"}}}),
        "input",
    );
    assert_eq!(input["status"], "ok");
    service.take_control();
    assert_eq!(
        call(
            &mut service,
            "preview",
            json!({"preview_id":token}),
            "after-take"
        )["status"],
        "ok"
    );
    assert_eq!(*mutations.lock().unwrap(), 1);
    let denied = call(
        &mut service,
        "execute",
        json!({"lease_id":lease,"observation_id":observation,"action":{"method":"click","args":{"element_token":"native-element:1"}}}),
        "denied",
    );
    assert_eq!(denied["execution_phase"], "not_dispatched");
    assert_eq!(*mutations.lock().unwrap(), 1);
}

fn monitored() -> (Fake, Arc<Mutex<Value>>) {
    let activity = Arc::new(Mutex::new(
        json!({"available":true,"revision":"native:0","active":false}),
    ));
    (
        Fake {
            activity: Some(activity.clone()),
            ..Default::default()
        },
        activity,
    )
}
#[test]
fn human_input_pauses_input_but_inspection_and_receipts_survive_restart() {
    let directory = Directory::new();
    let (fake, activity) = monitored();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake.clone(), &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    let original = call(
        &mut service,
        "execute",
        write(&lease, &observation),
        "saved",
    );
    assert_eq!(original["receipt"]["state"], "verified");
    let observation = call(
        &mut service,
        "observe",
        json!({"lease_id":lease,"pid":3,"window_id":4}),
        "new",
    )["observation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    activity.lock().unwrap()["revision"] = json!("native:1");
    let blocked = call(
        &mut service,
        "execute",
        write(&lease, &observation),
        "blocked",
    );
    assert_eq!(blocked["control"]["reason"], "human_input");
    assert_eq!(*mutations.lock().unwrap(), 1);
    let inspected = call(
        &mut service,
        "inspect",
        json!({"pid":3,"window_id":4}),
        "inspect",
    );
    assert_eq!(inspected["status"], "ok");
    assert_eq!(inspected["read_only"], true);
    assert_eq!(
        call(&mut service, "resume_control", json!({}), "model-resume")["status"],
        "error"
    );
    drop(service);
    let mut recovered = Controller::open(fake, &directory.journal()).unwrap();
    recovered.set_authorizer(|_, _, _| Ok(()));
    assert_eq!(
        call(&mut recovered, "acquire", json!({"ttl_ms":60000}), "lease2")["control"]["reason"],
        "human_input"
    );
    assert_eq!(
        call(
            &mut recovered,
            "receipt",
            json!({"request_id":"saved"}),
            "lookup"
        )["receipt"]["state"],
        "verified"
    );
    recovered.resume_control().unwrap();
    let (new_lease, _) = setup(&mut recovered);
    let denied = call(
        &mut recovered,
        "execute",
        write(&new_lease, inspected["inspection_id"].as_str().unwrap()),
        "inspection-input",
    );
    assert_eq!(denied["status"], "error");
    assert_eq!(*mutations.lock().unwrap(), 1);
}
#[test]
fn crash_of_armed_controller_requires_explicit_host_resume() {
    let directory = Directory::new();
    let (fake, _) = monitored();
    let mut service = Controller::open(fake.clone(), &directory.journal()).unwrap();
    setup(&mut service);
    drop(service);
    let mut recovered = Controller::open(fake, &directory.journal()).unwrap();
    assert_eq!(
        call(
            &mut recovered,
            "acquire",
            json!({"ttl_ms":60000}),
            "after-crash"
        )["control"]["reason"],
        "helper_restarted"
    );
    assert_eq!(
        call(
            &mut recovered,
            "inspect",
            json!({"pid":3,"window_id":4}),
            "view"
        )["status"],
        "ok"
    );
    recovered.resume_control().unwrap();
    assert_eq!(
        call(
            &mut recovered,
            "acquire",
            json!({"ttl_ms":60000}),
            "explicit"
        )["status"],
        "ok"
    );
}
#[test]
fn intervention_during_authorization_never_dispatches() {
    let directory = Directory::new();
    let (fake, activity) = monitored();
    let mutations = fake.mutations.clone();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    service.set_authorizer(move |_, _, _| {
        activity.lock().unwrap()["revision"] = json!("native:2");
        Ok(())
    });
    let result = call(
        &mut service,
        "execute",
        write(&lease, &observation),
        "approval-race",
    );
    assert_eq!(result["control"]["reason"], "human_input");
    assert_eq!(*mutations.lock().unwrap(), 0);
}
#[test]
fn lost_monitoring_and_held_physical_input_fail_closed() {
    for (field, value, reason) in [
        ("available", json!(false), "monitor_unavailable"),
        ("active", json!(true), "human_input"),
    ] {
        let directory = Directory::new();
        let (fake, activity) = monitored();
        let mut service = Controller::open(fake, &directory.journal()).unwrap();
        let (lease, observation) = setup(&mut service);
        activity.lock().unwrap()[field] = value;
        if field == "active" {
            activity.lock().unwrap()["revision"] = json!("external-key-down");
        }
        assert_eq!(
            call(&mut service, "execute", write(&lease, &observation), "lost")["control"]["reason"],
            reason
        );
        assert!(service.resume_control().is_err());
    }
}

#[test]
fn own_queued_key_state_without_external_revision_does_not_pause_lease() {
    let directory = Directory::new();
    let (fake, activity) = monitored();
    let mut service = Controller::open(fake, &directory.journal()).unwrap();
    let (lease, observation) = setup(&mut service);
    activity.lock().unwrap()["active"] = json!(true);
    let result = call(
        &mut service,
        "execute",
        write(&lease, &observation),
        "own-key-still-queued",
    );
    assert_eq!(result["receipt"]["state"], "verified");
    assert!(result.get("control").is_none());
}
