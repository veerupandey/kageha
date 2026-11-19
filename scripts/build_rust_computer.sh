#!/usr/bin/env bash
# Build Rust independently; package/install only after signing preflight passes.
set -euo pipefail
project_root="$(cd "$(dirname "$0")/.." && pwd)"
cargo_bin="${KAGEHA_CARGO:-cargo}"
package_script="$project_root/crates/kageha-computer/scripts/package_macos.sh"
install=0 check=0 destination='' options=()
while (($#)); do
  case "$1" in
    --install) install=1; shift ;;
    --package) [[ $# -ge 2 ]] || { echo '--package requires an absolute .app path' >&2; exit 2; }; destination="$2"; shift 2 ;;
    --ad-hoc|--allow-identity-change|--notarize) options+=("$1"); shift ;;
    --check-signing) check=1; shift ;;
    --help|-h) echo 'Usage: build_rust_computer.sh [--install | --package ABSOLUTE_APP] [--ad-hoc] [--allow-identity-change] [--notarize] [--check-signing]'; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ "$(uname -s)" == Darwin ]] || { echo 'The native computer helper requires macOS 14 or newer.' >&2; exit 1; }
if ((install)) && [[ -n "$destination" ]]; then echo 'Choose --install or --package, not both.' >&2; exit 2; fi
if ((check)); then exec "$package_script" --check-signing ${options[@]+"${options[@]}"}; fi
if ((install)) || [[ -n "$destination" ]]; then
  "$package_script" --check-signing ${options[@]+"${options[@]}"}
elif ((${#options[@]})); then echo 'Signing options require --install, --package or --check-signing.' >&2; exit 2; fi
bin_dir="$HOME/.local/bin"
if ((install)); then
  destination="$HOME/.local/share/kageha/computer/Kageha Computer.app"
  if [[ -e "$bin_dir/kageha-computer" || -L "$bin_dir/kageha-computer" ]]; then
    [[ -L "$bin_dir/kageha-computer" ]] || { echo "Refusing to replace non-symlink: $bin_dir/kageha-computer" >&2; exit 1; }
  fi
fi
"$cargo_bin" build --release --locked --manifest-path "$project_root/crates/kageha-computer/Cargo.toml"
target_dir="$("$cargo_bin" metadata --format-version 1 --no-deps --manifest-path "$project_root/crates/kageha-computer/Cargo.toml" | plutil -extract target_directory raw -o - -)"
helper_binary="$target_dir/release/kageha-computer"
if [[ -n "$destination" ]]; then
  "$package_script" --binary "$helper_binary" --output "$destination" ${options[@]+"${options[@]}"}
  if ((install)); then mkdir -p "$bin_dir"; ln -sfn "$destination/Contents/MacOS/kageha-computer" "$bin_dir/kageha-computer"; fi
else echo "Built: $helper_binary"; fi
