# Independent integration transport

`kageha-integrations` is a Rust library with no runtime, WebUI or application
store dependency. Its pooled Reqwest client is now used by the retained OAuth,
media and channel adapters through the WebUI compatibility boundary. There is no
curl child process, temporary credential configuration file or shell transport.

`HttpClient::new(Config)` accepts an optional bounded PEM trust bundle. Configuration
is operator-owned; the library does not read application credentials or environment
files. The compatibility host preserves its configured KAGEHA_CA_BUNDLE,
CURL_CA_BUNDLE and SSL_CERT_FILE trust-root lookup. Explicit proxy routing supports
HTTP, HTTPS and the compiled SOCKS transport. The compatibility host preserves
http_proxy, https_proxy/HTTPS_PROXY, all_proxy/ALL_PROXY and no_proxy/NO_PROXY;
uppercase HTTP_PROXY remains ignored, matching curl’s CGI-safe convention.

Use `request_async` in async consumers. `request` bridges synchronous callers and
Tokio multi-thread blocking workers; current-thread reactors must use the async
method. Cancellation and deadlines interrupt both headers and body reads. Dropping
the final client shuts down its owned I/O runtime without requiring an async caller
to block. Requests are not automatically retried by this library.

Bodies stay binary. Non-success status/body are returned to the domain adapter;
a negative HTTP response does not imply the overall operation had no side effects.
Redirects are never followed. URL userinfo/fragments, malformed headers and
plaintext credential headers are rejected. Error messages do not include URLs,
headers, body bytes or upstream responses. Request/response bounds are explicit.
The caller remains responsible for domain permissions, exact destination authority,
secret form/query fields and durable mutation receipts. This transport is not an
agent tool or an approval bypass.

This is the first extraction slice, **not complete integration parity**. Domain
ports, owned OAuth/channel/media stores, proxy deployment conformance,
standalone integration service hosting and managed replacement remain. Provider
adapters and resource authority must not be inferred from an arbitrary endpoint.

```sh
cargo test --locked -p kageha-integrations
scripts/check_native_integrations.sh
```

Unit tests cover binary data, negative status, redirect isolation, limits,
cancellation and OAuth-style calls from the credential blocking pool. The retained
WebUI E2E exercises configured TLS, OAuth refresh/revocation races, model login,
media, channels and SSH while curl/Python are absent from backend PATH.
