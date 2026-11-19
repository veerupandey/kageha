#!/usr/bin/env bash
# Run the built WebUI and native backend on one listener.
set -euo pipefail

repo="${KAGEHA_REPO_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
state="${KAGEHA_STATE_DIR:-$repo/.kageha/rust-webui}"
workspace="${KAGEHA_WORKSPACE:-$repo}"
host="${KAGEHA_WEBUI_HOST:-127.0.0.1}"
port="${KAGEHA_WEBUI_PORT:-8788}"
bin="${KAGEHA_LOOP_BIN:-$repo/target/release/kageha-loop}"
dist="${KAGEHA_WEBUI_DIST:-$repo/frontend/dist}"
resources="${KAGEHA_RESOURCE_DIR:-$repo/resources}"
pidfile="$state/webui.pid"
log="$state/webui.log"

running() {
  [[ -f "$pidfile" ]] || return 1
  local pid command
  read -r pid < "$pidfile"
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  command="$(ps -p "$pid" -o command= 2>/dev/null)" || return 1
  [[ "$command" == *kageha-loop\ serve* && "$command" == *"$state"* ]]
}

serve() {
  [[ -x "$bin" ]] || { printf 'Build kageha-loop with cargo build --locked --release -p kageha-loop\n' >&2; return 1; }
  [[ -f "$dist/index.html" ]] || { printf 'Build the frontend with npm run build in frontend\n' >&2; return 1; }
  umask 077
  mkdir -p "$state"
  if [[ -z "${KAGEHA_SERVER_TOKEN:-}" ]]; then
    if [[ ! -e "$state/server.token" ]]; then
      (set -o noclobber; openssl rand -hex 32 > "$state/server.token") || [[ -s "$state/server.token" ]]
    fi
    read -r KAGEHA_SERVER_TOKEN < "$state/server.token"
  fi
  export KAGEHA_SERVER_TOKEN KAGEHA_WEBUI_DIST="$dist" KAGEHA_RESOURCE_DIR="$resources"
  export KAGEHA_MODELS_FILE="${KAGEHA_MODELS_FILE:-$resources/models.yaml}"
  exec "$bin" serve "$workspace" "$state" "$host:$port"
}

start() {
  if running; then printf 'WebUI already running on %s:%s\n' "$host" "$port"; return; fi
  umask 077
  mkdir -p "$state"
  local offset=0
  [[ ! -f "$log" ]] || offset="$(wc -c < "$log" | tr -d ' ')"
  nohup "$BASH" "${BASH_SOURCE[0]}" serve >> "$log" 2>&1 < /dev/null &
  local pid=$!
  printf '%s\n' "$pid" > "$pidfile"
  for _ in {1..100}; do
    if ! kill -0 "$pid" 2>/dev/null; then
      printf 'WebUI startup failed. See %s\n' "$log" >&2
      return 1
    fi
    if tail -c "+$((offset + 1))" "$log" | grep -q '^Rust chat service listening on '; then
      printf 'WebUI listening on http://%s:%s (log %s)\n' "$host" "$port" "$log"
      return
    fi
    sleep 0.1
  done
  printf 'WebUI did not report readiness. See %s\n' "$log" >&2
  return 1
}

stop() {
  if running; then
    local pid
    read -r pid < "$pidfile"
    kill -TERM "$pid"
    for _ in {1..100}; do
      if ! running; then return; fi
      sleep 0.1
    done
    printf 'WebUI is still shutting down. See %s\n' "$log" >&2
    return 1
  fi
}

case "${1:-start}" in
  serve) serve ;;
  start) start ;;
  stop) stop ;;
  restart) stop; start ;;
  status)
    if running; then printf 'WebUI running on %s:%s (log %s)\n' "$host" "$port" "$log"
    else printf 'WebUI stopped\n'; fi ;;
  *) printf 'Usage: %s {start|stop|restart|status|serve}\n' "$0" >&2; exit 2 ;;
esac
