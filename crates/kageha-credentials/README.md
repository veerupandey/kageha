# Reusable credential vault

`kageha-credentials` is an independent Rust library. Its default build has no HTTP host, application database, agent runtime, WebUI, or Python dependency. Enable `service` for the Axum executable and `client` for authenticated Rust clients; enable both to host a replaceable composition.

## Embedded and service use

```sh
cargo run -p kageha-credentials --example embedded
cargo build -p kageha-credentials --features service,client
cargo test -p kageha-credentials --all-features
```

Use the ports in `kageha_contracts::credentials`: resolver, administrator, repository, key protector, OAuth refresh, transfer, and migration. `Vault` implements the same credential ports as `client::Client`. `registry::Registry` provides the stable logical binding consumers retain across switches. `client::RoutedClients` associates each application workload with a separately authenticated service connection. `config::Connection` reads protected token/identity files; transport identity always comes from IPC authentication or mTLS, never a request body's principal.

A `SecretLease` is explicitly exposed to trusted adapters, redacted in Debug, non-serializable, zeroizing, and valid for at most 60 seconds. Resolve immediately before dispatch, then validate admission before sending input. The explicit direct-key model constructor remains available for external in-memory callers. A vault error or tombstone never selects an environment fallback.

## Operator configuration

Create a directory owned by the service account with mode `0700`. On macOS the default key protector provisions a missing master key in Keychain, only for a new vault. Headless/Linux operators must mount an explicit 32-byte binary key file owned by that account with mode `0600`. No plaintext key is generated beside the vault. Keychain denial, a wrong key, and unsafe file permissions leave storage locked.

The CLI accepts an owner-only JSON configuration, for example (all paths absolute):

```json
{
  "vault":"/private/state/vault.sqlite",
  "key_id":"master-v1",
  "key_files":{"master-v1":"/run/secrets/credential-key"},
  "socket":"/private/state/credentials.sock",
  "grants":[{
    "principal":"model-adapter","namespace":"models","ids":["primary"],
    "purposes":["model"],"destinations":["https://api.example.com"],
    "permissions":["metadata","resolve"]
  }],
  "clients":[{
    "principal":"model-adapter","authentication_generation":1,
    "uid":501,"local_token_sha256":"SHA256_OF_A_RANDOM_SCOPED_TOKEN"
  }]
}
```

Configure separate grants and tokens for writes, revocation, exports and administration. The service accepts only a private Unix socket, checks peer UID, and binds tokens to server-side identities. It bounds connections, requests, request bodies, response sizes, rate and deadlines. Credential responses, including failures, use `Cache-Control: no-store`. Do not add credential-body access logging in a reverse proxy.

Hosted configuration replaces `socket` with `bind`, `server_cert`, `server_key`, and `client_ca`. Every client presents a valid certificate from `client_ca`; each leaf certificate SHA256 fingerprint must also map to a configured workload in `clients`. This token/certificate is separate from Kageha's server token. There is no unauthenticated HTTP network mode.

```sh
kageha-credentials serve --config /private/state/host.json
kageha-credentials status --config /private/state/host.json
kageha-credentials migrate --config /private/state/host.json --input /private/state/import.json
kageha-credentials rotate-key --config /private/state/host.json --next-key-id master-v2
kageha-credentials export-recovery --config /private/state/host.json --namespace models --passphrase-file /private/state/passphrase --output /private/state/recovery.json
kageha-credentials restore --config /private/state/empty-host.json --passphrase-file /private/state/passphrase --input /private/state/recovery.json
```

Passphrases are protected file input, never argv values. Exports use bounded Argon2id parameters and authenticated encryption. Restore requires an empty destination and advances client authentication authority: authorize new client generations before serving. Rotation is a single atomic rewrap of the bounded vault snapshot: after interruption the old or new complete generation is visible; rerunning with the same target key resumes safely. Retain the previous key until rotation verification and backup policy allow retirement.

`migrate` imports a bounded private JSON array of `operation_id`, `reference`, `kind`, `expected_revision`, `expires_at_ms`, and `secret_base64`. Operation IDs are retained; query `/v1/operation` after an unknown write response before retrying. The generic CLI intentionally leaves the source file to its owner. Kageha's importer verifies encrypted data, then publishes application references and deletes corresponding legacy rows. Cross-database publication failures fail closed and can reconcile the confirmed operation; they do not guess another credential.

## Managed replacement

Register endpoints through operator-owned configuration; clients cannot supply arbitrary network destinations. A host built with both features accepts:

```json
{
  "registry":{
    "journal":"/private/state/bindings.sqlite",
    "embedded_id":"embedded","device_id":"my-device",
    "admin_principal":"replacement","admin_purpose":"replacement",
    "backends":[{
      "id":"remote","device_id":"hosted-device",
      "workloads":{
        "replacement":{"transport":"unix","socket":"/private/service/credentials.sock","token_file":"/private/service/replacement.token"},
        "model-adapter":{"transport":"unix","socket":"/private/service/credentials.sock","token_file":"/private/service/model.token"}
      }
    }]
  }
}
```

This is an additional field in the host configuration above. Both endpoints require explicit system-admin and per-namespace export/write grants for the replacement workload, purpose and backend IDs. Preserve application workload identities, purposes and destinations on both backends. Management clients must set `management:true`. HTTPS connections use `endpoint`, `identity_file`, and `ca_file` instead of socket/token paths. Certificates, tokens, and private configuration files must be owner-only.

`POST /v1/replacements` accepts a purpose, destination ID and action: `binding`, `request`, `status`, `resume`, `rollback`, or `retire`. Request/rollback include an explicit decision `{id,destination,allow_browser_transfer}` and operation ID; rollback also includes the verified previous operation ID. Inspect blockers and resume the same operation after recovery. The coordinator copies state, fences the source at a quiescent checkpoint, catches up records/tombstones/receipts/audits, atomically commits its binding, activates and verifies the destination. OAuth exchanges that might have dispatched block cutover until explicitly resolved. Old generations cannot admit writes. Rollback transfers confirmed post-cutover changes rather than restoring an old snapshot. Previous encrypted journals remain retained after retirement.

Kageha uses this registry behind existing credential settings. `KAGEHA_CREDENTIAL_BACKENDS` points to an owner-only JSON array of remote-backend definitions with workloads `models`, `oauth`, `mcp`, `media`, `channels`, `migration`, and `replacement` as needed. The host exposes the same actions at `/api/credentials/replacements`, preserving active chats. Settings continue to accept write-only inputs and return status/references. Voice recordings are artifacts, not credential records. Browser password references retain exact HTTPS-origin/workspace/account checks and default to their device-local encrypted vault.

## Versioned protocol

The `/v1` API provides info, metadata, resolve, put with CAS, revoke, operation status, lease check, OAuth refresh, audit pagination, administrative control, authenticated state transfer, and replacement jobs. Only trusted adapters receive secret-bearing resolve/export transport replies. Do not give resolve/export capabilities to tool discovery, agents, or the browser frontend.

Rust clients implement the domain ports. TypeScript uses `sdk/typescript/credentials`; optional Python uses `kageha_client.credentials`. Python is not part of backend installation or operation. Unknown dispatch outcomes are explicit and never automatically replay a refresh-token exchange.

## Security and release notes

Payloads and per-record data keys use RustCrypto XChaCha20-Poly1305 with random nonces. Associated data binds vault, reference, namespace, kind, revision, expiry and state. Receipt fingerprints are keyed HMACs; audits do not contain secret values or unkeyed secret hashes. Portable recovery bundles have resource limits. The library cannot protect plaintext from a compromised authorized adapter or promise the elimination of all runtime/HTTP buffer copies.

Deleting a SQLite row does **not** erase historical backups, WAL remnants or storage snapshots. Treat previous backups and legacy credential keys under the operator's retention policy. Browser login cookies are not copied by this migration.

Run `cargo test --locked -p kageha-credentials` before changing vault storage or transport. Native Windows key protection, multi-tenant SaaS, HA and general authenticated-request proxying remain outside scope. Do not infer a production release from compilation alone; missing acceptance evidence remains a release blocker.

A container build and single-tenant Compose example live in `deploy/`. Provision mounted directories/files as UID 10001 with modes 0700/0600; the daemon never repairs unsafe key permissions. Use mTLS configuration with `bind: "0.0.0.0:8443"`; the sample publishes only loopback until an operator deliberately exposes it. Authenticated `/v1/info` is the service health endpoint; CLI `status` checks storage/key availability. SIGTERM/SIGINT stop admission and drain bounded requests before exit. Disposable container execution is a separate validation gate from the macOS tests.

For browser passwords, `KAGEHA_BROWSER_CREDENTIAL_SERVICE` may name an owner-only `config::Connection` JSON file pointing to a device-local Unix service. Grant the `browser-device` workload the exact namespace `browser:SHA256(JSON([workspace_id,https_origin]))`, purpose `password_fill`, destination origin, and needed reference IDs. Direct hosted browser-password transport is rejected. Existing legacy browser entries are not silently moved into a configured service; use an explicitly approved state transfer first. Both embedded and local-service browser flows have disposable account/password-fill fixtures.

`service_options.requests_per_minute` sets a bounded per-workload admission budget (default 6000; accepted range 1–60000). Administrative `/v1/info` replies expose counters without values. HTTP 429 is never interpreted as a credential fallback. An authenticated `cancel` replacement action is allowed before binding commit; after commit use rollback, which catches up new writes. Cancellation retains the quarantined destination rather than erasing its staged data.

Kageha's `/api/credentials/revoke` accepts a setting `name` and creates a durable tombstone; it never returns a stored value. Reference projections reject older receipts, including late responses after a newer revocation. Managed model references reject empty keys instead of silently becoming anonymous. Standalone model environment sources resolve per call; legacy alias precedence selects an explicit source rather than retaining a startup key string. Model resources resolve from configured/installed paths, not the compiled checkout; an original checkout `.env` is never implicitly loaded. Keep service tokens and key files outside untrusted agent workspaces.

OAuth identity details are encrypted snapshots, including legacy settings history. Reconnect creates a new opaque authorization ID; refresh and storage/key rotation preserve it. Integration access lists remain bound to the prior authorization until a successful account check confirms the current identity, preventing a failed or late profile response from carrying permissions to another account. The trusted `oauth` application workload needs namespace-scoped `export` in addition to its usual grants to inspect expired authorization state; model and WebUI callers do not receive that transport capability.

The Kageha importer inventories legacy `ui_secrets`, split OAuth grants, pending OAuth verifiers, account metadata/history, and MCP environment/header bundles before publishing references. Current and historical MCP bundles move to encrypted records; ordinary server commands, arguments and discovery metadata stay configuration. Voice bytes move to hash-verified artifacts. Deterministic import identities and verified projections make retries safe. A conflict with existing encrypted state blocks migration instead of overwriting it. Metadata readiness becomes unavailable when a managed credential is revoked, expired, locked or not yet published; it does not select an older credential source.
