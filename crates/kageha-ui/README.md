# Kageha UI

A standalone Rust state machine for one agent-controlled panel. A host can use
the library directly or run the executable over a private, ordered stdin/stdout
connection. The host can keep one panel per chat or workspace.

The host registers component IDs and each component's event names. The agent
can observe, present, replace, or close the current view. Component props are
JSON. The renderer validates its props and decides how to display them. Only
the host can change who controls the panel or send an interaction event.

## Run

```sh
cargo run --locked --manifest-path crates/kageha-ui/Cargo.toml <<'JSON'
{"source":"initialize","version":1,"components":{"checklist":["item_selected"]}}
{"source":"agent","command":{"method":"present","revision":0,"component":"checklist","props":{"title":"Review draft"}}}
{"source":"host","command":{"method":"dispatch","revision":1,"event":"item_selected","payload":{"id":"draft"}}}
{"source":"agent","command":{"method":"observe"}}
JSON
```

Each line receives one JSON response. Mutations use the current revision.
The host can use the state in a stale revision response to refresh the view.
Each accepted interaction increments the revision, so retrying an old event
does not deliver it again. State lives in memory for the worker's lifetime.

The host owns the process pipes. It parses agent input as `AgentCommand` and
constructs the outer agent envelope itself. Only trusted host code constructs
initialization or host envelopes. The `source` field is a message discriminator.
Keep executable renderers, authentication, event routing, and any tool approval
policy at the host boundary.

## Kageha adapter

Build the worker and set `KAGEHA_UI_PANEL_BINARY` to its absolute path in the
Kageha backend and agent worker environment. The `workspace_panel` tool can
show Browser, Computer, Files, or Activity, and present a checklist in Review.
Showing a native view keeps the current checklist available in the Review tab.
Kageha applies user pinning and collapse preferences when focusing a view.
Take control pauses agent changes until the user returns control.

Kageha stores the panel snapshot and the last 100 Review selections for each
chat in its runtime data directory. Read or wait for events after a revision
with `events` or `wait`. A response with `more_events` supplies the cursor in
`next_revision`. An `events_truncated` flag means older selections aged out.
The latest selection for the open checklist stays visible after reload.
Kageha sends the saved snapshot in the trusted initialization message when
starting a worker for a change. Other hosts can choose their own storage and
rendering strategy.

## Live agents in the terminal

The `kageha-agents` executable observes the same agents as the WebUI. In its
default snapshot/follow modes it is read-only. It observes
exactly the same resumable session event stream, including children that continue
after the parent finishes. Observation never starts a turn or approves/stops an agent; explicit controls
are available through `--interactive` or action flags described below.

```sh
cargo build -p kageha-ui --features terminal-client --bin kageha-agents
./target/debug/kageha-agents --session SESSION_ID \
  --token-file /absolute/private/api.token --url http://127.0.0.1:8788 --follow
```

Omit `--follow` for a snapshot. `--plain`, `NO_COLOR`, piped output and dumb
terminals disable redraws. Ctrl-C leaves agents running. On connection loss the
observer reconnects from its last sequence; repeated failures exit with a clear
message. Use HTTPS for remote hosts. Token files must be private and cannot be
symlinks. Terminal control characters from agent content are removed.

```sh
cargo test -p kageha-ui --features terminal-client
node crates/kageha-ui/tests/agents_terminal.mjs
```

The HTTP terminal client is an optional build feature. Embedding the UI state
library does not require its network stack.

## Interactive agent controls

```sh
./target/debug/kageha-agents --session SESSION_ID --token-file /private/api.token \
  --url http://127.0.0.1:8788 --interactive
```

The numbered tree keeps parent/child relationships, independent lifecycle states,
current tool, elapsed time, tool-call count and pending input visible. `use 2`
selects an agent; selection remains attached to its stable ID as other agents
change. `inspect` shows its assignment, result, approval and resume blocker.

`message Check the changed lines` queues steering for the selected agent's next
checkpoint. `cancel` requests cancellation and leaves the view attached until a
confirmed lifecycle update arrives. Neither a request acknowledgement nor a
completed spawn card means the child finished.

`resume` first explains the safe-resume decision. `resume confirm` explicitly
admits another attempt only when the backend reports it is safe. The original
turn and worker revision are sent with the request. Unknown dispatched mutations
are never automatically repeated. For an approval, inspect its details and then
use `approve EXACT_APPROVAL_ID` or `deny EXACT_APPROVAL_ID`; grants are once-only.
A changed/expired approval is rejected rather than approving a later request.

Scriptable equivalents require an explicit agent selector:

```sh
./target/debug/kageha-agents --session SESSION_ID --token-file /private/api.token \
  --agent AGENT_ID --inspect
./target/debug/kageha-agents --session SESSION_ID --token-file /private/api.token \
  --agent AGENT_ID --message 'Check the changed lines'
./target/debug/kageha-agents --session SESSION_ID --token-file /private/api.token \
  --agent AGENT_ID --resume --confirm
```

Inspection and stream reconnects are read-only. Every mutation is sent once and
has an explicit target/turn; network ambiguity is reported with instructions to
inspect the original outcome. No POST is retried automatically. Quitting the
terminal never cancels the agent. Interactive text commands do not require a raw
terminal mode and can be driven through an explicit `--interactive` pipe for
integration testing. Piped output never includes ANSI redraw sequences.

```sh
node crates/kageha-ui/tests/interactive_agents.mjs
```
