#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-workspace -p kageha-runtime-service
cargo build --locked -p kageha-runtime-service -p kageha-workspace -p kageha-loop
cargo build --locked -p kageha-mcp --example fixture
cargo run --locked -p kageha-runtime-service --example workers
node crates/kageha-runtime-service/tests/workers_e2e.mjs
# The same manager must preserve the existing application's worker semantics.
node crates/kageha-harness/tests/worker_worktrees_e2e.mjs
node crates/kageha-harness/tests/project_brain_e2e.mjs
