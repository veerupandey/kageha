#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
runs="${1:-1}"
if [[ ! "$runs" =~ ^[1-9][0-9]*$ ]]; then
  printf 'Expected a positive run count\n' >&2
  exit 2
fi
if [[ ! -d frontend/node_modules ]]; then npm ci --prefix frontend; fi
for ((i = 1; i <= runs; i++)); do
  scripts/qualify_core.sh
  npm run lint --prefix frontend
  npm run build --prefix frontend
  npm test --prefix frontend
  scripts/check_rust_components.sh
  scripts/check_rust_installation.sh
done
