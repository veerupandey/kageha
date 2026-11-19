#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-agents --features sqlite
cargo test --locked -p kageha-loop --lib
cargo build --locked -p kageha-loop -p kageha-harness
cargo build --locked -p kageha-agents --features sqlite --bin kageha-agent-profiles
KAGEHA_TEST_PROFILE_REPOSITORY=1 KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop" node crates/kageha-loop/tests/webui_runtime_e2e.mjs
node crates/kageha-harness/tests/document_migration_e2e.mjs
KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop" node crates/kageha-loop/tests/native_adapters_e2e.mjs
