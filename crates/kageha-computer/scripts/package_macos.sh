#!/usr/bin/env bash
# Stage and validate the standalone helper before replacing any installed app.
set -euo pipefail
umask 077
fail() { echo "Kageha signing: $*" >&2; exit 1; }
usage() {
  echo 'Usage: package_macos.sh --binary ABSOLUTE_FILE --output ABSOLUTE_APP [--ad-hoc] [--allow-identity-change] [--notarize]'
  echo '       package_macos.sh --check-signing [--ad-hoc] [--notarize]'
}
binary='' destination='' adhoc=0 allow_change=0 notarize=0 check=0
while (($#)); do
  case "$1" in
    --binary|--output) (($# >= 2)) || fail "Missing value for $1"; if [[ "$1" == --binary ]]; then binary="$2"; else destination="$2"; fi; shift 2 ;;
    --ad-hoc) adhoc=1; shift ;;
    --allow-identity-change) allow_change=1; shift ;;
    --notarize) notarize=1; shift ;;
    --check-signing) check=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; fail "Unknown option: $1" ;;
  esac
done
[[ "$(uname -s)" == Darwin ]] || fail 'Packaging requires macOS.'
identity="${KAGEHA_COMPUTER_SIGN_IDENTITY:-}"
profile="${KAGEHA_COMPUTER_NOTARY_PROFILE:-}"
if ((adhoc)); then
  [[ -z "$identity" || "$identity" == - ]] || fail '--ad-hoc conflicts with a configured signing identity.'
  ((notarize == 0)) || fail 'Apple cannot notarize an ad-hoc signed app.'
  identity='-'
  echo 'Local development signing selected. Changed ad-hoc builds may require renewed macOS grants.' >&2
else
  [[ "$identity" != - ]] || fail 'Ad-hoc signing requires --ad-hoc explicitly.'
  identity_rows="$(security find-identity -v -p codesigning)" || fail 'Cannot read signing identities from Keychain.'
  candidates=() names=()
  while IFS='|' read -r fingerprint name; do
    [[ -n "$fingerprint" ]] || continue
    candidates+=("$fingerprint"); names+=("$name")
  done < <(printf '%s\n' "$identity_rows" | sed -nE 's/^[[:space:]]*[0-9]+\) ([[:xdigit:]]{40}) "(Developer ID Application: .*)".*/\1|\2/p')
  selected=''
  if [[ -n "$identity" ]]; then
    for ((i=0;i<${#candidates[@]};i++)); do
      if [[ "$identity" == "${candidates[$i]}" || "$identity" == "${names[$i]}" ]]; then selected="${candidates[$i]}"; fi
    done
    [[ -n "$selected" ]] || fail 'Configured identity is not a valid Developer ID Application identity with its private key.'
  elif ((${#candidates[@]} == 1)); then
    selected="${candidates[0]}"
  elif ((${#candidates[@]} > 1)); then
    fail 'Multiple Developer ID Application identities found. Set KAGEHA_COMPUTER_SIGN_IDENTITY to the intended fingerprint.'
  else
    fail 'No Developer ID Application identity with private key is available. Enroll at developer.apple.com/programs/ and install the certificate, or explicitly use --ad-hoc for local development. The installed helper was not changed.'
  fi
  identity="$selected"
fi
if ((notarize)); then
  [[ -n "$profile" ]] || fail 'Set KAGEHA_COMPUTER_NOTARY_PROFILE to a notarytool Keychain credential profile.'
  xcrun notarytool history --keychain-profile "$profile" --output-format json >/dev/null || fail 'Notarization credentials are unavailable or invalid. Store them with notarytool store-credentials.'
fi
if ((check)); then echo "Signing preflight passed: $([[ "$identity" == - ]] && echo 'explicit local ad-hoc' || echo 'Developer ID Application')"; exit 0; fi
[[ "$binary" == /* && -f "$binary" && ! -L "$binary" ]] || fail '--binary must be an absolute regular file, not a symlink.'
[[ "$destination" == /*.app && "$destination" != *$'\n'* ]] || fail '--output must be an absolute .app path.'
[[ ! -L "$destination" ]] || fail 'Refusing to replace a symlinked app.'
version="${KAGEHA_COMPUTER_VERSION:-0.1.0}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'KAGEHA_COMPUTER_VERSION must have three numeric components.'
parent="$(dirname "$destination")"
mkdir -p "$parent"
lock="$destination.install-lock"
mkdir "$lock" 2>/dev/null || fail "Another install is active or a stale lock exists: $lock"
printf '%s\n' "$$" > "$lock/pid"
staging='' promoting=0
cleanup() {
  if [[ -n "$staging" ]]; then
    if ((promoting)) && [[ -d "$staging/Kageha Computer.app" ]]; then
      echo "Interrupted promotion: staged/rollback app retained at $staging/Kageha Computer.app" >&2
    else rm -rf "$staging"; fi
  fi
  rm -f "$lock/pid"; rmdir "$lock"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
staging="$(mktemp -d "$parent/.kageha-computer-stage.XXXXXX")"
app="$staging/Kageha Computer.app"
mkdir -p "$app/Contents/MacOS"
chmod 755 "$app" "$app/Contents" "$app/Contents/MacOS"
cp "$binary" "$app/Contents/MacOS/kageha-computer"
chmod 755 "$app/Contents/MacOS/kageha-computer"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>ai.kageha.computer</string>
<key>CFBundleName</key><string>Kageha Computer</string>
<key>CFBundleExecutable</key><string>kageha-computer</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>LSUIElement</key><true/>
<key>NSScreenCaptureUsageDescription</key><string>Show the selected app window in Kageha while you supervise computer actions.</string>
</dict></plist>
PLIST
chmod 644 "$app/Contents/Info.plist"
if ((adhoc)); then codesign --force --sign - --timestamp=none "$app";
else codesign --force --sign "$identity" --options runtime --timestamp "$app"; fi
codesign --verify --strict "$app"
if ((!adhoc)); then
  details="$(codesign --display --verbose=4 "$app" 2>&1)"
  printf '%s\n' "$details" | grep -q '^Authority=Developer ID Application:' || fail 'Signed artifact is not Developer ID Application signed.'
  team="$(printf '%s\n' "$details" | sed -n 's/^TeamIdentifier=//p')"
  [[ "$team" =~ ^[A-Z0-9]{10}$ ]] || fail 'Signed artifact has no valid Apple TeamIdentifier.'
  [[ -z "${KAGEHA_COMPUTER_TEAM_ID:-}" || "$team" == "$KAGEHA_COMPUTER_TEAM_ID" ]] || fail 'Signed artifact belongs to a different configured Apple team.'
fi
# Compatible designated requirements preserve the OS identity across updates.
# Do not substitute a weaker custom requirement to avoid a consent prompt.
if [[ -e "$destination" ]]; then
  [[ -d "$destination" ]] || fail 'Existing output is not an app directory.'
  [[ "$(plutil -extract CFBundleIdentifier raw -o - "$destination/Contents/Info.plist")" == ai.kageha.computer ]] || fail 'Existing app has a different bundle identifier.'
  codesign --verify --strict "$destination" || fail 'Existing app signature is invalid; repair it explicitly before replacement.'
  old_details="$(codesign --display --verbose=4 "$destination" 2>&1)"
  if ((adhoc)) && printf '%s\n' "$old_details" | grep -q '^Authority=Developer ID Application:'; then
    fail 'Refusing to downgrade a Developer ID app to ad-hoc signing.'
  fi
  old_requirement="$(codesign --display --requirements - "$destination" 2>&1 | sed -nE 's/^(# )?designated => //p')"
  new_requirement="$(codesign --display --requirements - "$app" 2>&1 | sed -nE 's/^(# )?designated => //p')"
  [[ -n "$old_requirement" && -n "$new_requirement" ]] || fail 'Could not read designated requirements.'
  if ! codesign --verify --strict --test-requirement "=$old_requirement" "$app" >/dev/null 2>&1 ||
     ! codesign --verify --strict --test-requirement "=$new_requirement" "$destination" >/dev/null 2>&1; then
    ((allow_change)) || fail 'This update changes the macOS app identity and may invalidate existing grants. Nothing was replaced. Use --allow-identity-change only for an intentional signing migration or local ad-hoc rebuild.'
    echo 'Explicit identity migration: Accessibility and Screen Recording may require renewed consent after installation.' >&2
  fi
fi
if ((notarize)); then
  ditto -c -k --keepParent --sequesterRsrc "$app" "$staging/upload.zip"
  if ! xcrun notarytool submit "$staging/upload.zip" --keychain-profile "$profile" --wait --timeout 20m --output-format json > "$staging/notary.json"; then
    cp "$staging/notary.json" "$parent/kageha-notary-failure-$.json"
    fail "Notarization did not complete. The installed app is unchanged. Report: $parent/kageha-notary-failure-$.json"
  fi
  status="$(plutil -extract status raw -o - "$staging/notary.json")"
  if [[ "$status" != Accepted ]]; then
    cp "$staging/notary.json" "$parent/kageha-notary-failure-$$.json"
    fail "Apple notarization was not accepted. Report: $parent/kageha-notary-failure-$$.json"
  fi
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  codesign --verify --strict "$app"
  syspolicy_check distribution "$app"
fi
# A single Darwin rename/exchange preserves the installed path across crashes.
# On update, the exchanged old bundle remains at $app until saved for rollback.
backup=''
if [[ -e "$destination" ]]; then backup="$parent/.kageha-computer-previous-$(basename "$staging").app"; fi
script_dir="$(cd "$(dirname "$0")" && pwd)"
promoting=1
swift "$script_dir/atomic_install.swift" "$app" "$destination"
if [[ -n "$backup" ]] && ! mv "$app" "$backup"; then
  echo "Updated app is installed; rollback bundle retained at: $app" >&2
  staging='' # Preserve the old bundle if its backup rename was refused.
fi
promoting=0
echo "Packaged: $destination"
[[ -z "$backup" || ! -d "$backup" ]] || echo "Rollback app: $backup"
if ((notarize)); then echo 'Developer ID signature, notarization ticket and distribution assessment verified.';
elif ((!adhoc)); then echo 'Developer ID signature verified. Notarization was not requested.'; fi
