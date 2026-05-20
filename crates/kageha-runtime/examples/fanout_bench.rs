//! Synthetic adapter latency only: not a provider/model/browser speed comparison.
use kageha_runtime::{execution::*, fanout::execute_parallel};
use serde_json::{json, Value};
use std::{
    io,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
struct Tools;
impl ToolPort for Tools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![])
    }
    fn dispatch_access(&self, _: &ToolCall) -> io::Result<DispatchAccess> {
        Ok(DispatchAccess::Resource(String::new()))
    }
    fn execute(&self, c: &ToolCall, ctl: &Control<'_>) -> io::Result<ToolReply> {
        std::thread::sleep(Duration::from_millis(25));
        ctl.check()?;
        Ok(ToolReply::success(c.id.clone()))
    }
}
fn main() {
    let calls = (0..8)
        .map(|n| ToolCall {
            id: n.to_string(),
            name: "fixture".into(),
            arguments: json!({}),
        })
        .collect::<Vec<_>>();
    let flag = AtomicBool::new(false);
    let ctl = Control {
        cancelled: &flag,
        deadline: None,
    };
    let mut samples = Vec::new();
    for width in [1, 4] {
        let mut elapsed = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            let replies = execute_parallel(&Tools, &calls, &ctl, width).unwrap();
            assert_eq!(replies.len(), 8);
            elapsed.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        samples.push(json!({"parallelism":width,"milliseconds":elapsed}));
    }
    println!(
        "{}",
        json!({"workload":"8 independent tools with 25ms synthetic latency; 5 trials","samples":samples})
    );
}
