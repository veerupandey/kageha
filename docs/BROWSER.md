# Kageha Browser Architecture

Kageha owns a private browser for the agent.
The agent never silently attaches to the user's Chrome/Comet profile.

## Selection policy

1. Use `web_fetch` for simple public pages.
2. Use `browser_open` for JavaScript or interactive pages — it starts the
   owned Lightpanda/Chromium. Skip `browser_connect` unless switching to
   comet/chrome after the user consents.
3. Start a private Lightpanda sidecar when the binary is on PATH
   (`KAGEHA_LIGHTPANDA_MANAGED=1`, default on). CLI research and DOM-only
   extraction stay on Lightpanda.
4. On the WebUI, an ordinary interactive URL task uses owned Chromium so the
   live panel can paint. The **desktop shell** (`desktop/`) instead attaches
   to an Electron `WebContentsView` (`embedded` backend) over CDP — that
   guest is the same document the agent drives. Hosted/browser-only clients
   keep the MJPEG screencast. Session extract/open reuses the warm session.
   `/research` keeps its own HTTP / Lightpanda pool so it does not steal the
   live tab.
5. If a CDP endpoint on the configured port is Chrome/Comet, ignore it on the
   auto route and keep a private profile (Lightpanda on `:9333` or Chromium).
6. If the page reports `login_required=true`, the workspace decision updates
   to a consented user-browser recommendation. The WebUI panel offers
   **Use Comet**, **Use Chrome**, or **Stay private**. Kageha never silently
   imports login cookies and never auto-attaches from `execution_browser_backend`.
7. `promote(target, url, consent)` switches lanes with a partial teardown:
   the URL and login state stay, the Lightpanda sidecar stays up, and an
   attached user browser is never quit. Lightpanda → Chromium needs no
   consent. Private → Comet/Chrome/CDP requires `consent=true`.
8. On servers without a GUI, remain headless.
9. When `/machine` is active, the Rust browser service attaches to headed
   Chromium on the chat's Linux display. The Browser panel and browser tools
   use that binding. The connection runs through Docker stdio and a host
   loopback relay. Build the workstation image from the repository root with
   `docker build -f docker/desktop/Dockerfile -t kageha-desktop:machine-v3 .`.

Every browser workspace exposes a decision object:

```json
{
  "backend": "lightpanda",
  "reason": "private_fast_default",
  "privateProfile": true,
  "requiresConsent": false
}
```

## Complete control

Agent tools cover the act loop:

- Snapshot refs: `browser_snapshot`, `browser_find`, `browser_click`, `browser_fill`, `browser_type`
- Pointer: `browser_click_xy`, `browser_hover`, `browser_drag`, `browser_scroll`
- Navigation: `browser_open` / `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`, `browser_stop`
- Inspection: `browser_highlight`, `browser_get_bounding_box`, `browser_evaluate`, `browser_cdp`, `browser_diagnostics`
- Session: `browser_tabs`, `browser_lock`, `browser_connect`, `browser_close`

Raw CDP is diagnostic. `Input`, `Browser`, `Target`, `Storage`, and cookie
mutations stay denied — use the high-level tools instead, including on the
owned Lightpanda/Chromium session.

The WebUI "Kageha Browser" panel is the human view of that same session.
Hosted clients see a CDP screencast and send coordinate click/drag/keys while
the user holds the lease. The desktop shell paints a real `WebContentsView`
(`persist:kageha` partition — never the user's Chrome profile). Take control
lets the user type into that document; Release ignores mouse events on the
guest so the agent can drive. `browser_highlight` still injects into the
page. `browser_lock` blocks the panel until the agent unlocks.

Start the shell with `npm start` in `desktop/` (it spawns `kageha webui` and
sets `KAGEHA_EMBEDDED_CDP`). `<webview>` is not used: Playwright does not
expose that CDP target type as a Page.

## Self-hosting defaults

The browser CDP endpoint should bind to loopback or a private network. Do not
publish it directly to the Internet. Put the Kageha WebUI behind TLS and an
authenticated reverse proxy when hosting it remotely.

Useful settings include:

- `KAGEHA_HEADLESS_BACKEND=lightpanda|chromium`
- `KAGEHA_LIGHTPANDA_MANAGED=1`
- `KAGEHA_LIGHTPANDA_BIN=lightpanda`
- `KAGEHA_LIGHTPANDA_CDP=http://127.0.0.1:9333`
- `KAGEHA_COMET_CDP=http://127.0.0.1:9222`
- `KAGEHA_BROWSER_ALLOWED_HOSTS=example.com,example.org`
- `KAGEHA_BROWSER_BLOCKED_HOSTS=localhost,169.254.169.254`
- `KAGEHA_BROWSER_IDLE_TTL=1800`

Keep private browser profiles separate from Comet/Chrome profiles. Use
Comet/Chrome only for tasks that need the user's existing authenticated state.

## Performance contract

The hot path should reuse a warm session, prefer accessibility snapshots, and
avoid screenshots unless the WebUI needs a live frame or the caller requests a
durable artifact. Preview frames are disposable transport data, not a new
artifact for every browser action. While a screencast subscriber is connected,
agent tools must not take an extra `page.screenshot` (M15).

Two benches stay separate:

- **Act-loop** (`kageha browser benchmark --kind act`) — connect, navigate,
  snapshot, click, one screenshot. Ordinary CI when a backend is installed.
- **Live view** (`kageha browser benchmark --kind stream`) — first paint, moving
  and idle FPS, input-to-frame, reconnect, promote. Nightly only.

Live-view timing numbers are **internal candidate gates**, not a product
comparison. Do not publish a competitor claim until a same-machine, same-site
run exists.

| Metric | Initial gate | Where |
|---|---|---|
| Preview tax while streaming | 0 screenshots | CI (hard) |
| First frame after open | <200ms | Nightly |
| Moving-page FPS median | ≥12 | Nightly |
| Idle FPS | ~0 | Nightly |
| Input → frame p95 | <100ms | Nightly |
| Reconnect | <2s | Nightly |
| Promote Lightpanda → Chromium | <400ms | Nightly |

Release tests (isolation, consent, no external `Browser.close`, latest-frame
overwrite, last-subscriber stop) run in default CI as M15–M22.

```bash
uv run pytest -q tests/test_browser_policy.py tests/test_browser_workspace.py tests/test_browser_sidecar.py tests/test_browser_control.py tests/test_browser_release_matrix.py
kageha browser doctor
kageha browser benchmark --kind act --target auto
KAGEHA_BROWSER_NIGHTLY=1 uv run pytest -q -m nightly
kageha browser benchmark --kind stream
```

## Annotations

The WebUI pencil button lets a user click an exact page coordinate and leave a
comment. Annotations stay in the session browser workspace and are available to
the agent through `browser_annotations`. They are review instructions, not
trusted webpage content; the agent should still verify the page before making
changes.

## Backend contributions

New backends should keep the browser tool contract stable and declare:

- whether they are private or user-profile based;
- whether they require a GUI;
- whether they support accessibility snapshots;
- whether they support screenshots and raw CDP;
- how they start, health-check, and shut down;
- their failure and fallback behavior.

Backend-specific code belongs under `src/kageha/harness/browser`; selection
logic belongs in `policy.py`, and policy behavior must remain side-effect free
and unit-testable. Lightpanda process management belongs in `sidecar.py`.
