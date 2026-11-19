#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
fixture_context=$(mktemp -d /tmp/kageha-linux-permissions-build.XXXXXX)
trap 'rm -rf "$fixture_context"' EXIT
# Deliberately copy only public component sources. No credentials, workspace data,
# backend environment or host Docker socket are exposed to the test container.
node --input-type=module - "$fixture_context" <<'JS'
import fs from 'node:fs';
import path from 'node:path';
const destination=process.argv[2];
const crates=['kageha-contracts','kageha-sandbox','kageha-mcp','kageha-workspace'];
for(const crate of crates) fs.cpSync(`crates/${crate}`,path.join(destination,'crates',crate),{recursive:true});
const manifest=fs.readFileSync('Cargo.toml','utf8')
  .replace(/^members = .*$/m,`members = ${JSON.stringify(crates.map(c=>`crates/${c}`))}`)
  .replace(/^default-members = .*\n/m,'');
fs.writeFileSync(path.join(destination,'Cargo.toml'),manifest);
fs.copyFileSync('Cargo.lock',path.join(destination,'Cargo.lock'));
fs.copyFileSync('scripts/fixtures/linux-permissions.Dockerfile',path.join(destination,'Dockerfile'));
JS
docker build -t kageha-permission-fixture:local "$fixture_context"
# Bubblewrap creates nested namespaces. Docker's default seccomp/capability set
# prevents that, including mounting proc over Docker's masked system paths.
# These permissions belong only to this disposable, networkless
# test container; it has no host mounts and is never privileged.
docker run --rm --network none --cap-add SYS_ADMIN \
  --security-opt seccomp=unconfined --security-opt systempaths=unconfined \
  kageha-permission-fixture:local
docker run --rm --network none --cap-add SYS_ADMIN \
  --security-opt seccomp=unconfined --security-opt systempaths=unconfined \
  kageha-permission-fixture:local sh -ec \
  'chmod -R a+rwX /fixture/target; exec setpriv --reuid=1000 --regid=1000 --clear-groups env HOME=/tmp cargo test --locked -p kageha-sandbox -p kageha-mcp -p kageha-workspace'
