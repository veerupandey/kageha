#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo run --locked -p xtask -- cutover-audit |
  node -e 'const audit=JSON.parse(require("fs").readFileSync(0,"utf8")); console.log(JSON.stringify(audit,null,2)); if (!audit.python_free_backend_ready) process.exit(1)'
cargo test --locked -p kageha-runtime -p kageha-models -p kageha-skills -p xtask
