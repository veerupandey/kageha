//! Service client CLI: client ENDPOINT TOKEN_FILE METHOD < request.json
//! Methods: info/run/status/events/steer/cancel/approvals/approval/decide.
use kageha_agent_sdk::*;
use serde_json::{json, Value};
use std::io::Read;
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: client ENDPOINT TOKEN_FILE METHOD < request.json".into());
    }
    let token = std::fs::read_to_string(&args[2])?;
    let client = AgentClient::new(&args[1], token.trim_end())?;
    let mut text = String::new();
    std::io::stdin()
        .take(9 * 1024 * 1024)
        .read_to_string(&mut text)?;
    let value: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text)?
    };
    let result = invoke(&client, &args[3], &value).await;
    println!(
        "{}",
        match result {
            Ok(v) => json!({"value":v}),
            Err(e) =>
                json!({"error":{"status":e.status,"code":e.code,"message":e.message,"retryable":e.retryable}}),
        }
    );
    Ok(())
}
async fn invoke(client: &AgentClient, method: &str, value: &Value) -> Result<Value> {
    let parse = |e| Error {
        status: None,
        code: "example_input".into(),
        message: format!("Invalid example input: {e}"),
        retryable: false,
    };
    let run = value["run"].as_str().unwrap_or("");
    Ok(match method {
        "info" => client.info().await?,
        "run" => json!(
            client
                .run(&serde_json::from_value::<RunSpec>(value.clone()).map_err(parse)?)
                .await?
        ),
        "status" => json!(client.status(run).await?),
        "cancel" => json!(client.cancel(run).await?),
        "steer" => {
            client
                .steer(
                    run,
                    &serde_json::from_value::<Steering>(value["message"].clone()).map_err(parse)?,
                )
                .await?
        }
        "approvals" => {
            let page = client
                .approvals(ApprovalFilter {
                    session: value["session"].as_str(),
                    turn: value["turn"].as_str(),
                    pending: value["pending"].as_bool(),
                    after: value["after"].as_u64(),
                })
                .await?;
            json!({"approvals":page.approvals,"next_cursor":page.next_cursor})
        }
        "approval" => json!(client.approval(value["id"].as_str().unwrap_or("")).await?),
        "decide" => json!(
            client
                .decide(
                    value["id"].as_str().unwrap_or(""),
                    &serde_json::from_value::<ApprovalDecision>(value["decision"].clone())
                        .map_err(parse)?
                )
                .await?
        ),
        "events" => {
            let mut events = client
                .events(run, value["after"].as_u64().unwrap_or(0))
                .await?;
            let mut values = Vec::new();
            let limit = value["limit"].as_u64().unwrap_or(1024).min(1024);
            while let Some(event) = events.next().await? {
                values.push(json!(event));
                if values.len() as u64 >= limit {
                    break;
                }
            }
            json!(values)
        }
        _ => {
            return Err(parse(serde_json::Error::io(std::io::Error::other(
                "Unknown method",
            ))))
        }
    })
}
