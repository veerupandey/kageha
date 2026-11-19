#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-workspace
cargo test --locked -p kageha-loop worker_manager_preserves_source_scope
cargo build --locked -p kageha-loop --no-default-features
node crates/kageha-harness/tests/worker_worktrees_e2e.mjs
node crates/kageha-harness/tests/project_brain_e2e.mjs
