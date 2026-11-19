# Plugin setup and account connections

Kageha imports supported OpenAI and Anthropic package contributions into its existing extension engines. Installation is not proof of host compatibility or account access. Catalog imports are labelled unverified, and unsupported host features remain visible as package warnings.

## User flow

Open **Settings → Extensions → Plugins → Set up** after installation. A task can also present the same card through `request_plugin_connection(server, purpose)`. The card asks for declared configuration, shows OAuth application prerequisites and the exact callback URL, and requires trust in the package before enabling its executable contributions. Never paste passwords, tokens or authorization codes into chat.

Choose **Connect account**, follow the provider's consent page in a normal browser, and return to Kageha. OAuth callback state is single-use and expires after ten minutes. Kageha initializes the server and lists tools before marking the connection Ready. Calendar also makes a reviewed read-only API probe. Generic verification never invokes an arbitrary tool. Ready establishes connection/tool discovery; it does not certify every action the plugin offers.

When setup was requested by an agent, the card resolves the pending approval after verification. The agent reopens the connection, refreshes tools, and continues its task. **Not now** defers setup. Cancellation invalidates late callbacks and token writes. Restarting during an SDK OAuth browser flow asks the user to connect again; the Calendar callback flow persists its verifier through a restart.

**Disconnect** removes account tokens and sensitive setup values, immediately blocks new agent calls, and invalidates previous session generations. It retains the deployment's OAuth application registration. Disconnect cannot undo a provider request already in flight. Provider-side revocation remains available in the account's application settings. Package removal and disabling also block calls. Updates that change server execution/configuration invalidate prior setup.

## Supported setup paths

- Local stdio servers: declared typed `userConfig` plus inferred `${ENV_NAME}` placeholders, package-relative execution, real MCP initialization/tool discovery. Commands execute on the Kageha host, including when that host is a container; this is not the desktop bridge container.
- Remote SSE and Streamable HTTP: configured headers/API keys or the pinned official MCP SDK's OAuth flow, protected-resource/issuer discovery, PKCE, pre-registered clients, supported dynamic registration and client metadata documents.
- Google Calendar preview package (`https://calendarmcp.googleapis.com/mcp/v1`): explicitly labelled **Kageha Calendar API**, implemented through the standard Calendar REST API. Read-first consent supports listing calendars/events and availability. Optional event-write consent adds create/update/delete tools. This adapter does not claim access to Google's preview MCP service.

Host-specific app mappings and foreign hooks cannot be silently translated into working connectors. Servers that depend on unsupported host behavior, interactive terminal login, or server URL/form elicitation are not certified by this flow. They must return actionable errors or remain unverified; add a reviewed adapter and integration test before advertising support. There is no blanket promise that every catalog package works.

## Deployment prerequisites

Set `KAGEHA_PUBLIC_URL` to the externally reachable HTTPS origin for phone, remote or Docker deployments, for example `https://agent.example.com`. Proxy `/api/plugin-connections/callback` to the backend. Register exactly the callback displayed in Application setup with the provider. Local development defaults to `http://localhost:5173/api/plugin-connections/callback`; it cannot complete sign-in initiated on a phone. Proxy headers do not select OAuth callback URLs.

Providers without dynamic registration need a Kageha application/client ID. Imported sample client credentials are not reused. For Google, enable the Calendar API in that application's project, configure the consent screen/test users or required provider verification, and register the callback. Account sign-in and Google's deployment requirements must be satisfied before real Calendar use. Optional `KAGEHA_OAUTH_CLIENT_METADATA_URL` supplies an administrator-managed HTTPS client metadata document for servers supporting CIMD.

Persist the complete `KAGEHA_HOME` across restarts, including `integrations/key` and encrypted connection data. Losing the encryption key requires reconnecting. This is the existing single-user encrypted integration store; no separate vault service was added. Backend processes sharing this home use file locks to serialize rotating refresh tokens. Different homes/replicas do not share credentials. HTTP OAuth discovery and browser authorization links require public HTTPS endpoints and reject private DNS addresses. Restrict outbound networking at deployment boundaries as additional protection against DNS rebinding.

## Verification

`tests/test_plugin_connections.py` covers package normalization, encryption/redaction, typed configuration, callback replay/cancellation, expiry across restart, SDK discovery/PKCE/token exchange/refresh, remote callback prerequisites, private endpoint rejection, Calendar permission gates, real stdio verification, stale session revocation, HTTP routes and an agent pausing/resuming with refreshed direct tools. `PluginConnectionCard.test.tsx` covers the shared UI and deferred/automatic-resume behavior.

SDK OAuth tests use controlled HTTP responses and the real SDK. Stdio tests use a real subprocess. These are not substitutes for signing in to each production provider. A live Google account has not been connected by automated tests; production registration and human consent are required.

The browser smoke check is reproducible with `.venv/bin/python scripts/check_plugin_connections_ui.py` while frontend/backend run on ports 5173/8788 and Playwright Chromium is installed. It creates an isolated temporary Kageha home/backend, routes only plugin API calls there, verifies trust → actual stdio server → Ready → updated Plugins list → phone disconnect, and deletes the temporary environment. It does not alter installed user plugins or connect real accounts.

Packaging excludes frontend development dependencies while retaining built assets, following [uv's build-backend exclusions](https://docs.astral.sh/uv/concepts/build-backend/#file-inclusion-and-exclusion). A live public OpenAI documentation MCP server was initialized and listed five tools; transport cleanup was also verified across separate tasks. This confirms remote transport wiring, not third-party account consent.
