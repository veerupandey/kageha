#!/usr/bin/env bash
# Fresh, private acceptance binaries. Never replace the running composition.
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-}" in
  runtime|memory|approvals|policy-import) scenario="$1" ;;
  *) echo 'usage: check_migration_hosts.sh runtime|memory|approvals|policy-import' >&2; exit 2 ;;
esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target/migration-acceptance}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export KAGEHA_LOOP_BIN="$CARGO_TARGET_DIR/debug/kageha-loop"
export KAGEHA_RUNTIME_BIN="$CARGO_TARGET_DIR/debug/kageha-runtime-service"
export KAGEHA_MEMORY_BIN="$CARGO_TARGET_DIR/debug/kageha-memory"
if [[ "$scenario" == approvals ]]; then
  cargo build --locked -p kageha-runtime-service
else
  cargo build --locked -p kageha-loop --no-default-features -p kageha-runtime-service -p kageha-memory
fi
case "$scenario" in
  runtime) node crates/kageha-runtime-service/tests/service_e2e.mjs ;;
  memory)
    node crates/kageha-loop/tests/service_replacement_e2e.mjs
    node crates/kageha-loop/tests/service_recovery_e2e.mjs
    ;;
  approvals) node crates/kageha-runtime-service/tests/approvals_e2e.mjs ;;
  policy-import) node crates/kageha-loop/tests/policy_migration_e2e.mjs ;;
esac
