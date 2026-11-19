#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-agents --features sqlite
cargo run --locked -p kageha-agents --features sqlite --example repository
cargo test --locked -p kageha-loop agent_export
cargo test --locked -p kageha-runtime-service persistent_profile_changes
cargo build --locked -p kageha-agents --features sqlite --bin kageha-agent-profiles
cargo build --locked -p kageha-runtime-service -p kageha-loop
cargo build --locked -p kageha-mcp --example fixture
node crates/kageha-agents/tests/repository_e2e.mjs
KAGEHA_TEST_PROFILE_REPOSITORY=1 node crates/kageha-runtime-service/tests/profiles_e2e.mjs
KAGEHA_TEST_PROFILE_REPOSITORY=1 KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop" node crates/kageha-loop/tests/webui_runtime_e2e.mjs
