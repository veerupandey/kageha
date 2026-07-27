# Shared Rust chat CLI

`kageha-chat` and `kageha-loop chat` attach to the existing Rust application server. They use the **same session ID, transcript, agent loop, tool catalog, approvals, computer controller and action journal as the UI**. The CLI does not start a second agent loop, desktop helper or conversation database. This crate is an independent HTTP/session client; the native computer runtime remains independently usable through its Rust SDK and MCP interface.

```sh
cargo build -p kageha-session-client -p kageha-loop

# Attach to a UI chat. /computer opens its live panel.
target/debug/kageha-chat --url http://127.0.0.1:8788 \
  --token-file /absolute/private/server-token --session CHAT_ID --interactive

# Open that same chat's live Computer panel immediately.
target/debug/kageha-chat --url http://127.0.0.1:8788 \
  --token-file /absolute/private/server-token --session CHAT_ID \
  --ui-url http://127.0.0.1:5173 --open-computer --interactive

# A new shared chat; stream tool actions and the result as JSON lines.
printf '%s\n' 'Use the computer to inspect my selected app' | \
  target/debug/kageha-loop chat --url http://127.0.0.1:8788 \
  --token-file /absolute/private/server-token --new --stdin --json

# Read-only recovery of an existing request. Nothing is submitted again.
target/debug/kageha-chat --url http://127.0.0.1:8788 \
  --token-file /absolute/private/server-token --session CHAT_ID \
  --request-id ORIGINAL_REQUEST_ID --follow
```

Use the application's existing access token; token files must be owned private regular files (`chmod 600`). `KAGEHA_SERVER_TOKEN_FILE`, `KAGEHA_SERVER_TOKEN`, `KAGEHA_SERVER_URL` and `KAGEHA_UI_URL` are supported. Tokens are never put in panel URLs or command-line arguments. Outside loopback, HTTPS is required. Authentication redirects preserve the selected chat/panel and reject external return URLs. Serve the built frontend through the Rust host (`KAGEHA_WEBUI_DIST=/absolute/frontend/dist`) or pass the existing UI origin with `--ui-url`.

Without `--session`, sending a prompt creates a shared chat. The CLI prints its session and request IDs. `--follow --session ID` observes UI/CLI activity without starting work. `--request-id ID --follow` follows that original request and exits at its terminal result. `--after-sequence N` resumes a delivered journal cursor. Snapshots never skip admission events, and replayed events are deduplicated. Event reads reconnect automatically; prompt submission, approvals and ownership changes are never automatically retried. Ambiguous submission errors print the original request's recovery command.

Interactive controls:

| Command | Effect |
| --- | --- |
| `/computer` or `/computer open` | Open this session's live Computer panel |
| `/computer status` | Read the same computer state shown by the UI |
| `/computer take` | Explicit operator takeover; invalidates queued agent authority |
| `/computer agent` | Explicit return; interrupted control still requires fresh observation |
| `/computer enable` / `/computer disable` | Change the existing host input opt-in, exactly as the UI does |
| `/approve ID` / `/deny ID` | Decide the exact current session approval once |
| `/history` / `/session` | Read shared transcript or print session/panel identity |
| `/quit` | Detach; shared work continues |

Computer tools come from the server's existing catalog, including info, permissions, apps, windows, activity, inspect, observe, execute, receipt and release. Input requires the same host opt-in, macOS grants and approval policy as the UI. There is no CLI bypass. A terminal without an attached UI can approve exact actions interactively; a piped run can be approved from the same UI session. Closing stdin, Ctrl-C, or a closed output pipe detaches rather than cancelling or approving work. The event queue and input/event/response frames are bounded. Terminal text strips control characters; `--json` emits the structured shared events.

The previous local read-only mode remains explicit:

```sh
printf '%s\n' 'Summarize README.md' | target/debug/kageha-loop chat --offline /workspace
```

## Verification

```sh
cargo test -p kageha-session-client
KAGEHA_CHAT_BIN=/absolute/kageha-chat KAGEHA_LOOP_BIN=/absolute/kageha-loop \
  node crates/kageha-session-client/tests/cli_transport_e2e.mjs

# macOS, granted installed native helper, disposable AppKit fixture only:
KAGEHA_LOOP_BIN=/absolute/kageha-loop KAGEHA_CHAT_BIN=/absolute/kageha-chat \
KAGEHA_COMPUTER_BIN='/absolute/Kageha Computer.app/Contents/MacOS/kageha-computer' \
KAGEHA_CLI_BROWSER_TEST=1 \
  node crates/kageha-session-client/tests/shared_computer_e2e.mjs
```

The browser check uses the official `@playwright/cli` npm package and saves screenshots under `output/playwright/`. The native fixture runs a deterministic provider through Kageha's actual agent loop; it is not a real-model benchmark. It verifies real native input, approval, receipt recovery, shared history, live images, login/deep links, narrow layouts, and detach/reattach. It never submits an application or saves the fixture document. The native desktop must be free; it cannot be controlled concurrently by another helper. `KAGEHA_TEST_RELEASE_READONLY_CONNECTION=1` is explicit local test setup to close only the installed helper's previous **read-only** Kageha connection; it does not terminate input-enabled controllers or user applications.
