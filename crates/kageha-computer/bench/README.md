# macOS computer-use benchmark loop

The goal is to find failures, fix their cause, and rerun the original case plus fresh variants. Passing this suite does not establish that all computer-use issues are solved.

## Three evaluation layers

1. **Native runtime reliability — implemented here.** A Rust runner controls a disposable Swift/AppKit app through the actual standalone service. A separate app-state oracle grades results. No model, real account, browser draft, or existing document is involved.
2. **Kageha integration — a separate gate.** Run `crates/kageha-loop/tests/computer_host_e2e.mjs` for native tool discovery, policy, takeover and permission-probe integration. This currently does not measure full desktop streaming or model-driven GUI task completion.
3. **Full agent tasks — not yet run.** Use resettable macOS VMs, pinned tasks and a fixed model/configuration. [MacArena](https://github.com/MacPaw/MacArena) is the macOS-focused candidate; [OSWorld](https://osworld-v1.xlang.ai/) supplies the broader execution-based evaluation methodology. MacArena's published setup requires UTM and a prepared VM. Its external evaluation harness is separate from Kageha's Rust/Swift execution backend. No public benchmark score is claimed by this local suite.

## Run

```sh
cargo build -p kageha-computer --bin kageha-computer --example runtime_bench
cargo run -p kageha-computer --example runtime_bench -- \
  /absolute/path/to/target/debug/kageha-computer \
  /tmp/computer-bench-new-run \
  101,307,509,701,907
```

Use an absolute helper path and a new output directory. Each seed starts an owned helper and disposable app. At most 100 unique seeds are accepted. The helper must already have macOS Accessibility access; the runner never grants or prompts for it. Missing permission is **blocked**, counted in the denominator, and fails the suite.

The initial v2 AX suite had 14 cases per seed (the expanded v3 suite is described below):

- Complex Unicode roundtrip, including combining marks and emoji.
- Identically labeled controls in different windows.
- Checkbox and radio state changes.
- Refusal of disabled and secure fields.
- Superseded and consumed observations.
- Duplicate click IDs: exact receipt recovery and exactly one app callback.
- Moving an observed field to another window before input.
- Key refusal when another window has focus.
- Released-lease refusal.
- Receipt recovery after helper restart.
- Actual native click followed by deliberately discarding its response and killing the helper; recovery reads its journal without sending the click again.

Setup commands reset the app or inject a fault. They do not complete a task. All task input goes through the Rust runtime and Swift native adapter. Grading reads the fixture's own text/control values and callback counters, not AX readback or the service's success claim. The fixture is not a screenshot/model grounding benchmark.

## Output and gates

`report.json` contains per-case outcomes, oracle state, final receipt/error, tool-call counts, timings, seeds and executable/fixture hashes. `seed-*/results.jsonl` is flushed and synced after each case, so failures retain partial evidence.

A worker has a 60-second outer watchdog. Its process group contains only its fixture and helper. Timeout kills that owned group and leaves incomplete cases blocked. Normal workers clean up their own children; fixture stdin EOF also terminates its app. The benchmark does not pretend that the production synchronous SDK now has a hard I/O deadline.

The suite fails on any failed/blocked case, false confirmation, or helper binary changing during the run. Known native preflight refusals must report `not_dispatched`; returning `unknown` for these proven refusals fails the recovery-precision gate. True in-flight errors and missing completion records must remain unknown and must never be downgraded by matching an error message.

Call latency uses nearest-rank p50/p95 across both read-only and input calls. Task latency includes setup and independent oracle waits. These local debug-build measurements are not a production SLO, a Rust-versus-Swift comparison, or model latency. No latency gate is imposed before a representative workload/budget is agreed.

## First measured improvement

The initial safety-only smoke passed 42/42 but exposed 12 unnecessarily unknown refusal receipts. The stricter v2 gate was then frozen and rerun against the original helper before making the runtime fix.

| v2 evaluation | Passed | Failed | Blocked | False unknowns | False confirmations |
| --- | ---: | ---: | ---: | ---: | ---: |
| Original helper, development seeds 101/307/509 | 30/42 | 12 | 0 | 12 | 0 |
| Fixed helper, same seeds | 42/42 | 0 | 0 | 0 | 0 |
| Fixed helper, held-out seeds 701/907 | 28/28 | 0 | 0 | 0 | 0 |

Both comparison runs used the **same runner binary and fixture source**. See [comparison](results/2026-11-07-comparison.json), [original raw report](results/2026-11-07-before.json) and [fixed raw report](results/2026-11-07-after.json). Later harness changes add watchdog cleanup/provenance checks without changing these 14 task definitions.

The fix carries explicit pre-dispatch evidence from Swift validation through the compatibility protocol and Rust controller. It does not infer safety from error text. Failures inside an action remain uncertain. A durable regression test checks refusal receipts and duplicate recovery; existing unknown-outcome tests still pass.

The matched-call p95 was about 113 ms before and 115 ms after. This change improves recovery truthfulness; it is not a demonstrated latency improvement.

## Coverage still missing

A green AX v2 score is not a production release decision. Remaining benchmark groups include successful keyboard/caret workflows, text selection and insertion chains, long forms, scroll/drag/menu/dialog actions, actual save/open/export tasks with persisted-file evaluators, screenshots and mixed-DPI transforms, permission revocation, physical user intervention, long soak/resource limits, panel rendering, and full model-driven multi-app workflows.

Move cross-app destructive/settings tasks into resettable VMs. Record model, prompt, tool schema, app/OS versions, action budgets and all blocked tasks. Keep policy/human handoffs separate from control defects. Pin the benchmark release and rotate fresh held-out seeds/tasks after tuning; the seeds published above are no longer secret holdouts.

## Expanded v3 run — 2026-11-07

The initial v3 run below used **29 scenarios per seed**. The action-timeout regression extension described afterward adds a 30th scenario. The 14-case v2 results above are historical. V3 adds successful window/field focus and keyboard activation, Unicode selection/replacement followed by an explicit commit, ambiguous-selection refusal, a validated form saved to an independently decoded JSON file, rejected invalid saves, checkbox roundtrips, reacquisition freshness, foreign-window references, request-ID retargeting, restart-token invalidation, disabled/moved buttons, and three image/pixel scenarios.

Image cases use the separate `ImageOracle.swift` decoder to check actual PNG/JPEG decoding, dimensions and nonuniform content. They require Screen Recording. Missing access is blocked and counted, never silently skipped. The runtime does not prompt for or grant OS permissions.

| V3 run | Pass | Fail | Blocked |
| --- | ---: | ---: | ---: |
| Exploratory, 3 seeds | 69 | 9 | 9 |
| Corrected harness, original backend, same 3 seeds | 72 | 6 | 9 |
| Fixed backend, same runner/fixture/seeds | 78 | 0 | 9 |
| First 10-seed expansion | 259 | 1 | 30 |
| Build-overlap run, invalidated by helper hash drift | 260 | 0 | 30 |
| Frozen serial rerun, same 10 seeds | **258** | **2** | **30** |

Findings and fixes are kept separate:

- **Runtime:** disabled buttons exposed AXPress and were acknowledged despite being disabled. Observed disabled controls are now refused before dispatch.
- **Runtime capability:** an unfocused native text field could not accept selection. Added explicit `focus_element`, which requires the exact window already focused and verifies actual native focus. It never silently activates another application as a fallback.
- **Harness:** save scenarios initially shared a request ID, producing the correct conflict rejection but a misleading benchmark failure. IDs now include the scenario.
- **Workflow:** the initial long run found one field-focus request without verified window focus. The runtime safely refused it. The driver now explicitly focuses the window, re-observes, focuses the field, and checks each outcome before continuing. The failed run remains archived.

[Full summary and per-run provenance](results/2026-11-07-v3-summary.json) links compressed raw reports with SHA-256 hashes, oracle states, step traces, permission state and latency by operation. The matched backend comparison uses the same runner and fixture; the later workflow correction changes the driver, so its gain is not presented as a second backend fix.

Final trial seeds: `5003,6007,7001,8009,9001,10007,11003,12007,13001,14009`. The frozen rerun passed 258/260 runnable trials. Two invalid-save clicks (seeds `10007` and `11003`) returned macOS AX error `-25204` after dispatch began. Both remained `unknown`; neither produced a saved file or a confirmed save. No write was retried. The original test error said “promoted to a verified save,” which was misleading: the raw replies clearly show uncertainty, not confirmation. The diagnostic is now corrected without relaxing the pass condition. No false confirmations, false unknowns or worker crashes were recorded. **The full suite remains non-green:** these two intermittent native failures remain unresolved, and all 30 screenshot/pixel trials were blocked. Its exit code is nonzero by design. This is not an overall 100% score.

The preceding 260-pass run is preserved as `v3-build-overlap`, excluded from the final baseline because a concurrent Cargo regression build changed the helper hash. The frozen rerun has matching start/end helper hashes. Its call latency was p50 7.10 ms / p95 115.16 ms; these mixed-operation timings are not an end-to-end agent speed score.

Run live suites and Cargo builds serially: native CLI tests, the Rust-host integration test and this benchmark share the exclusive physical-desktop controller. A parallel test attempt was correctly rejected as busy; the regression suite was rerun after the benchmark released the desktop. Separate VMs are required for parallel desktop runs.

Successful control does not establish general agent reasoning, screenshot grounding, long-term soak stability or performance parity with another agent. Those remain separate benchmark layers.

## AX action-timeout correction

Snapshot traversal sets a 100 ms AX messaging timeout on observed elements. Native mutations previously inherited that read budget. The added `delayed_action_acknowledgment` fixture action deliberately processes for 250 ms; the original helper executed it but returned AX error -25204 and an unknown receipt on all three matched seeds. This reproduces the short-budget failure without depending on timing noise.

The adapter now configures a bounded one-second timeout for each AX mutation, sends the operation exactly once, and restores the short read timeout afterward. A timeout or other error after dispatch remains unknown, never automatically retried. Failure to configure the budget is a pre-dispatch refusal. The new scenario checks independent callback count and exact duplicate-ID receipt recovery to ensure one execution.

Using identical runner/fixture hashes and seeds 10007, 11003 and 17011, the old helper recorded 77 passes / 4 failures / 9 blocked; the patched helper recorded **81 passes / zero failures / 9 blocked**. Three baseline failures were the deterministic delayed action; the fourth observed a click counter exceeding its expected value, preserved in the raw evidence without claiming that it establishes a duplicate dispatch. [Matched reports and hashes](results/2026-11-07-action-timeout-summary.json).

The frozen ten-seed follow-up passed **270 runnable trials, zero failures, 30 permission-blocked trials** (300 total), including both previously failing seeds plus seven fresh seeds. Helper hashes matched at start/end; no worker failures, false confirmations or false unknowns were recorded. Capture remains unverified until macOS Screen Recording access is granted to the helper.

## Real GPT-5.4 agent-loop evaluation

The opt-in driver at `crates/kageha-loop/tests/computer_model_bench.mjs` runs the actual Rust host, OpenAI Chat Completions, standalone helper and disposable AppKit fixture. Set absolute `KAGEHA_LOOP_BIN`, `KAGEHA_COMPUTER_BIN` and `OPENAI_API_KEY`; optionally load an existing dotenv file through `KAGEHA_MODEL_BENCH_ENV_FILE`. Keys are never logged. Run from the repository root with Node. The relay exposes only computer tools and records the model's own decisions; it does not simulate provider responses. Only input for the exact owned process/window can be approved, once per action. The app's own oracle and saved JSON grade outcomes independently of assistant claims. Report directories are private and retained for investigation.

The initial real run served `gpt-5.4-2026-03-05` but completed **0/3 tasks**: form save, delayed clicks and continuation editing. All were stopped by exact-window observation failure before input; these dependent task failures do not measure GPT-5.4's general task ability. A fresh scripted reproduction also failed all 27 runnable trials (3 capture trials blocked). A separate direct Swift probe returned two AXWindows objects, but both identified as AXApplication with no window geometry. Adding failure geometry diagnostics made this mismatch visible without selecting a different window or weakening authority.

[Model evidence and native reproduction](results/2026-11-07-gpt54-agent-summary.json). These newer results supersede any interpretation of the earlier 270-pass runtime batch as proof of current end-to-end stability. The OS/fixture AX identity failure remains unresolved. No existing application, account or draft was modified. No claim of production readiness or screenshot performance follows.

## Ten-task real-model repeat after fixture correction

The signed app-bundle fixture resolves the earlier AXApplication/window mismatch in these runs. The model driver now packages, explicitly ad-hoc signs and verifies its own disposable app before starting the native test. The scripted native runner uses the same packaging/signature verification. This changes the benchmark target, not permissions or the exact-window control policy; it never signs a user's existing application. The exact macOS internals behind the unsigned target's failure are not established.

Real `gpt-5.4-2026-03-05` through Kageha's Rust loop passed **10/10 tasks twice: 20/20 attempts**, with zero recorded tool/provider errors. Each round starts a fresh app, isolated host state, chat and journal; the second uses a fresh fixture seed. Tasks cover Unicode field editing, enabling/disabling a checkbox, radio selection/return, persisted saves, a delayed click, a primary-window click with identical secondary-window labels, and preserving draft versus saved state across user turns. App callback counters and saved-file contents grade outcomes independently of assistant messages. Protected controls and the secondary window must remain unchanged. These are ten tasks in one controlled app, not ten general applications.

A signed-fixture native follow-up passed **81/81 runnable trials**, with nine capture trials still permission-blocked. No helper artifact drift, worker failure, false confirmation or false unknown was recorded. The original failed model/native reports remain retained. [Ten-task reports, prompts, traces, exact hashes and native regression](results/2026-11-07-gpt54-ten-task-summary.json). Full screenshot/multi-app performance remains unverified.

To run the ten-task driver again:

```sh
KAGEHA_LOOP_BIN=/absolute/path/kageha-loop \
KAGEHA_COMPUTER_BIN=/absolute/path/kageha-computer \
KAGEHA_MODEL_BENCH_SEED=31013 \
OPENAI_API_KEY=... node crates/kageha-loop/tests/computer_model_bench.mjs
```

Prefer exporting the key through the environment or using `KAGEHA_MODEL_BENCH_ENV_FILE` to read an existing dotenv file; do not put credentials into source control or shared shell transcripts. The script fails on task, tool/provider or binary-provenance errors. Reports are retained privately in the OS temporary directory.

## Cloudflare DeepSeek multi-turn suite

The live-model driver also supports `KAGEHA_MODEL_BENCH_PROVIDER=cloudflare` and `KAGEHA_MODEL_BENCH_SUITE=multiturn`. It reads `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` from the environment or the explicitly supplied dotenv file. The default Cloudflare model is `@cf/deepseek-ai/deepseek-v4.1-flash-1017`, served through the real Workers AI OpenAI-compatible endpoint; reasoning effort is explicitly `low` unless overridden by `KAGEHA_MODEL_BENCH_REASONING`. [Cloudflare's model and API contract](https://developers.cloudflare.com/workers-ai/models/deepseek-v4.1-flash-1017/) supports function calling and this reasoning setting.

This suite sends **ten sequential user messages in one chat**. It tests exact recall of the original field text without repeating it, corrections kept separate from saved state, restoration from conversation history, narrow changes preserving unrelated state, delayed actions performed once, a later commit, cancellation of another unsaved edit, and a final read-only audit. All turns are independently checked against app state and saved JSON. Read-only recall/audit turns must send no `computer_execute` calls. Provider traces record increasing user-turn counts and history hashes.

The baseline passed **3/10 checkpoints**. It confused “original title” with the OS window title and propagated the wrong text through later turns; it also attempted to reuse a consumed observation and requested a screenshot without permission. Production tool descriptions now spell out one-shot observation consumption, screenshot permission prerequisites, and OS title versus editable field values. Suite v2 refers explicitly to “Primary field text.” Because both the contract and prompt terminology changed, this is not a matched backend-only comparison or a claim that the model's general memory was fixed.

The corrected run passed **10/10**, making **50 real Cloudflare model calls with zero recorded tool errors and zero provider errors**. Both read-only turns sent zero input calls. Host/helper hashes stayed fixed within the run, and user-turn counts reached all ten messages. The secondary window, disabled controls and secure field remained untouched. [Failed and corrected runs, prompts, oracle states, traces and hashes](results/2026-11-07-cloudflare-multiturn-summary.json).

Run from the repository root with the configured provider credentials:

```sh
KAGEHA_MODEL_BENCH_PROVIDER=cloudflare \
KAGEHA_MODEL_BENCH_SUITE=multiturn \
KAGEHA_MODEL_BENCH_SEED=41011 \
KAGEHA_MODEL_BENCH_ENV_FILE=/absolute/path/to/existing.env \
KAGEHA_LOOP_BIN=/absolute/path/kageha-loop \
KAGEHA_COMPUTER_BIN=/absolute/path/kageha-computer \
node crates/kageha-loop/tests/computer_model_bench.mjs
```

These are ten dependent native-app checkpoints, not ten independent apps or screenshot tasks. Failed baseline reports remain preserved; a passing run does not establish universal error-free behavior.

## Real application walkthrough: TextEdit kickoff brief

`KAGEHA_MODEL_BENCH_SUITE=textedit-demo` opens a new TextEdit instance through LaunchServices, excludes pre-existing TextEdit process IDs, and creates only an empty temporary document as setup. The real Cloudflare model uses Kageha's computer tools to author, save, revise and audit the brief. No content is written by a shell, filesystem tool or simulated app action. The harness independently reads the file for grading and delivers a byte-identical copy afterward. The original finished document is left open in TextEdit.

Both recorded runs completed the three workflow stages. The first recovered from three rejected shortcut argument shapes. The repeat completed all three stages with exact file/editor equality after every stage, but recorded six rejected/recoverable tool calls including focus preflight refusals. Neither is described as an error-free tool run. All rejected key calls were pre-dispatch; exact-window authority was not bypassed to force a save.

The walkthrough exposed missing keyboard argument documentation, now clarified in the public Kageha tool description. Native observations now use the resolved AX window title when CG redacts it, so the computer status names the actual document. The Rust status still reports `previewAvailable:false` and `canControl:false`: live desktop streaming/manual controls are not implemented. The frontend now explains unsupported preview instead of displaying an indefinite waiting spinner. This was checked with eight ComputerPanel tests and TypeScript compilation; the native component passed 51 regression tests.

[Walkthrough evidence, recovered errors and hashes](results/2026-11-07-textedit-demo-summary.json). The completed text and verification summary are delivered under `artifacts/computer-use-demo-2026-11-07/`. Build checks used a frozen source/dependency snapshot to avoid an incomplete crate being concurrently created in the migration worktree. Existing running services and past chats were not replaced.

## Supervision and independent-agent acceptance (2026-11-07)

Evidence is preserved in `results/2026-11-07-supervision/`:

- `native_preview_recheck.json`: installed LaunchServices app reports Accessibility and Screen Recording granted; three independently decoded JPEGs; preview does not stale an input observation; preview survives release while old input is denied. Run `KAGEHA_COMPUTER_BIN="/absolute/Kageha Computer.app/Contents/MacOS/kageha-computer" node crates/kageha-computer/bench/native_preview_e2e.mjs`.
- `langchain_packaged.json`: real LangChain `createAgent` and Cloudflare DeepSeek control the standalone packaged helper through public MCP stdio. Both turns passed, zero tool errors, independent saved-file/app oracle. This does not use Kageha's agent loop. Install pinned dependencies with `npm ci --prefix crates/kageha-computer/bench/agent-integration`; run `bench/langchain_agent.mjs` with `KAGEHA_COMPUTER_BIN` and the provider environment file specified by `KAGEHA_MODEL_BENCH_ENV_FILE`. Dependencies and API credentials are not bundled into the runtime.
- `multiapp_baseline.json`: real Kageha/DeepSeek Calculator → TextEdit workflow. The initial 28-call turn budget was insufficient for phase 3. The phase-2 grader also incorrectly required case-sensitive headings; raw failure evidence is retained.
- `multiapp_restart_failure.json`: first resume made zero provider calls because a persisted environment-owned endpoint still referenced the previous relay port. Startup now refreshes environment-owned endpoints without overwriting user-owned provider configurations (unit tested).
- `multiapp_recovered.json`: resumed the same chat, runtime journal, app windows and saved document after the endpoint fix. Both remaining phases passed in eight model calls with zero tool errors. The saved memo equals the editor text: $1,296 subtotal plus $150 setup = $1,446. `BudgetMemo.txt` is the verified output. These reports do not claim a clean four-phase first-attempt score.

`computer_host_e2e.mjs` separately passes real native preview after takeover, continued user ownership across a new turn, default input denial, explicit enable/revoke, and no pending approval after a denied input. Its provider is a fixture, not a model benchmark. ComputerPanel has 11 passing UI regressions including takeover during an in-flight frame request. Standalone Rust tests pass 52 cases. In-panel remote mouse/keyboard control, physical human-intervention detection, stable distribution signing and hard transport deadlines remain separate production gates.

## Manual panel control (2026-11-07)

`crates/kageha-loop/tests/computer_manual_e2e.mjs` exercises the authenticated Rust host against an owned two-window AppKit fixture. It is an operator-control test with a fixture provider, not a model benchmark. Set `KAGEHA_LOOP_BIN` and `KAGEHA_COMPUTER_BIN` to absolute built executables, then run the script. `KAGEHA_MANUAL_SEED` changes the layout. Run all tests which launch real computer controllers **serially**: competing controllers are intentionally refused by the physical desktop lock.

[Preserved comparison](results/2026-11-07-manual-control/summary.json) records a failing pre-timing-fix run and four consecutive passing runs with the identical harness hash. Final seeds 61001/61043/61087/61103 each passed nine checks: cross-chat rejection, pixel click with duplicate replay returning the original receipt, conflicting request ID rejection, consumed-frame rejection, Unicode typing/Select All/replacement, independently verified drag selection, scrolling dispatch, explicit read-only refresh and rejection of its old frame, and input refusal after return/revocation. The secondary field and saved-file count remained unchanged. Runtime hashes stayed fixed during every final run.

The live tests exposed missing window metadata in PID mouse events, ineffective default PID shortcuts, stale NSWorkspace focus state, and intermittent delivery from inherited input state with immediate press/release. Manual mouse/key requests now explicitly use foreground delivery, fresh AX focus and window-stack guards, a private CGEventSource and short press/release intervals. No automatic retry switches delivery modes. The fixture also now supplies a standard Select All menu command; without it cmd+A did not define a valid document-app task. Earlier failing reports are retained; the four passes are bounded acceptance evidence, not a claim that all applications or human-intervention races are solved.

ComputerPanel has 13 passing regressions; request-ID fallback has three, including LAN HTTP. Rust host coordinate/intent validation has a separate passing unit test. Stable Apple signing is still pending: no valid signing identity is installed on this machine. Updating an ad-hoc signed app required refreshing only Kageha Computer's existing macOS grants, which were verified enabled for the final runs.

## Signing and update safety (2026-11-07)

`node crates/kageha-computer/bench/package_signing_e2e.mjs` passes 16 packaging fault-injection cases. Apple signing/notarization tools are mocked; the Darwin directory exchange is real. Coverage includes absent/ambiguous/wrong certificate types, Apple team pinning, preserving the installed executable on signing/notarization/assessment/promotion failures, mutually compatible update requirements, explicit identity migration, refusal of Developer ID-to-ad-hoc downgrade, retaining rollback bundles, concurrent installer locks, and symlink/unrelated-app refusal. These tests do not claim a successful Apple notarization.

`KAGEHA_COMPUTER_BIN=/absolute/existing/helper node crates/kageha-computer/bench/package_local_e2e.mjs` verifies real ad-hoc signatures on disposable copies, a same-identity update, refusal of an identity-changing update, deliberate migration with rollback, and an unchanged source helper. It does not install the test app or modify Keychain trust. The real test caught the `# designated =>` form of implicit ad-hoc requirements and the required `=` prefix for inline `codesign --test-requirement` expressions; both are covered in the fault-injection fixtures too.

Evidence: `results/2026-11-07-signing/`. The installed helper was preserved and still reports Accessibility and Screen Recording granted. Actual Developer ID signing and Apple notarization remain blocked because no valid certificate/private key or notarization profile is installed and the user has no Apple Developer account. The local checks and mocked release path are recorded separately.
