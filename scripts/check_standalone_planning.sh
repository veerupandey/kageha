#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-workflow --features sqlite -p kageha-runtime-service
cargo test --locked -p kageha-loop --lib ui_planning::tests
cargo run --locked -p kageha-workflow --example planning
cargo build --locked -p kageha-runtime-service -p kageha-loop
cargo build --locked -p kageha-runtime-service --example planning_fault_fixture
node crates/kageha-runtime-service/tests/planning_e2e.mjs
KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop" node crates/kageha-loop/tests/webui_runtime_e2e.mjs
