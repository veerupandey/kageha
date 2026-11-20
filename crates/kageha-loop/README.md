# Kageha chat service

The Rust host serves the Web UI, native chat API, and durable turn and tool execution. For development setup, see the [root README](../../README.md#build-from-source).

## Durable foreground controls

When the binary is available, WebUI Stop and steering requests for managed
foreground turns use the Rust SQLite adapter. It checks the session and turn
under a write transaction. Stop commits the event, checkpoint, and session and
turn projections together. Steering uses the same inbox and completion fence
as existing chats. Python still supervises the executor and delivers the
stored controls to running work.

Rust also reserves the foreground dispatch token, accepts it exactly once,
and commits recovery ownership for the same turn under a new generation.
Duplicate observers attach to the already reserved execution. Python still
creates the session and turn records, checks process identity and cleanup,
launches the executor, and restores the saved request.

The same adapter records executor heartbeats, fences watchdog revocation and
cleanup, and commits the result and reply outbox in one transaction. The host
still supplies process identity, runs the executor, and delivers outbox replies.

Turn admission also uses this adapter. The host prepares the persona snapshot
and redacted journal payload. Rust checks the idempotency key and the session
metadata and agent binding that the host read, then commits the session, turn,
first event, checkpoint, and foreground ownership rows in one transaction.
When a different turn changes the session binding or metadata before commit,
the host prepares the admission again against the current session.


The task planner uses Rust to validate step identifiers and dependencies.
On Unix, the remote session adapter uses the Rust OpenSSH launcher for
commands, binary transfers, and control socket operations. Plan orchestration
and remote session lifecycle are still hosted by Python.


## Explicit offline chat

Build with Cargo. Set `KAGEHA_PROVIDER_URL` to an OpenAI-compatible chat
completions endpoint and set `KAGEHA_MODEL` to its model identifier. Set
`KAGEHA_API_KEY` for authenticated HTTPS endpoints. The native provider transport uses Rust HTTP clients.

```sh
printf '%s\n' 'Summarize README.md' | target/release/kageha-loop chat --offline /path/to/workspace
```

This runs one read-only turn. The existing WebUI can also use the native HTTP service below.

## Native HTTP service

Run `kageha-loop serve WORKSPACE STATE_DIR [BIND]` to serve native read-only
chat turns. The default bind address is `127.0.0.1:8081`. Configure the same
provider variables as standalone chat and set `KAGEHA_SERVER_TOKEN` to at
least 32 characters. Requests use `Authorization: Bearer TOKEN`.

- `GET /health` reports readiness.
- `POST /chats/CHAT_ID/turns` accepts JSON `request_id` and `message`.
- `GET /chats/CHAT_ID/history` returns the newest 100 turn records.

Chat and request identifiers accept up to 64 ASCII letters, digits, underscores
and hyphens. Repeating a completed request returns its saved answer. A changed
message with the same request ID, or a second turn in an active chat, returns
409. Restart marks unfinished turns as interrupted. The state directory holds
a separate native conversation database and a service lock.

The service also exposes the React WebUI session and chat API described below.
Existing chat imports, planning and specialist workers are supported by the native WebUI API.


## Existing React WebUI

Build the existing frontend with `npm run build --prefix frontend`.
Set `KAGEHA_WEBUI_DIST` to the absolute path of that frontend's `dist` directory
when starting `kageha-loop serve`. Open the server address and sign in with
`KAGEHA_SERVER_TOKEN`. Tokens contain 32 to 256 ASCII letters, digits,
underscores or hyphens. The server uses an HttpOnly, SameSite cookie.

The same frontend can run through Vite by setting
`KAGEHA_WEBUI_BACKEND_URL=http://127.0.0.1:8081`. Its relative API requests
and existing stream parser work with either backend.

The native implementation supports session creation, listing, detail, rename,
pin, archive and deletion, chat submission, SSE status and tool cards, request
receipts, event replay, page-refresh reattachment and Stop. Turn completion and
its terminal events commit together. A disconnected browser does not cancel
the worker. Explicit Stop cancels the provider subprocess.

Bootstrap responses expose configured models and available capabilities. Peer alerts
honor an explicit disabled capability. Unsupported actions fail explicitly; there
is no fallback to a Python process.

`tests/native_server.rs` exercises the WebUI contract using a fixture provider
and starts the backend with only curl on PATH. The browser smoke test uses the
actual production React build against the native server.


### Native browser, profiles, uploads and approvals

The existing browser panel now talks directly to the standalone Rust browser
protocol. Configure a running daemon with `KAGEHA_BROWSER_STATE_DIR` (its private
state directory). Launch it with `--standalone on --chromium ABSOLUTE_CHROMIUM`;
the native WebUI does not accept a Python application bridge. Private browsing
is available by default. Optional `KAGEHA_CHROME_CDP_ENDPOINT` and
`KAGEHA_COMET_CDP_ENDPOINT` expose those profiles in the same picker. Profile
attachment requires the existing consent dialog; browser endpoints are supplied
by the server configuration, never by the client request.

Supported routes retain their current names and JSON fields:

- `GET /api/sessions/ID/browser`, plus `frame` and multipart JPEG `stream`.
- Browser POST operations `connect`, `promote`, `revoke-login`, `lease`,
  `layout`, and `action` (navigation, tabs, mouse and keyboard).
- `POST /api/sessions/ID/upload` accepts one multipart file (maximum 16 MiB).
  `GET /api/sessions/ID/files/PATH` serves chat-owned, hash-checked uploads.
- `POST /api/approvals` resolves durable, single-use turn approvals. Pending
  approvals are restored on refresh and invalidated by cancellation or restart.

Native WebUI turns expose browser open, snapshot, click, fill, key and upload
tools. Browser fills honor explicit routine grants; clicks, keys and uploads still
require per-action approval. Typed values are not persisted in approval records.
The existing permissions API supports Ask, routine session grants (reset on
backend restart), and persistent full routine grants. Approval is pinned to the selected tab and destination.
Reference targets are uniquely re-resolved after approval, even when the old
observation expired. Uploads additionally pin the file digest and destination
origin, recheck bytes after approval, and stage into the standalone daemon's
workspace. A failed script is reported as a failed tool, never a completed action.
These routes do not invoke Python. The standalone command-line chat remains
read-only; browser tools belong to native WebUI turns.

Run the real Chromium integration test after building both Rust crates:

```sh
KAGEHA_BROWSER_BIN=/absolute/path/to/kageha-browser \
  node crates/kageha-loop/tests/webui_browser_e2e.mjs
```

The test launches disposable Chrome processes for private and signed-in-profile
adapter paths. The native backend receives an isolated PATH containing only curl.
It checks preview, responsive layout, consent, profile switching, binding fencing,
fill after an expired observation, exact file upload, changed bytes, deny, cancel,
and switching profiles while an approval is pending. Set `KAGEHA_WEBUI_DIST`
to the frontend build and add `--keep` for manual testing of the same React UI.


### Native planning, specialists and settings

The unchanged React application uses the existing `/api` routes for:

- Plans: `/api/sessions/ID/design`, editable `plan.md`, durable Build approvals
  pinned to the reviewed revision, dependency validation and tool-evidenced
  progress. Editing a pending plan requires a new approval. Incomplete approved
  plans finish as unverified rather than claiming success.
- Agents and teams: versioned definitions, draft generation, rollback, effective
  instructions, tool policies, model preferences and knowledge sources.
- Specialist jobs: bounded parallel workers, isolated child chats, steering,
  cancellation, durable results and attributed replies. Interrupted jobs require
  explicit retry. Restart recovers a committed child result without rerunning it.
- Models: compatible endpoint discovery, credentials, roles, fallback policies
  and optional shadow/active model-routing decisions. Each turn pins its model
  configuration and persona. Credentials are omitted from API responses.
- MCP: native stdio and streamable HTTP transports, tool discovery, bounded
  execution and argument-specific approvals. Server configuration revisions are
  checked again before dispatch. Local skills and UTF-8 knowledge sources can
  be used by native turns.

Set `KAGEHA_LEGACY_SESSIONS_DIR` to an existing sessions directory for one-time,
read-only import of `chat.jsonl` histories. The default is `$KAGEHA_HOME/sessions`
or `~/.kageha/sessions`. Existing chats, speaker metadata and referenced local
attachments remain available; original files are never rewritten. The native
SQLite database stores subsequent messages and imported source markers. Back up
both the database and the original attachment files.

### Additional native adapters

The native server also implements these paths without a Python application or
Python subprocess:

- OpenAI-compatible and Anthropic-compatible model APIs, including model
  discovery, tool-call translation, ordered tool results, cancellation and
  multimodal images from hash-checked chat uploads (4 MiB per turn).
- PDF, DOCX, PPTX, XLSX/XLSM, XML/HTML and UTF-8 text extraction. Office archives
  have expansion limits; PDF parsing runs in a bounded Rust child process.
  Scanned PDFs require OCR, which is not implemented.
- SSH host settings, connectivity checks, per-chat activation, remote workspace
  reads, approved command execution and hash-verified file uploads. Hosts are
  pinned per turn and cannot be switched while that chat or its workers run.
  OpenSSH uses a private state-directory known-hosts file. Chat uploads, MCP and
  browser control remain local; SSH browser/desktop tunnelling is not implemented.
  Cancelling SSH stops the local transport; a remote side effect may remain
  uncertain. Dispatched actions are journaled and are never automatically replayed.
- OAuth authorization-code flow with PKCE, single-use state, refresh and
  disconnect fencing, plus GitHub device login with bounded polling. Gmail,
  LinkedIn identity and repository-scoped GitHub reads enforce agent access lists.
  Registered model providers can use the same native OAuth flow. The server does
  not extract another application's login tokens or impersonate its OAuth client.
- Native OpenAI-compatible speech, transcription and image generation, plus
  Audio8-compatible HTTP voice inference. Reference WAV recordings are validated,
  stored privately and hash-checked. Generated artifacts are bound to their chat
  turn, survive refresh and appear through the existing Files API.
- Telegram Bot API polling and replies with sender allowlists, durable offsets,
  multi-turn history and non-replayed delivery intent. WhatsApp, iMessage and
  phone use a configured HTTP gateway adapter; their service protocols and local
  inference engines have not been reimplemented in this crate.

Media providers can be configured using `POST /api/media-providers` or the
server's `KAGEHA_MEDIA_CONFIG` JSON array. Each entry has `id`, `protocol`
(`openai_compat` or `audio8_http`), `base_url`, optional `api_key`, and `models`
with `tts`, `stt` and/or `image` IDs. Credentials require HTTPS and are omitted
from catalog replies. The existing Settings selectors consume this catalog.

OAuth app configuration uses the existing integration setup routes. Existing
`KAGEHA_GOOGLE_*`, `KAGEHA_LINKEDIN_*` and `KAGEHA_GITHUB_*` client variables are
accepted. Server-owned `KAGEHA_OAUTH_<ID>_AUTHORIZE_URL`, `TOKEN_URL`, `PROFILE_URL`,
`API_URL`, `SCOPES` and `DEVICE_URL` can select compatible endpoints. For model
OAuth, list provider IDs in `KAGEHA_OAUTH_MODEL_PROVIDERS` (JSON), then configure
`KAGEHA_OAUTH_PROVIDER_<ID>_CLIENT_ID`, `CLIENT_SECRET`, `REDIRECT_URI`,
`AUTHORIZE_URL`, `TOKEN_URL`, `MODEL_BASE_URL` and `MODEL`; use uppercase IDs with
hyphens replaced by underscores. The redirect ends in
`/api/integrations/provider-<id>/callback`. Supply an OAuth client registered
for that provider, rather than credentials belonging to a different application.

Gateway channels implement authenticated JSON POST `health`, `updates` and
`send`. Updates accept `{offset, timeout, limit}` and return
`{updates: [{update_id, sender, destination, text}]}`; IDs are monotonic integers.
`send` accepts `{to, text}`. Optional `qr`, `calls` and `transcript` support the
existing channel UI. QR strings are rendered as SVG locally in Rust. Gateway
credentials require HTTPS. Unknown senders are rejected by default.

This is a Python-free native runtime, not complete feature parity with the
Python application. Remaining ports include projects/worktrees, memory/peer
services, arbitrary plugin runtimes, proprietary provider CLI integrations,
local ML/OCR inference and service-specific channel protocols. Live vendor
accounts also require configured credentials and separate acceptance testing.

Run `node crates/kageha-loop/tests/webui_runtime_e2e.mjs` after a release build.
It starts fixture model and MCP servers, then runs the Rust backend with only
curl on PATH. It covers legacy imports, attachments, model routing, agent
policies, skills, both MCP transports, plan editing/approval/evidence, parallel
workers, steering, cancellation, restart recovery and persistence. Add `--keep`
and `KAGEHA_WEBUI_DIST` to inspect the same production React build against the
fixture backend. The separate `webui_browser_e2e.mjs` covers real Chromium,
profiles, preview binding, uploads and approval isolation without Python.


`tests/native_adapters_e2e.mjs` runs local TLS fixtures for Anthropic, OAuth,
media and channels, plus an SSH protocol stand-in. It tests real native parsing,
WAV validation, multimodal payloads, remote upload hashing, approval/target
fencing, OAuth refresh/replay/disconnect races, device polling, channel history
and restart persistence. It is not a live-vendor or real-SSH-server acceptance
test. The backend PATH contains curl and the SSH stand-in, without Python.


Shared terminal chat uses the same server sessions and computer tools as the UI:
`kageha-loop chat --url http://127.0.0.1:8788 --token-file /absolute/private/server-token --session CHAT_ID --interactive --open-computer`.
Use `--ui-url` when the frontend has a different origin. See [the shared Rust CLI](../kageha-session-client/README.md) for approval controls, streaming, safe reattachment and standalone `kageha-chat`.
