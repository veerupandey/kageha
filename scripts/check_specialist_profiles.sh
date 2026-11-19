#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-agents -p kageha-contracts -p kageha-policy -p kageha-knowledge -p kageha-models -p kageha-tools -p kageha-runtime-service
cargo run --locked -p kageha-agents --example profiles
cargo build --locked -p kageha-runtime-service -p kageha-loop
cargo build --locked -p kageha-mcp --example fixture
node crates/kageha-runtime-service/tests/profiles_e2e.mjs
KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop" node crates/kageha-loop/tests/webui_runtime_e2e.mjs
KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop" node crates/kageha-harness/tests/specialist_policy_e2e.mjs
