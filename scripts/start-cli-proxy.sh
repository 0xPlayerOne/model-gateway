#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ -n "${MODEL_GATEWAY_BIN:-}" ]; then
    BIN="$MODEL_GATEWAY_BIN"
else
    BIN="$ROOT/target/release/model-gateway"
    if [ ! -x "$BIN" ]; then
        echo "Building release binary..."
        cargo build --release --manifest-path "$ROOT/Cargo.toml"
    fi
fi
CLI_PROXY_HOME="${MODEL_GATEWAY_CLI_PROXY_HOME:-$HOME/.config/model-gateway/cli-proxy}"
LOG="${MODEL_GATEWAY_CLI_PROXY_LOG:-$CLI_PROXY_HOME/server.log}"
PIDFILE="${MODEL_GATEWAY_CLI_PROXY_PIDFILE:-$CLI_PROXY_HOME/server.pid}"

FOLLOW=false
if [ "${1:-}" = "--follow" ] || [ "${1:-}" = "-f" ]; then
    FOLLOW=true
elif [ -n "${1:-}" ]; then
    echo "Usage: $0 [--follow|-f]" >&2
    exit 2
fi

mkdir -p "$CLI_PROXY_HOME"

if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE")
    if kill -0 "$OLD_PID" 2>/dev/null; then
        echo "CLIProxyAPI is already running (PID $OLD_PID). Use 'scripts/restart-cli-proxy.sh' to restart."
        exit 1
    fi
    rm -f "$PIDFILE"
fi

if [ "$FOLLOW" = true ]; then
    exec "$BIN" cli-proxy serve
fi

echo "Starting CLIProxyAPI in the background (log: $LOG)..."
nohup "$BIN" cli-proxy serve > "$LOG" 2>&1 &
PID=$!
printf '%s\n' "$PID" > "$PIDFILE"
echo "CLIProxyAPI started (PID $PID, log: $LOG)"
