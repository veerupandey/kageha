#!/usr/bin/env bash
# Sign a staged backend before replacing the executable used by the live host.
set -euo pipefail

if [[ $# != 1 || ! -f "$1" ]]; then
  echo "Usage: bash scripts/sign_local_kageha_backend.sh /path/to/staged/kageha-loop" >&2
  exit 2
fi

signing_dir="${KAGEHA_LOCAL_SIGNING_DIR:-$HOME/.kageha/rust-native-browser-live/signing}"
keychain="$signing_dir/backend.keychain-db"
identity="$(<"$signing_dir/identity")"
[[ "$identity" =~ ^[[:xdigit:]]{40}$ ]]

/usr/bin/security unlock-keychain -p "$(<"$signing_dir/signing-keychain.password")" "$keychain"
trap '/usr/bin/security lock-keychain "$keychain"' EXIT
/usr/bin/codesign --force --keychain "$keychain" --sign "$identity" \
  --identifier com.kageha.loop --timestamp=none "$1"
/usr/bin/codesign --verify --strict \
  --test-requirement "=identifier \"com.kageha.loop\" and certificate leaf = H\"$identity\"" "$1"
