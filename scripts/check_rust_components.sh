#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
component_bin="$CARGO_TARGET_DIR/debug"
export KAGEHA_LOOP_BIN="$component_bin/kageha-loop"
export KAGEHA_RUNTIME_BIN="$component_bin/kageha-runtime-service"
export KAGEHA_MEMORY_BIN="$component_bin/kageha-memory"
export KAGEHA_AGENT_CLIENT_BIN="$component_bin/examples/client"
cargo test --locked -p kageha-browser-client -p kageha-contracts -p kageha-policy -p kageha-sandbox -p kageha-runtime -p kageha-runtime-store -p kageha-models -p kageha-mcp -p kageha-tools -p kageha-agents -p kageha-workspace -p kageha-execution -p kageha-integrations -p kageha-agent-sdk -p kageha-runtime-service -p kageha-workflow --features kageha-workflow/sqlite -p kageha-memory -p kageha-services -p kageha-loop
cargo test --locked -p xtask
cargo run --locked -p xtask -- boundaries
cargo build --locked -p kageha-loop -p kageha-memory -p kageha-runtime-service -p kageha-workspace
cargo build --locked -p kageha-models --example review_eval
cargo build --locked -p kageha-agents --features sqlite --bin kageha-agent-profiles
node crates/kageha-agents/tests/repository_e2e.mjs
cargo build --locked -p kageha-mcp --example fixture
node crates/kageha-runtime-service/tests/mcp_e2e.mjs
node crates/kageha-runtime-service/tests/memory_e2e.mjs
node crates/kageha-runtime-service/tests/workspace_e2e.mjs
node crates/kageha-runtime-service/tests/hooks_e2e.mjs
node crates/kageha-runtime-service/tests/profiles_e2e.mjs
KAGEHA_TEST_PROFILE_REPOSITORY=1 node crates/kageha-runtime-service/tests/profiles_e2e.mjs
cargo build --locked -p kageha-runtime-service --example planning_fault_fixture
node crates/kageha-runtime-service/tests/planning_e2e.mjs
node crates/kageha-runtime-service/tests/execution_e2e.mjs
KAGEHA_LOOP_BIN="$component_bin/kageha-loop" node crates/kageha-loop/tests/native_adapters_e2e.mjs
KAGEHA_LOOP_BIN="$component_bin/kageha-loop" node crates/kageha-loop/tests/webui_runtime_e2e.mjs
KAGEHA_LOOP_BIN="$component_bin/kageha-loop" node crates/kageha-loop/tests/model_catalog_e2e.mjs
KAGEHA_LOOP_BIN="$component_bin/kageha-loop" node crates/kageha-loop/tests/runtime_supervision_e2e.mjs
KAGEHA_LOOP_BIN="$component_bin/kageha-loop" KAGEHA_RUNTIME_BIN="$component_bin/kageha-runtime-service" node crates/kageha-runtime-service/tests/service_e2e.mjs
KAGEHA_TEST_MODEL_CATALOG=1 KAGEHA_LOOP_BIN="$component_bin/kageha-loop" KAGEHA_RUNTIME_BIN="$component_bin/kageha-runtime-service" node crates/kageha-runtime-service/tests/service_e2e.mjs
KAGEHA_RUNTIME_BIN="$component_bin/kageha-runtime-service" node crates/kageha-runtime-service/tests/approvals_e2e.mjs
KAGEHA_LOOP_BIN="$component_bin/kageha-loop" node crates/kageha-loop/tests/policy_migration_e2e.mjs
node crates/kageha-loop/tests/service_replacement_e2e.mjs
node crates/kageha-loop/tests/service_recovery_e2e.mjs
cargo build --locked -p kageha-agent-sdk --examples
"$component_bin"/examples/embed
node --test sdk/typescript/client.test.mjs sdk/typescript/agent.test.mjs
AGENT_SDK_LANGUAGES=typescript,rust node sdk/tests/agent_e2e.mjs
# Deliberately separate: xtask release-gate remains red until full retained parity.

node crates/kageha-runtime-service/tests/bridge_recovery_e2e.mjs
node crates/kageha-loop/tests/standalone_runtime_ui_e2e.mjs

cargo test --locked -p kageha-ui --features terminal-client
cargo build --locked -p kageha-ui --features terminal-client --bin kageha-agents
node crates/kageha-ui/tests/agents_terminal.mjs
