#!/usr/bin/env bash
# Build a relocatable native archive from this checkout.
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-dist}
mkdir -p "$out"
out=$(cd "$out" && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
os=$(uname -s)
arch=$(uname -m)
case "$os" in Darwin) platform=macos ;; Linux) platform=linux ;; *) echo "Unsupported OS: $os" >&2; exit 1 ;; esac
case "$arch" in arm64|aarch64) arch=arm64 ;; x86_64) arch=x64 ;; *) echo "Unsupported architecture: $arch" >&2; exit 1 ;; esac
name="kageha-$platform-$arch"
root="$stage/$name"
mkdir -p "$root/bin" "$root/share/kageha/webui"
cargo build --locked --release -p kageha-harness -p kageha-loop -p kageha-workspace -p kageha-browser \
  --bin kageha --bin kageha-loop --bin kageha-shell-service --bin kageha-browser
npm --prefix frontend ci
npm --prefix frontend run build -- --outDir "$root/share/kageha/webui" --sourcemap false
for bin in kageha kageha-loop kageha-shell-service kageha-browser; do
  install -m 755 "${CARGO_TARGET_DIR:-target}/release/$bin" "$root/bin/$bin"
done
# Only version-controlled resources belong in the distribution.
git archive HEAD:resources models.yaml skills | tar -x -C "$root/share/kageha"
install -m 755 packaging/start "$root/start"
install -m 644 LICENSE README.md "$root/"
mkdir -p "$root/docs/assets"
install -m 644 docs/assets/kageha-browser-demo.png "$root/docs/assets/"
install -m 644 docs/LINUX_DEPLOYMENT.md "$root/docs/"
if [[ "$os" == Darwin ]]; then
  cp "$root/start" "$root/Start Kageha.command"
  cp "$root/start" "$root/Sign in.command"
else
  mkdir -p "$root/share/kageha/linux"
  install -m 644 packaging/linux/kageha.service packaging/linux/kageha.nginx.conf "$root/share/kageha/linux/"
  install -m 755 packaging/linux/kageha-linux-session "$root/bin/"
fi
"$root/bin/kageha" doctor --resources "$root/share/kageha"
COPYFILE_DISABLE=1 tar -czf "$stage/$name.tar.gz" -C "$stage" "$name"
(cd "$stage" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
mv "$stage/$name.tar.gz" "$stage/$name.tar.gz.sha256" "$out/"
printf 'Built %s/%s.tar.gz\n' "$out" "$name"
