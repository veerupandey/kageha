# macOS computer-use benchmarks

The goal is to find failures, fix their cause, and rerun the original case plus fresh variants. Passing this suite does not establish that all computer-use issues are solved.

## Three evaluation layers

1. **Native runtime reliability — implemented here.** A Rust runner controls a disposable Swift/AppKit app through the actual standalone service. A separate app-state oracle grades results. No model, real account, browser draft, or existing document is involved.
2. **Kageha integration — a separate gate.** Run `crates/kageha-loop/tests/computer_host_e2e.mjs` for native tool discovery, policy, takeover and permission-probe integration. This currently does not measure full desktop streaming or model-driven GUI task completion.
3. **Full agent tasks — not yet run.** Use resettable macOS VMs, pinned tasks and a fixed model/configuration. [MacArena](https://github.com/MacPaw/MacArena) is the macOS-focused candidate; [OSWorld](https://osworld-v1.xlang.ai/) supplies the broader execution-based evaluation methodology. MacArena's published setup requires UTM and a prepared VM. Its external evaluation harness is separate from Kageha's Rust/Swift execution backend. No public benchmark score is claimed by this local suite.

## Run

```sh
mkdir -p output/benchmarks/computer
cargo build -p kageha-computer --bin kageha-computer --example runtime_bench
cargo run -p kageha-computer --example runtime_bench -- \
  /absolute/path/to/target/debug/kageha-computer \
  "$PWD/output/benchmarks/computer/NEW_RUN" \
  101,307,509,701,907
```

Use an absolute helper path and a new output directory. Create its parent directory before running. Each seed starts an owned helper and disposable app. At most 100 unique seeds are accepted. The helper must already have macOS Accessibility access; the runner never grants or prompts for it. Missing permission is **blocked**, counted in the denominator, and fails the suite.

The native fixture covers cases such as:

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
