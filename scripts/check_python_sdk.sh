#!/usr/bin/env bash
# Optional client conformance only. Never required by backend build/install/tests.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export KAGEHA_MEMORY_BIN="$CARGO_TARGET_DIR/debug/kageha-memory"
export KAGEHA_RUNTIME_BIN="$CARGO_TARGET_DIR/debug/kageha-runtime-service"
cargo build --locked -p kageha-runtime-service -p kageha-memory
PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -v
AGENT_SDK_LANGUAGES=python node sdk/tests/agent_e2e.mjs
