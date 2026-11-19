---
name: web_browse
description: Fast human-like web browsing — HTTP first for static text, browser_open for interactive pages (private session). Never attach to the user's Chrome unless they name comet/chrome or login cookies are required.
triggers:
  - open the site
  - open website
  - log in
  - login
  - fill the form
  - browse to
  - navigate to
  - browser_open
  - use comet
  - interact with the page
  - click through
allowed-tools: web_fetch browser_connect browser_open browser_snapshot browser_inspect browser_wait_for_control browser_receipts browser_reconcile browser_find browser_click browser_type browser_fill browser_select browser_check browser_upload browser_press browser_scroll browser_wait browser_screenshot browser_evaluate browser_script browser_batch browser_cdp browser_diagnostics browser_annotations browser_tabs browser_lock browser_close browse_logged_in
---

# web_browse

Per-tool mechanics (snapshot refs `e0`, `text=` / `role=button:Name` targets,
`<select>` handling, upload, scroll) are in each tool's own description, which
the model already receives. Screenshot rules are in the always-loaded operating
policy. This skill covers routing, session discipline, and pitfalls from real runs.

## Routing

- Static docs, articles, READMEs, search answers → `web_fetch` / `research_run`.
  Do **not** call `browser_open` just to read text.
- Interactive sites, dashboards, forms, JS apps, clicks → `browser_open` then
  these tools. Reuse the selected task browser. With no selection, let the runtime
  choose a private backend; watching needs a paintable surface. Do not override a
  consented panel selection with a stale backend preference.
- Standard UI tasks use browser controls, including Next/Previous pagination.
  Use `browser_evaluate` for explicit scripting or bulk extraction when appropriate.
  One missing immediate page change is not a reason to switch to scripts;
  a repeated one is (see the escalation ladder).
- Search-only answers → `web_research` (it may call these tools itself).
- Native macOS apps → `computer_use`.
- User CDP (`comet` / `chrome` / `:9222`) only after the user chooses
  **Use Comet** / **Use Chrome** in the panel, or names that browser in the
  prompt and then consents. `login_required=true` is a recommendation, not
  permission to attach. `auto` may follow an already consented panel selection;
  it never creates consent or chooses a new personal profile.

## Session discipline

- To show a generated chat HTML artifact, focus Files with `workspace_panel`
  when available and identify the artifact by path. The Files preview can run
  self-contained HTML without connecting a signed-in browser.
- When an existing session or a user-requested browser matters, inspect the
  selected profile and managed tabs with `browser_tabs` and reuse the relevant
  tab. A profile name does not establish that the site is signed in: inspect
  the page. Respect the panel choice and never attach a personal profile
  without consent. For a live page, confirm the intended tab handle and URL
  with `browser_tabs`. If the selected profile needs
  reconnecting, show Browser and point to Reconnect. A shell/CDP screenshot
  does not establish that the chat's right panel is displaying the page.
  If you already have the exact tab ID, current HTTP(S) URL and title of an
  externally opened page, an approved Rust `browser_script` can call
  `browser.attachTab(tabHandle, url, title)` and return `browser.previewReady()`.
  Use Files for generated local HTML.
- With a panel handoff use `browser_connect(target="auto")` to follow that exact
  session. Otherwise call `browser_open(url)` first. It starts a private browser. Do **not** call
  `browser_connect` unless the user named Comet/Chrome/CDP or login cookies are
  required.
- `browser_click` performs one execution attempt. If the result is uncertain,
  inspect before retrying. A missing immediate DOM change does not prove failure.
  `browser_snapshot` is fresh by default; request it when the expected result is missing.
  When an AX delta is returned, only refs listed as removed are invalidated.
- `browser_fill` / `browser_type` do **not** return a snapshot. If the fields
  are already on the current snapshot, one `browser_batch` (one snapshot at
  the end). For a single fill, call `browser_snapshot` only if the next click
  needs new refs.
- Do not screenshot unless asked.
- Keep `browser_wait` short — 200–1500ms. Long waits are usually the wrong fix.
  If a snapshot is only nav/sidebar but the live view or Text preview already
  shows the table/data, do **not** wait again — read it and act.
- Prefer `browser_batch` for 2–30 predictable actions from one snapshot: it
  observes once at the end instead of per action. It stops on failure by default;
  continue after an error only when the remaining actions are independent.
- `browser_evaluate` is the extract path. After you can see the grid, collect
  rows in one script. If it throws, you get a JS exception + stack — fix the
  expression only after checking whether effects occurred. Prefer `browser_inspect`
  for read-only DOM and form inspection, including while a receipt holds input.
  `count: 0` or `truncated: true` means not done.
  Confirm the count against the page before claiming the extract finished.
- `browser_find` searches the last snapshot and unique `role=` / `label=` / `placeholder=` locators. It does **not** take a new Accessibility tree. `count: 0` means snapshot only if the control is new.
- If a fill reports `focus_blocked_by_modal`, inspect the current page for the
  active dialog and resolve it before filling the form. For a cookie-consent
  dialog, prefer rejecting optional cookies when that choice is available. Do not
  enter data behind a dialog or repeatedly retry a blocked fill.
- Sparse AX does not prove loading, hydration or failure. Use `browser_inspect` to
  inspect visible form controls and child frames; wait for the specific needed
  control. Use visual evidence for genuinely visual content. Refresh the snapshot
  after the relevant page condition changes.
- With the Rust runtime, use the control's snapshot `frame_id` for actions inside
  iframes. Keep the same frame for a click's selector verification. Use
  `browser_select` with the exact option value for a native single-select and
  `browser_check` with the intended boolean for a native checkbox.
- For a known native JavaScript dialog triggered by a Rust main-frame click,
  declare its URL, type, exact message and response in `dialog`. Kageha
  requests approval for that response separately. For a Rust script, declare the
  same response in the script arguments and its batch click. Inspect the
  receipt before continuing if the click outcome is uncertain.
- On a selection conflict use `auto` to follow the current panel. Only an explicitly
  authorized switch should pass the returned `selection_revision`; never guess it.
- On human takeover use `browser_wait_for_control`, then observe and continue the
  existing task. Revocation remains an explicit access boundary.
- The first snapshot lists **Ready locators** for unique named controls — click `role=button:Name` immediately. A later unique hit is `replay: true`. If `replay: heal`, the cache missed and resolved once; keep going.
- A click that returns `filechooser: true` already intercepted the OS picker — call `browser_upload(paths)` next (omit the input target). Do not click the picker again.
- `browser_wait` accepts CSS, named locators, or visible text (`Success`). Do not invent a CSS selector for a status string.
- `browser_batch` is for click/fill/type/select/press/scroll/wait/upload sequences
  only. For a URL, always call `browser_open(url)`; never encode navigation as
  `{action: "navigate"}` inside a batch.
- Never use `browser_cdp` for navigation, clicking, typing, or page search.
- App bugs or slowness → `browser_diagnostics`.

## Escalation ladder: when the UI tools are stuck

Finish the task. Move one rung down only when the current one keeps failing
on the same page:

1. `browser_snapshot` / `browser_find` → `browser_click` / `browser_batch`.
2. `browser_inspect`: fixed read-only queries for frames, visible controls and
   values (opt-in, credentials masked). Use `browser_evaluate` only when custom
   JavaScript is needed; exceptions can follow external effects.
3. **`browser_script`**: check the tool schema for the active runtime. Use it
   after 2–3 failed or no-effect actions
   in a row (the tool results say `escalation:` when this happens), or right
   away for custom dropdowns, drag-and-drop grids, shadow DOM, virtualized
   lists, rich-text editors, or React inputs that ignore `fill`.
   - On Rust, pass `language="javascript"` and a synchronous `code` body.
     Use `browser.open(url)`, `browser.navigate(url)`, `browser.tabs()`,
     `browser.select(tabHandle)`, `browser.snapshot()`, `browser.wait(ms)`,
     and `browser.batch(steps)` with the same step types as `browser_batch`.
     Open selects the new tab. To follow a popup, find its tab by `opener_id`
     and select its `target_id`. Return a small JSON value.
   - For a click that navigates the selected tab, use `expect_url` with the
     exact destination and an optional `expect_selector` on that page.
     A Rust click scrolls the target into view and uses its requested timeout.
     If navigation remains uncertain, inspect the original tab and reconcile
     using the expectation from the receipt.
   - On Rust, save larger results with `browser.artifacts.writeText(name, text)`
     or `browser.artifacts.writeJson(name, data)`. Use
     `browser.artifacts.screenshot("page.jpg")` for the selected viewport.
     Use a filename without directories. Exports appear in the chat's Files panel.
     In Rust's Private browser, use `browser.artifacts.download(target, name)`
     for a visible direct download link. Check the receipt after an uncertain click.
   - On Rust, use `browser.upload("input[type=file]", ["inputs/resume.pdf"])`
     to attach a chat file in the selected tab. Use a unique selector and a
     chat-relative path under `inputs/`, `artifacts/`, or `outputs/`. Pass
     the exact destination origin as `upload_origin` and the same ordered list
     of existing files as `upload_paths` on the tool call. Each approval covers
     one attachment. Save any files created by a script before requesting
     approval in a later call. The
     receipt verifies the file input. Verify the site's own result after any
     separately authorized submission. Check the page and receipt if attachment
     is uncertain before attempting another upload.
   - On Python, pass `code=...`, an async Python body with top-level await. Use
     `return await main(page)` for a helper, never `asyncio.run(main(page))`.
   - Python pointer and keyboard events: `page.get_by_role(...).click()`,
     `page.keyboard.type(...)`, `page.mouse`. Sites that ignore synthetic
     `el.click()` accept these.
   - Wait on the exact condition. Use bounded snapshot polling on Rust and
     locator waits or `page.evaluate` on Python.
   - Verify inside the script and `return` the evidence (new value, dialog
     text, row count). One script can do a whole multi-step flow.
   - The Python worker can save large data to `artifacts / 'name.json'`.
   - After it runs, earlier refs are stale: `browser_snapshot` before refs.
4. If a script fails, inspect its receipts and effects before deciding to retry.
   A rejected preflight sent no input: correct its schema/async body and continue.
   Preserve the objective; do not ask the user to restate it for a tool defect.
   Leave unknown effects protected and continue independent work when possible.

In the Python runner, when a script opens a popup or a result tab, use `await follow(result_tab)`
to show that exact tab in the right panel while the script continues.
`follow` also accepts a CDP target ID belonging to the managed session.
For multiple new tabs, choose explicitly; otherwise the original tab stays
selected. Script-created tabs remain available through `browser_tabs`.
If a script fails or times out, inspect its effects before deciding to retry.

Never write a separate `cdp_*.py` / Playwright file and run it with `bash`
against the user's Chrome. It can grab the wrong tab, the live panel can't
follow it, and it bypasses the browser lease. `browser_script` is the
supported way to script the page. It is not available on `/machine`.

## Comet / login cookies

Do **not** call `browser_connect(target="comet")` or `target="chrome"` just
because `browser_open` reported `login_required=true`. Wait for the user to
choose Use Comet, Use Chrome, or Stay private. Only attach a signed-in
browser after that choice or when they named the browser and consented.
Otherwise stay on the private profile. Do not auto-attach to `:9222`.

`browse_logged_in(url)` is a one-shot screenshot fallback.
Backend override: `KAGEHA_BROWSER_MODE=auto` (default) | `comet` | `chrome` |
`headless`.

## Learned pitfalls

- **Login-gated sites:** if `web_fetch` redirects to a login page, try
  `browser_open(url)` on the private browser. If login is still required, do
  **not** call `ask_human` in the same turn and do **not** attach to the user's
  Chrome. The panel asks the user to choose a signed-in browser or stay
  private. Continue with public findings until they choose.
- If the private browser is unavailable, `browser_open` still starts a private
  Chromium session. Never attach to `:9222` Chrome/Comet on auto.

## Fast browser lane

- A request that says “use the browser”, “open in browser”, “show the live view”,
  “take control”, or asks for a screenshot/video uses a paintable lane
  (Chromium or embedded).
- Ordinary URL tasks start on Lightpanda and escalate when the panel is watched.
- For an ordinary URL with no existing session to reuse, open it promptly with
  `browser_open(url)`. When an existing session or named browser matters, check
  `browser_tabs` first and respect the panel selection and consent boundary.
- Reuse the active browser session and prefer `browser_batch` for predictable
  multi-action sequences.
- On `/machine`, Chromium runs inside the box — every extra snapshot/wait/connect
  is expensive. Batch actions, skip redundant `browser_snapshot` after clicks,
  and never reconnect between steps of a long task.
- If `browser_open` is DENIED because the host cannot attach, stop and tell the
  user. Chromium is already in the box — do not `uv sync`, pip, or
  `playwright install` from bash.
- For **redirect / abuse-report proof** on `/machine`, call
  `capture_redirect_evidence(url, expect_host=…)` instead of bash ffmpeg +
  Playwright sidecars. Never `pkill chromium` or launch Chromium from bash.
