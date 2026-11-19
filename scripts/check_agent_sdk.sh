#!/usr/bin/env bash
# Backend and Rust/TypeScript client gate. Optional Python has its own lane.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export KAGEHA_MEMORY_BIN="$CARGO_TARGET_DIR/debug/kageha-memory"
export KAGEHA_RUNTIME_BIN="$CARGO_TARGET_DIR/debug/kageha-runtime-service"
export KAGEHA_AGENT_CLIENT_BIN="$CARGO_TARGET_DIR/debug/examples/client"
cargo test --locked -p kageha-agent-sdk --all-targets
cargo build --locked -p kageha-agent-sdk --examples
cargo build --locked -p kageha-runtime-service -p kageha-memory
"$CARGO_TARGET_DIR/debug/examples/embed"
npx --yes --package typescript@6.0.2 tsc --noEmit --strict --module nodenext --moduleResolution nodenext --target es2022 sdk/typescript/agent-types.test.ts
node --test sdk/typescript/client.test.mjs sdk/typescript/agent.test.mjs
AGENT_SDK_LANGUAGES=typescript,rust node sdk/tests/agent_e2e.mjs
