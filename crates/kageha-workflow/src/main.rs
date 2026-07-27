//! NDJSON adapter for frameworks that do not link Rust crates.
use kageha_workflow::{evaluate, Checkpoint, Observation, Spec, State};
use serde::Deserialize;
use std::io::{self, BufRead, Read, Write};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evaluate {
    spec: Spec,
    previous: Option<State>,
    observation: Observation,
    checkpoint: Checkpoint,
}
fn main() -> io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--describe") {
        println!(
            "{}",
            serde_json::json!({"protocol":"workflow.v1","transport":"ndjson","operation":"evaluate","storage":"caller-owned","model_and_browser":"adapter-owned"})
        );
        return Ok(());
    }
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let n = std::io::Read::by_ref(&mut input)
            .take(1_048_577)
            .read_until(b'\n', &mut bytes)?;
        if n == 0 {
            break;
        }
        if bytes.len() > 1_048_576 {
            return Err(io::Error::other("Workflow frame exceeds 1 MiB"));
        }
        let result = (|| -> io::Result<serde_json::Value> {
            let r: Evaluate = serde_json::from_slice(&bytes)?;
            let (state, decision) =
                evaluate(&r.spec, r.previous.as_ref(), &r.observation, r.checkpoint)?;
            Ok(serde_json::json!({"state":state,"result":decision}))
        })();
        serde_json::to_writer(
            &mut output,
            &match result {
                Ok(value) => value,
                Err(error) => serde_json::json!({"error":error.to_string()}),
            },
        )?;
        writeln!(output)?;
        output.flush()?;
    }
    Ok(())
}
