use kageha_ui::{AgentCommand, HostCommand, Panel, Registry, Snapshot, MAX_MESSAGE_BYTES, PROTOCOL_VERSION};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};

#[derive(Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Initialize { version: u32, components: Registry, state: Option<Snapshot> },
    Agent { command: AgentCommand },
    Host { command: HostCommand },
}

fn handle(panel: &mut Option<Panel>, request: Request) -> Value {
    match request {
        Request::Initialize { version, components, state } => {
            if panel.is_some() {
                return json!({"error": "already_initialized"});
            }
            if version != PROTOCOL_VERSION {
                return json!({"error": "unsupported_version", "version": PROTOCOL_VERSION});
            }
            let initialized = match state {
                Some(state) => Panel::restore(components, state),
                None => Panel::new(components),
            };
            match initialized {
                Ok(new) => {
                    let response = json!({"version": PROTOCOL_VERSION, "state": new.snapshot()});
                    *panel = Some(new);
                    response
                }
                Err(error) => json!({"error": error}),
            }
        }
        Request::Agent { command } => match panel.as_mut() {
            Some(panel) => match panel.apply_agent(command) {
                Ok(state) => json!({"state": state}),
                Err(error) => json!({"error": error, "state": panel.snapshot()}),
            },
            None => json!({"error": "initialize_first"}),
        },
        Request::Host { command } => match panel.as_mut() {
            Some(panel) => match panel.apply_host(command) {
                Ok(event) => json!({"state": panel.snapshot(), "event": event}),
                Err(error) => json!({"error": error, "state": panel.snapshot()}),
            },
            None => json!({"error": "initialize_first"}),
        },
    }
}

fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut panel = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut input).take((MAX_MESSAGE_BYTES + 1) as u64).read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        let response = if line.len() > MAX_MESSAGE_BYTES {
            if line.last() != Some(&b'\n') {
                input.skip_until(b'\n')?;
            }
            json!({"error": "message_too_large"})
        } else {
            match serde_json::from_slice(&line) {
                Ok(request) => handle(&mut panel, request),
                Err(_) => json!({"error": "invalid_request"}),
            }
        };
        serde_json::to_writer(&mut output, &response)?;
        writeln!(output)?;
        output.flush()?;
    }
}
