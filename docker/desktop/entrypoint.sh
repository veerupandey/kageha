#!/bin/sh
set -eu
rm -f /tmp/.X99-lock
Xvfb "$DISPLAY" -screen 0 1440x900x24 -nolisten tcp &
for i in $(seq 50); do
    xdotool getdisplaygeometry >/dev/null 2>&1 && break
    sleep .1
done
openbox &
kageha-shell-service --container --socket /tmp/kageha-shell.sock &
mkdir -p "$HOME/profile/Default" /work/outputs
if [ ! -f "$HOME/profile/Default/Preferences" ]; then
    printf '%s\n' '{"download":{"default_directory":"/work/outputs","prompt_for_download":false}}' \
        > "$HOME/profile/Default/Preferences"
fi
exec chromium --disable-setuid-sandbox --no-first-run --disable-gpu \
    --remote-debugging-address=127.0.0.1 --remote-debugging-port=9222 \
    --user-data-dir="$HOME/profile" about:blank
