#!/usr/bin/env bash
# Deterministic regression gate. Uses disposable local services and test credentials.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
  toolchain_dir="${RUSTUP_HOME:-$HOME/.rustup}/toolchains/stable-aarch64-apple-darwin/bin"
  export PATH="$toolchain_dir:$PATH"
fi

cargo test --locked -p kageha-contracts -p kageha-runtime -p kageha-runtime-store -p kageha-runtime-service -p kageha-workflow --lib --tests
cargo test --locked -p kageha-models --lib --test rate_recovery
cargo test --locked -p kageha-loop --lib
cargo test --locked -p kageha-browser research::tests

npm --prefix frontend run test -- src/components/PersistenceSettings.test.tsx src/components/Transcript/ToolDetails.test.tsx src/components/Transcript/ToolCallPill.test.tsx src/components/Transcript/Transcript.test.tsx
npm --prefix frontend run build

cargo build --locked -p kageha-loop -p kageha-browser -p kageha-runtime-service
node crates/kageha-runtime-service/tests/bridge_recovery_e2e.mjs
node crates/kageha-loop/tests/research_recovery_e2e.mjs
KAGEHA_RESEARCH_BRIDGE=1 node crates/kageha-loop/tests/research_recovery_e2e.mjs
git diff --check
