# Kageha optional Python client

Python 3.10+ standard-library clients for the independent Rust agent runtime and
memory service. This package is never imported or launched by the backend.

```python
from kageha_client import AgentClient
client = AgentClient(endpoint, operator_token)
run = client.run(portable_run_spec)
with client.events(run["spec"]["id"]) as events:
    for event in events:
        print(event["sequence"], event["record"]["state"])
```

Also exposes status, steer, cancel, approvals, approval and decide. TypedDict records
and `py.typed` ship with the package. Approval decisions retain exact authority and
revision. Events are resumable lifecycle records; closing a stream does not cancel
its server run. Input is never automatically retried. See the repository's
`sdk/AGENT.md` for complete requests, pagination and recovery guidance.

Install locally with `python3 -m pip install ./sdk/python`. This is a local
prerelease SDK, not a published package release.

The standalone browser has an optional async client (`pip install './sdk/python[browser]'`):

```python
from kageha_client.browser import BrowserClient

async with BrowserClient(browser_url, token, workspace_id, "external-agent") as browser:
    selection = await browser.call("browser_selection")
```

This extra uses HTTPX. The browser implementation remains Rust, with fixed client
scope, epoch fencing, bounded responses and no automatic input retries. See the
[standalone browser guide](../../crates/kageha-browser/STANDALONE.md) for service
setup, receipts, and external MCP integration.

For the browser's native MCP stdio transport, install the independent optional
`browser-mcp` extra and keep an initialized official MCP session open across calls:

```python
from kageha_client.browser_mcp import McpBrowserClient

browser = McpBrowserClient(mcp_session, "my-workspace", "my-agent")
await browser.connect()  # discovers tools, validates launcher identity, pins epoch
selection = await browser.call("browser_selection")
```

The caller owns MCP connection/process lifetime. This bridge does not reconnect,
retry input, launch a Python backend or depend on LangGraph. It preserves native
errors and receipts.
