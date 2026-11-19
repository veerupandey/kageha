#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-workspace -p kageha-execution
cargo test --locked -p kageha-runtime --test fanout
cargo run --locked -p kageha-workspace --example hooks
cargo build --locked -p kageha-loop --no-default-features
node crates/kageha-harness/tests/worktrees_e2e.mjs
node crates/kageha-harness/tests/hooks_e2e.mjs
node crates/kageha-harness/tests/project_brain_e2e.mjs
npm --prefix frontend test -- --run src/components/Sidebar/WorktreesPanel.test.tsx
