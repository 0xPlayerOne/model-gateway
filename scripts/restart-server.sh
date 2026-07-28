#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${MODEL_GATEWAY_PORT:-8008}"
PIDFILE="$ROOT/server.pid"

# Kill the process actually listening on the gateway port. The executable's
# macOS process name can be its full path, so an exact pgrep is unreliable.
PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    echo "Stopping existing gateway (PIDs: $PIDS)..."
    echo "$PIDS" | xargs kill 2>/dev/null || true
    sleep 1
    PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        echo "$PIDS" | xargs kill -9 2>/dev/null || true
        sleep 1
    fi
fi
rm -f "$PIDFILE"

# Wait for port to be free
for i in $(seq 1 15); do
    if ! lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
        break
    fi
    echo "Waiting for port $PORT... ($i)"
    sleep 0.5
done

if lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "Gateway port $PORT is still occupied; refusing to start another server." >&2
    exit 1
fi

CLI_PROXY_HOME="${MODEL_GATEWAY_CLI_PROXY_HOME:-$HOME/.config/model-gateway/cli-proxy}"
CLI_PROXY_CONFIG="${MODEL_GATEWAY_CLI_PROXY_CONFIG:-$CLI_PROXY_HOME/config.yaml}"
if [ -f "$CLI_PROXY_CONFIG" ]; then
    "$(cd "$(dirname "$0")" && pwd)/restart-cli-proxy.sh"
fi

exec "$ROOT/scripts/start-server.sh" "$@"
