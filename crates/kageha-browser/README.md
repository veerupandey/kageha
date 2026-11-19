# Kageha browser runtime

The Rust browser service provides authenticated local control, CDP
observations, and verified input for private Chromium, signed-in profiles,
and host-provisioned browsers. It also supports isolated background reads.
Hosts integrate through versioned Unix socket messages. The crate builds and
packages independently of Kageha. Standalone mode provides browser control and
receipt storage without a Kageha installation. A host can instead supply its own
approval and journal systems through the application bridge. Kageha uses that
bridge and keeps its chat, workspace files and UI outside the daemon.
[Migration plan](../../docs/BROWSER_RUST_MIGRATION_PLAN.md)

For independent HTTP deployment and agent integration, see the
[standalone API guide](STANDALONE.md). The API uses the same Rust authority,
observation and receipt engine as local clients. It requires explicit startup
flags and a separate private bearer token; existing local deployments are unchanged.

The [browser quality gate](bench/README.md) runs seeded failure scenarios and a
pinned public MiniWoB++ subset through HTTP and native MCP. It saves task outcomes,
wrong effects, step counts and latency reports, and fails on regressions.

Health checks, observations, receipt reads, and preview captures do not consume
the mutation request history. Repeated polling can keep a long-lived panel
connected without exhausting replay protection. Cancellation and shutdown remain
available even when that history is full. Reusing an input request ID still
fails before dispatch.

## Build and test without touching the running application

Run from the migration worktree. Keep build output separate from the installed
computer helper and existing app builds:

```sh
CARGO_TARGET_DIR=/tmp/kageha-browser-rust-m2-target \
  cargo build --locked --manifest-path crates/kageha-browser/Cargo.toml

CARGO_TARGET_DIR=/tmp/kageha-browser-rust-m2-target \
  cargo test --locked --manifest-path crates/kageha-browser/Cargo.toml

python scripts/generate_browser_protocol_fixtures.py --check

KAGEHA_TEST_BROWSER_BINARY=/tmp/kageha-browser-rust-m2-target/debug/kageha-browser \
  KAGEHA_TEST_CHROMIUM="/absolute/path/to/test/Chromium" \
  PYTHONPATH=src python -m pytest -q tests/test_browser_rust_runtime.py
```

Use an interpreter with the project's existing dependencies. Tests opt into an
explicit binary path; the application never builds or starts a daemon on import.
All integration tests use short, disposable `/tmp/kbr-*` directories and their
own processes. They do not call production startup scripts or browser endpoints.

To run a development instance manually, supply every identity flag:

```sh
DEV_BROWSER_ROOT="$(mktemp -d /tmp/kageha-browser-dev.XXXXXX)"
/tmp/kageha-browser-rust-m2-target/debug/kageha-browser \
  --state-dir "$DEV_BROWSER_ROOT/state" \
  --instance-id browser-rust-dev \
  --application-id kageha-browser-dev \
  --chromium "/absolute/path/to/test/Chromium" \
  --standalone on
```

Use an absolute, private state directory owned by the current user.
An existing nonempty directory must carry this instance's matching marker. The
service refuses unsafe permissions rather than changing them. Use a short path:
Unix sockets have a platform-specific path length limit. The daemon uses Unix sockets.

## Private and signed-in browsing in Kageha

Build a native release archive on the platform where the daemon will run.
The archive contains the standalone binary, its protocol schema and a
manifest with the binary's SHA-256 digest.

```sh
python scripts/package_rust_browser.py \
  --target-dir /tmp/kageha-browser-release \
  --output /tmp/kageha-browser-bundles
```

Extract the archive and set these variables in the Kageha process:

- `KAGEHA_BROWSER_CANARY_PACKAGE` is the absolute path to the extracted bundle directory.
- `KAGEHA_BROWSER_CANARY_CHROMIUM` is the absolute path to the Chromium or Chrome executable.

Use a bundle built from this revision to enable the signed-in profile picker.

With both paths configured, eligible local chats use Rust by default. Apply
the same configuration to the WebUI and any workers that create chats. Use
the Browser profile picker for Private, Chrome, or Comet. Chrome and Comet
require an existing debug-enabled profile at `KAGEHA_CHROME_CDP` or
`KAGEHA_COMET_CDP`, respectively. Selecting a profile grants access to new
tabs in that profile, including its existing site sign-in. A named Chrome
or Comet preference starts selected but disconnected until the user connects
it. Hosted browser preferences keep their existing routing. Existing chats
retain their runtime and can switch using
**Try Rust browser** or `{"runtime":"rust"}` at
`POST /api/sessions/{id}/browser/runtime`.

Set `KAGEHA_BROWSER_RUNTIME=python` to select Python for future chats, or
set `KAGEHA_BROWSER_CANARY_SESSIONS` to restrict Rust to manual enrollment
for a comma-separated list of chats. These settings preserve existing pins.
Kageha verifies the package when starting the daemon. The daemon accepts
only the pinned chat's workspace ID and the supported local profiles.

After a daemon crash, Kageha restarts the service and retains the selected
profile name and chat action journal. The panel binding changes. Private tabs
are lost. Reconnect a signed-in profile to recover its previously managed tabs
if the same browser process is still open. Other open tabs can be claimed
explicitly from the panel.
**Use Python browser** stops the Rust daemon and changes the chat's pin at
an idle checkpoint. Return browser control first. An unresolved receipt or
unverified browser input blocks rollback. If the original outcome cannot be
verified, the chat keeps its hold for review.

Set `KAGEHA_TEST_BROWSER_CANARY_PACKAGE` to the extracted directory and
`KAGEHA_TEST_CHROMIUM` to a disposable Chromium executable to run the
packaged canary tests in `tests/test_browser_rust_runtime.py`.

## Python control client and explicit runtime seam

```python
from pathlib import Path
from kageha.harness.browser.rust_client import RustBrowserClient

async with RustBrowserClient(
    state_dir=Path("/tmp/your-development-directory/state"),
    instance_id="browser-rust-dev",
    application_id="kageha-browser-dev",
    workspace_id="disposable-workspace",
) as client:
    health = await client.health()
    assert health["browser_execution"] is False
    browser = (await client.request("launch_browser"))["result"]
    tab = (await client.request("open_tab", {
        **browser, "url": "about:blank",
    }))["result"]
    snapshot = (await client.request("observe_tab", tab))["result"]
    await client.shutdown()
```

Use `browser_runtime("rust", ...)` for explicit client integration.

The state token is read only from the user-private state directory, never from
command-line arguments. Handshake validates instance, application, canonical
state root, and service epoch; the socket also checks the peer UID. Workspace
and client identity are pinned to the authenticated connection. This is local
application-instance separation, not isolation from another process with full
access to the same user's private files.

## Protocol and bounds

The canonical [JSON schema](protocol/schema.json)
owns the protocol version, bounds, envelope definitions, and examples. Generate
the Python package's schema copy and the JSON and TypeScript fixtures with:

```sh
python scripts/generate_browser_protocol_fixtures.py
```

Rust compiles the same schema and round-trips the generated request/reply
fixtures. Python validates every outgoing request and incoming reply. The
handshake advertises supported methods. Callers can use that list to detect
capabilities as the service and host ship independently. The
schema defines reserved authority, state, receipt-event, cancellation, and
binary-frame metadata shapes. Those reserved shapes do not mean authority,
events, browser state, or frame transport are implemented.

Control uses UTF-8 NDJSON. Limits are 64 KiB per message including newline,
16 simultaneous connections, 64 requests per connection, 4,096 remembered IDs
per service epoch, 30-second maximum request deadline, and 5-second connection
read-idle timeout. Writes have a 1-second timeout. Saturated connections close
without entering an unbounded waiting queue. A client that reaches the request
limit reconnects explicitly; the service does not replay a previous request.

Implemented methods:

| Method | Result |
|---|---|
| `handshake` | Authenticate and return available methods and limits |
| `health` | Report daemon readiness and page-input capability |
| `cancel` | Validate the cancellation shape and report `cancellation_target_unknown`; the reserved cancel RPC reports an unknown target |
| `shutdown` | Acknowledge and shut down this instance |

With `--chromium` set, the daemon also advertises the following methods.

| Method | Parameters and result |
|---|---|
| `launch_browser` | Empty params. Returns the ID of the workspace's disposable browser. |
| `open_tab` | `browser_id` and `url`. Returns the browser and tab IDs. |
| `list_tabs` | `browser_id`. Returns explicitly opened tabs and attributable popups. |
| `observe_tab` | `browser_id` and `target_id`. Returns normalized DOM and AX evidence by default. Set `compact: true` for Rust-issued control refs, visible text, and an observation ID. |
| `query_tab` | Exact `selection` with `binding_id`, `browser_id`, and `target_id`. Reads bounded matches for a CSS `selector` with optional `text` or `frame_id`. Set `wait_for` to `visible`, `hidden`, `attached`, or `detached`. For a top-level navigation, set `wait_for: "url"`, `selector: "body"`, and the exact `url`. Waits use the request deadline. Set `console: true` with `selector: "body"` to read buffered console entries. |
| `disconnect_browser` | `browser_id`. Releases the CDP connection. The next read attaches to the same browser and target. |
| `close_browser` | `browser_id`. Closes an owned browser or detaches from a signed-in browser. |

Each workspace can retain multiple profiles. The daemon caps disposable and
signed-in physical browser endpoints at four across the registry, plus its configured hosted
profiles. Each browser can track up to 32 tabs. Requests verify the
browser ID before operating. Standalone and application modes accept HTTP
and HTTPS pages and `about:blank`. The fixture and observation-only modes
limit initial navigation to `about:blank` or an explicit loopback HTTP address
and port. Redirects and subresources follow Chromium's network behavior.

In standalone mode, different chats can attach the same named signed-in profile
with separate consent and control identities. Managed tabs remain exclusive to
one chat: another chat cannot claim them through either the picker or a script.
Detaching a chat does not close the external browser. The native Kageha host
accepts `KAGEHA_CHROME_CDP_ENDPOINT` and `KAGEHA_COMET_CDP_ENDPOINT`; on macOS it
also discovers explicit debugging ports from running Chrome/Comet processes.
Discovery neither restarts browsers nor enables debugging on ordinary windows.

Observations use Chromium's frame and loader IDs, with separate CDP
attachments for cross-process iframes. Each observation covers up to eight
frames and returns up to 80 DOM and 80 AX entries. The daemon checks frame
identities again before returning evidence. The main document has a reserved
budget. If a child frame exceeds the budget or disappears during capture,
the snapshot reports `truncated: true`. Compact snapshots list observed
child frames in `frames` and put `frame_id` on their control entries.
CDP messages have an 8 MiB limit.
The IPC envelope retains its 64 KiB limit.
[Chromium CDP reference](https://chromedevtools.github.io/devtools-protocol/)

The current daemon serializes browser operations through its workspace
registry. The browser driver uses a loopback CDP port reported by the Chromium
process it launched. An internal worker owns the temporary profile and watches
a private pipe from the daemon. It cleans up Chromium and the profile when the
pipe closes, including after a forced daemon exit.

## Standalone control and receipts

Start with `--standalone on` and an explicit Chromium executable. Any client
that implements the socket protocol can use this mode. The daemon owns the
workspace leases, Chromium, and the durable action journal. The state root is
private to this daemon instance and pinned to the selected authority backend.

1. Authenticate, then launch the disposable browser and open a loopback fixture.
2. Call `claim_control` with empty params or an optional `ttl_ms` from 1 to
   60000. The default is 30000. A repeated claim by the active client returns
   the same grant without extending it. A different client in that workspace
   must wait for release or expiry.
3. Observe the tab and pass the returned grant as request `authority` to
  `click_observed`, `fill_observed`, `press_observed`, `select_observed`,
  or `check_observed`. Use
  `action_receipt` to inspect delivery and `reconcile_click` for an uncertain click.
4. Call `release_control` with the matching grant and empty params. A delayed
   release cannot revoke a newer generation. Restart invalidates old grants
   and preserves receipts.

Standalone mode covers the disposable Chromium input methods described below.
Authenticated clients in the same workspace share read access. The control
lease fences page input.

## Browser input and fixture host

Start the daemon with `--fixture-actions on` alongside `--chromium`.
This mode lets the test harness inject grants and revocations at precise
points for fault testing. Grants expire within 60 seconds and belong to the
daemon epoch. The grant and receipt files share an exclusive lock. A test
takeover or revocation attempted while an action holds that lock receives
`action_control_busy`.

- `click_observed` takes `browser_id`, `target_id`, `observation_id`,
  `target`, and an expected selector or URL. Pass the grant
  in the request authority field and choose an explicit request ID. The click
  consumes the latest observation, which expires after 30 seconds. Use a ref from a compact
  observation, an exact `role=button:Name` or `role=link:Name` locator, or
  a unique CSS selector. Rust checks that the resolved element was in the
  observation, scrolls it into view, and checks that it still has the same
  identity and receives pointer events. Existing callers can pass
  `backend_node_id` in place of `target`.
  Use `expected_selector` for one visible element in the original frame.
  In the main frame, use `expected_url` for the exact committed destination in the original tab,
  including any fragment. Add `expected_selector` to also check an element in
  the destination document. Nonempty `expected_text` requires exact trimmed
  text. An optional `timeout_ms` bounds the action and its postcondition
  within the overall request deadline.
- `navigate_tab` takes `browser_id`, `target_id`, `observation_id`, and
  `url`. Pass the grant and an explicit request ID. The daemon consumes the
  observation and waits for the requested tab's document to commit. It returns
  the actual committed URL and document ID. Navigation respects pending input
  receipts on that tab. If the response is lost, observe that tab before
  trying again.
- `fill_observed` takes `browser_id`, `target_id`, `observation_id`,
  `target`, and `text`. Resolve the field by ref, exact role and name, or
  unique CSS selector. Rust replaces the value through Chromium input and
  checks readback. Empty text clears the field. This slice supports textarea
  and text, search, telephone, and URL inputs. Fill text is limited to 16 KiB
  of UTF-8 and omitted from receipts.
- `press_observed` takes `browser_id`, `target_id`, `observation_id`, and
  `key`. Rust checks that the observed element in the selected frame still has focus.
  Supported keys include Enter, Escape, Tab, Space, Backspace, Delete, Home,
  End, PageUp, PageDown, arrows, and ASCII letters and digits. Use chords
  such as `Shift+Tab` with Shift, Control, Alt, or Meta. The result confirms
  key delivery. Inspect the page to verify the resulting behavior.
- `select_observed` and `check_observed` take the same browser, tab and
  observation IDs, plus `target`. Use `option` for the exact value of an
  enabled option in a native select. `option` also accepts an array of up to 64
  unique values to set the complete selection of a native multi-select; `[]`
  clears it. Disabled or ambiguous option values are refused before input, and
  every selected value is read back. Use `checked` for the desired
  boolean state of a native checkbox. A visible native radio supports
  `checked: true`; select a different radio to change the group selection.
  Directly unchecking a radio is rejected before dispatch. Both methods verify
  the resulting state. Hidden radios with visible labels use the click path.
- Pass an observed `frame_id` to scope input to a child frame. With the field
  omitted, locators use the main frame. A child frame replacement invalidates
  the old observation. Child-frame clicks require untransformed iframe geometry.
- A main-frame click can declare one native `dialog` with `url`, `kind`
  (`alert`, `confirm` or `prompt`), exact `message`, and boolean `accept`.
  An accepted prompt may include `prompt_text`. The response is sent only if
  the declared dialog opens during the click and matches the declaration.
  Keep the usual click postcondition to verify the page after the response.
- Input methods record dispatch before input and hold uncertain actions on
  their original tab. A verified receipt can recover a lost reply. An
  unresolved fill, key press, select, or check requires inspection.
  `reconcile_click` only applies to clicks.
- `action_receipt` takes the input request ID and reads its durable receipt
  within the authenticated workspace. This also works after daemon restart.
- `reconcile_click` takes the browser, tab, fresh observation, and click request
  IDs. It checks the original postcondition without sending input. A selector
  alone stays bound to the original document. A recorded destination URL can
  be verified after the original tab navigates.

The fixture journal records dispatch before input. An interrupted input
sequence holds further input in that browser. Once Chromium acknowledges
the input, the hold is scoped to the original tab. Unresolved receipts from
an earlier daemon lifetime hold input until reviewed.
A request ID stays reserved across reconnects, clients, and daemon restarts.

The fixture journal is bounded at 1,024 receipts and 1 MiB. Keep it with the
disposable state root for the duration of the experiment. Standalone mode uses
the same receipt storage. Application-backed sessions use a separate daemon
state directory.

## Run and recover a batch

Call `batch_observed` with the browser ID, tab ID, current observation ID,
and one to eight actions. Supply a control grant and an explicit batch request
ID. The daemon holds one control fence for the batch. It checks the document
before each step and takes fresh observations within that document.

| Action | Fields |
|---|---|
| `fill` | `target` and `text`. Rust checks readback. |
| `press` | `key`. Rust uses the newly observed focused element. |
| `select` | `target` and exact `option` value, or an array for a multi-select. |
| `check` | `target` and boolean `checked` state. |
| `click` | `target` and `expect_selector` or main-frame `expect_url`. Optional `dialog`. |
| `verify` | CSS `target` and optional exact visible `text`. |

Pass `frame_id` on any step to scope it to an observed child frame.
Use unique CSS selectors or exact role/name locators for controls.
Snapshot refs apply to one observation and can be renumbered during a batch.

The runtime records the plan before executing steps. The report lists each
step as `verified`, `not_dispatched`, `uncertain`, or `not_attempted`.
It stops at the first failure and preserves each input receipt. Call
`batch_status` with the batch request ID to recover the report after a lost
reply or daemon restart. Then inspect fresh page state before sending any
remaining work. A still-pending input receipt protects its original tab.

## Preview and profile switching

Use `browser_selection` to read the active profile, tab, and binding. Pass
that binding to `switch_profile` with a name such as `private` or
`alternate`. The daemon starts a separate disposable Chromium process for a
new profile and restores its previous tabs when it is selected again.
`select_tab` takes the binding, browser ID, and target ID.

For a signed-in profile in standalone or application mode, call
`switch_profile` with `profile: "chrome"`, `"comet"`, or `"cdp"`,
`consent: true`, the current `binding_id`, and `endpoint`. The endpoint
must be a local CDP HTTP origin or browser WebSocket on `127.0.0.1` or
`localhost` with an explicit IPv4 port. It must belong to the running profile
you want to share. Rust opens managed tabs in that browser's default context
so they inherit its website sign-in. Existing personal tabs stay outside the
workspace's tab list. `detach_profile` takes the current `binding_id`, drops
the active personal attachment, and returns to private browsing. Detaching or
shutting down the daemon leaves the personal browser and its tabs open.

`preview_tab` takes those same three fields. Its successful JSON reply
contains `byte_length`, `content_type`, and image dimensions, followed by
exactly `byte_length` raw JPEG bytes. The daemon limits a frame to 1 MiB and
checks the binding, document, and CSS viewport around capture. Read the binary body before
sending or reading the next request on that socket.

`capture_tab` takes `selection` with the same three fields, `format` (`jpeg`
or `png`), `quality` (1–100, for JPEG), and `max_width` (0 for native size).
It uses the same binary framing with a 5 MiB limit and applies the same
binding checks. The daemon returns bytes; Kageha publishes them to the chat's
artifact directory for `browser_screenshot` and the panel screenshot endpoint.

For direct input, send `panel_action` with the current `selection` (the same
three fields as `preview_tab`), `viewport` from `browser_selection`, and an
`input` object such as `{"action":"click","x":42,"y":20,"button":"left"}`.
Standalone callers pass the active `claim_control` authority. The application
bridge grants one request under the current user lease. A change of tab,
document, or viewport invalidates the prior frame binding. Pointer input uses
CSS pixels, including the browser's scrollbar area. Use
`{"action":"layout","size":{"width":800,"height":600}}` to fit the target
and `{"action":"layout","size":null}` to restore native geometry. The viewport
override belongs to the managed tab's CDP connection and is released when the
tab or profile changes.

For a Kageha development chat, start the daemon with `--application-bridge on` and set
`KAGEHA_RUST_BROWSER_STATE_DIR` to its private state directory and
`KAGEHA_RUST_BROWSER_SESSION_ID` to the intended chat ID before starting
Kageha. The agent and authenticated right panel use the same Rust selection.
The panel shows a live preview with profile and tab selection. Choose
**Private browser**, **Chrome profile**, or **Comet profile**, then ask the
agent to open a page or select **Take control** to use the address bar and
page. **Return control** releases user control. Kageha publishes Rust JPEGs over
its existing authenticated MJPEG endpoint, with one latest-frame producer
per active binding and up to four viewers. Kageha reads the Chrome and Comet endpoints from
`KAGEHA_CHROME_CDP` and `KAGEHA_COMET_CDP`, with defaults on local ports 9223
and 9222 respectively. Start the desired browser profile with CDP access
before selecting it. Switching back restores managed tabs while the browser
stays available. Use **Browser access → Revoke access** to disconnect the
selected signed-in profile.

## Background reading and hosted browsers

Pass `--lightpanda /absolute/path/to/lightpanda` to enable Lightpanda
Markdown reads and `--curl /absolute/path/to/curl` to enable HTTP reads
with HTML extraction. Curl must support the `%{json}` write-out format.
Call `browser_adapters` to discover available research engines and hosted profiles.
Call `read_page` with `backend` (`http`, `lightpanda`, or `chromium`),
an HTTP(S) `url`, and an optional `max_chars` from 1 to 8000. The result
includes URL, HTTP status, text, format and truncation. HTTP HTML extraction
also returns link labels and hrefs. The daemon accepts two simultaneous
reads and bounds each reply and request deadline. Chromium starts with a
temporary profile for each read, and Lightpanda runs without a cookie jar.
Background reads leave the selected browser and its right-panel preview alone.

In a Rust-backed Kageha chat, `web_fetch` and parallel HTTP reads use the
Rust HTTP adapter. `headless_fetch` and research enrichment use
`KAGEHA_HEADLESS_BACKEND`. Lightpanda execution failures can fall back to
isolated Chromium. HTTP failures retain their status. An arbitrary custom
CDP endpoint is available as a consented interactive profile only. Set
`KAGEHA_RUST_CUSTOM_CDP` in the Kageha process to show it in the profile picker.

Use `--adapter-config /absolute/path/to/adapters.json` to attach to
provider-managed Chromium for Docker, a session computer, or an SSH tunnel,
or to the Electron guest the desktop host has explicitly identified.
The configured endpoints must be distinct loopback CDP origins or browser
WebSockets. The host starts and stops the container, machine, tunnel, or
desktop shell.

```json
{
  "workspace_id": "disposable-workspace",
  "endpoints": {
    "docker": "http://127.0.0.1:19223",
    "machine": "http://127.0.0.1:19224",
    "remote": "http://127.0.0.1:19225",
    "embedded": "http://127.0.0.1:19334"
  },
  "embedded_target": "A0C12409456789ABCDEF0123456789AB"
}
```

Only the configured workspace sees these entries in `available_profiles`.
Choose one by name in `switch_profile` using the current binding, or select
it in Kageha's browser panel. A hosted profile opens its own managed tabs.
The embedded profile binds only the configured guest tab and uses
`browser_navigate` or the panel address bar for navigation in that tab.
Closing a hosted profile releases the attachment. Chat file uploads and
captured downloads require a shared filesystem and are unavailable on these
profiles. Safari and WebKit require a separate adapter.

## JavaScript browser workflows

`run_script` runs a synchronous JavaScript body in a sandboxed QuickJS worker
spawned by the Rust binary. The worker has a cleared environment, bounded
memory, stack and execution time, and only a message pipe to the runtime.
The browser stays in the daemon. The worker exposes a versioned set of
managed tab and page operations through `browser`.

```javascript
const tab = browser.open("http://127.0.0.1:8080/form");
browser.batch([
  { action: "fill", target: "#name", text: "Fixture" },
  { action: "verify", target: "#status", text: "Ready" },
]);
return { tab: tab.tab_handle, page: browser.snapshot().url };
```

- `open(url)` creates and selects a managed tab. `navigate(url)` uses the
  verified navigation path on the selected tab.
- `tabs()` lists managed tabs and attributable popups. `select(tabHandle)`
  selects an owned target. Kageha's right panel follows both open and select.
- With version 3, `attachTab(tabHandle, url, title)` attaches and selects one
  existing tab in the connected personal profile. Pass the exact native tab
  ID, current HTTP(S) URL, and title in the approved script. A changed tab is
  rejected. `previewReady()` verifies that the selected binding can produce a
  preview frame. This reports frame availability, not paint in a UI client.
- `snapshot()` returns compact observation data. Use it to branch or poll a
  condition with `wait(ms)`, which allows up to 5 seconds per call.
- A batch click accepts `expect_selector` or `expect_url` and an optional
  `timeout_ms`. After a verified navigation the next step uses the recorded
  destination document. An unexpected document change stops the batch.
- `batch(steps)` uses the existing receipt-backed fill, press, click and
  verify operations. A failed browser operation stops the script. Batch IDs
  remain available through `batch_status`, including after a lost reply.

Standalone clients pass `version: 1`, `code`, and the current `browser_id`,
`binding_id`, and nullable `target_id` from `browser_selection`. Use the
active `claim_control` grant as request authority. The script allows 32 browser
calls and returns up to 8 KiB of JSON. To stop it, send `cancel` with its
original request ID from another connection authenticated as the same client.
An in-flight browser operation settles under its normal deadline. Later calls
do not run.

In a Kageha chat using the Rust service, use
`browser_script(language="javascript", code=...)`. Kageha asks for approval
and associates batch receipts with the script attempt. Use `browser_receipts`
with the attempt ID to inspect them.

### Script artifacts

Pass `version: 2` to use the export API. Version 1 remains available for
existing standalone clients. Kageha selects version 2.

```javascript
const page = browser.snapshot();
browser.artifacts.writeText("page-notes.md", page.text);
browser.artifacts.writeJson("page-data.json", { url: page.url, controls: page.controls });
return browser.artifacts.screenshot("page.jpg");
```

Use a single filename per export. Text supports `.txt`, `.md`, and `.csv`.
JSON uses `.json`. A screenshot uses `.jpg` or `.jpeg` and captures the
selected tab's viewport. The runtime accepts up to eight exports per script,
with a 256 KiB limit per text or JSON file and a 1 MiB limit per JPEG.
Files publish atomically. A repeated name within the same run fails and the
first file remains available.

The script receives relative paths, MIME types, and byte counts. The runtime
publishes standalone exports below the private state directory at
`artifacts/<workspace>/<request>/`, with both IDs encoded as unpadded
base64url. Kageha publishes under its chat workspace at
`artifacts/browser-script/<attempt>/` and lists the files in **Files**.
Already published files remain available after a later script failure, and
Kageha can recover their references if the script reply is interrupted.

### Download a file from the selected tab

In a Kageha-owned Private browser, use
`browser.artifacts.download("#download-link", "report.pdf")`. The target
must resolve to a visible direct HTTP or HTTPS link in the selected main frame.
Rust checks the locator, dispatches one click, and matches Chromium's download
event to that frame and URL. It waits for terminal progress for that download
ID before publishing the file. The receipt records when Chromium reports
that the download started. The artifact confirms that the file finished.

The download operation shares the eight-file script limit. Each download may
take up to 15 seconds and contain up to 16 MiB. Use a single filename ending
in `.bin`, `.csv`, `.jpeg`, `.jpg`, `.json`, `.pdf`, `.png`, `.txt`, or
`.zip`. Files with the same name in separate runs remain separate.
Cancellation stops waiting and asks Chromium to cancel the matching download
when its ID is known. Incomplete files stay outside the published artifact
folder. If the click outcome is uncertain, inspect its receipt before retrying.

Download capture runs in Private browser. For a button or redirect, pass the
exact expected HTTP(S) download URL as the third argument. Rust matches the
event against the original frame and the supplied URL. Signed-in Chrome and
Comet keep their existing download settings.

### Attach workspace files

Use `browser.upload('input[type=file]', ['inputs/resume.pdf'])` in a version 3
script. Use a unique CSS or role locator for an enabled file input in the
selected managed tab's main document. Private, Chrome, and Comet profiles
support this API. Hidden file inputs work by CSS selector. The result gives
the tab handle and an ordered list of the filenames and byte sizes.

In Kageha, pass `upload_origin` and `upload_paths` to `browser_script`.
Kageha shows the origin, profile and chat-relative file paths for approval and
pins the bytes before asking. Each approved script can attach one set of files.
Use paths under the chat's `inputs/`, `artifacts/`, or `outputs/`.
For a standalone daemon, put files under
`<state>/inputs/<base64url workspace id>/` or
`<state>/outputs/<base64url workspace id>/` and use the same script paths.
For standalone exports, pass the exact `artifacts/…` path returned by
`browser.artifacts`. Paths accept regular files and reject links and internal
directories. Each call can attach up to eight files totaling 16 MiB.

Standalone callers pass `upload: {origin, files: [{path, sha256}]}` in
`run_script` alongside the normal binding and control grant. Use the exact
HTTP(S) origin including its non-default port, and the same ordered paths
in the script. The daemon verifies the live origin and the staged SHA-256
digests before dispatch. Version 1 and 2 scripts retain their other APIs.

The daemon stages private copies so the browser can read them after the
script exits. Copies are retained until their tab or browser closes, with a
limit of 16 attachment operations and 64 MiB per browser. A verified receipt
means Chromium returned the staged files in the input's file list. Some
sites start uploading on selection. Check the site's result separately.
If the receipt is uncertain, inspect the page and receipt before any retry.
The receipt remains in the journal when a different browser profile is selected.
Within the same daemon lifetime, its hold is scoped to the original browser.
After a daemon crash the old browser IDs cannot identify a profile, so
unresolved receipts continue to hold input until reviewed.

## Application authority and journal

Start an isolated daemon with `--application-bridge on` and an explicit
`--chromium` executable. The trusted application host opens
`ApplicationBrowserBridge(state_dir, arbiter, tool_journal)` as an async context
manager. Acquire the browser capability through the persistent arbiter with
owner `rust:<client_id>`. Then call `bridge.bind(client, lease, attempt_id)` and
pass the returned authority to the action and receipt RPCs. The attempt must
be active in the bound application turn and match the requested action.
The bridge binds one session and one active grant at a time.
Use `bridge.bind_reader(client)` to verify a receipt from the same client
without acquiring input authority.

The private Unix socket carries acknowledgement and receipt queries. Rust
holds the arbiter's per-session dispatch fence while sending input. Takeover
and release use that same fence, and a renewed lease carries a new generation.
Kageha's event store commits the dispatch receipt before acknowledging it.
A failed or missing acknowledgement leaves Chromium untouched. A receipt
whose exact evidence would change under journal redaction is refused before
dispatch.

The bridge pins its state directory to the application database and authority
directory. Queries and reconciliation read the application event journal after
restart. An unresolved legacy receipt holds Rust input until its original
outcome is resolved. Application mode uses the application store as the
canonical receipt journal.

## Application tool routing

A trusted test host can set `HarnessContext.rust_browser` to a
`RustBrowserSession(client, arbiter)` for an application-backed daemon.
Use the same workspace ID for the client and application tool journal. This
registers `browser_open`, `browser_navigate`, `browser_tabs`,
`browser_snapshot`, `browser_click`, `browser_fill`, `browser_press`,
`browser_select`, `browser_check`, `browser_batch`, `browser_receipts`,
`browser_reconcile`, and `browser_close`.

Open a loopback fixture in a new tab. List or select owned tabs with
`browser_tabs` and navigate the selected tab with `browser_navigate`.
Select by `tab_handle` from the list or snapshot. The selected tab is local
to the tool session. Each browser RPC identifies its exact target.

Use the snapshot's button or link ref and
`observation_id`. Supply an `expect_selector` for a unique visible element
that should appear after the click. The tool requests explicit approval and
uses the journal attempt as its request ID. If the outcome is uncertain, read
the receipt, take a fresh snapshot of the original tab, then reconcile with
the original selector. The tool router uses the runtime pinned to the chat.

Fill, press, select, and check also request explicit approval. Fill always checks value
readback, including when the caller omits `verify_value`. Take a fresh
snapshot before sending another action.

Kageha requests approval once per batch. It requests a separate, single-use
approval for a declared dialog response. When a Rust script handles a dialog,
declare the same response in the `browser_script` arguments and its batch click.
Kageha can reconstruct the report from
the application journal when the Rust daemon is unavailable. Use
`browser_receipts(attempt_id=...)` to recover the batch and get the receipt ID
for each step. Fill and verification values are masked in stored batch
arguments.
