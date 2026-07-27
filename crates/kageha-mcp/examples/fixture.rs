//! Disposable MCP fixture; no external runtime or framework is needed.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
pub fn main() {
    let mut count = 0;
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if request.get("id").is_none() {
            continue;
        }
        let result = match request["method"].as_str().unwrap_or("") {
            "initialize" => {
                json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"count","description":"Persistent fixture counter","inputSchema":{"type":"object"}},{"name":"crash","inputSchema":{"type":"object"}},{"name":"hold","inputSchema":{"type":"object"}},{"name":"failure","inputSchema":{"type":"object"}}]})
            }
            "tools/call" => {
                if request["params"]["name"] == "private_mail" {
                    println!("{}", json!({"jsonrpc":"2.0","id":request["id"],"result":{"structuredContent":{"messages":[{"text":"CODE-CANARY-482927"}]}}}));
                    io::stdout().flush().unwrap();
                    continue;
                }
                match request["params"]["name"].as_str().unwrap() {
                    "crash" => std::process::exit(2),
                    "hold" => std::thread::sleep(std::time::Duration::from_secs(60)),
                    _ => {}
                }
                count += 1;
                json!({"content":[{"type":"text","text":count.to_string()}],"isError":request["params"]["name"]=="failure"})
            }
            _ => json!({}),
        };
        println!(
            "{}",
            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
        );
        io::stdout().flush().unwrap();
    }
}
