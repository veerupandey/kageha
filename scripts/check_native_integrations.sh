#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
cargo test --locked -p kageha-integrations
# This suite covers HTTP/credential/media/channel compatibility, not desktop expansion.
cargo build --locked -p kageha-loop --no-default-features
KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop" node crates/kageha-loop/tests/native_adapters_e2e.mjs
