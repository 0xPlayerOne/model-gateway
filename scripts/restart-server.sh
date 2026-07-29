#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${MODEL_GATEWAY_PORT:-8008}"
PIDFILE="$ROOT/server.pid"

is_gateway_process() {
    local pid="$1"
    local command
    command=$(ps -p "$pid" -o command= 2>/dev/null || true)
    case "$command" in
        *model-gateway[[:space:]]serve*) return 0 ;;
        *) return 1 ;;
    esac
}

stop_gateway_pid() {
    local pid="$1"
    if ! is_gateway_process "$pid"; then
        return 1
    fi
    kill "$pid" 2>/dev/null || true
    return 0
}

# Kill the process actually listening on the gateway port. The executable's
# macOS process name can be its full path, so an exact pgrep is unreliable.
PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    GATEWAY_PIDS=""
    while IFS= read -r pid; do
        if stop_gateway_pid "$pid"; then
            GATEWAY_PIDS="$GATEWAY_PIDS${GATEWAY_PIDS:+ }$pid"
        else
            echo "Port $PORT is occupied by a non-gateway process (PID $pid); refusing to stop it." >&2
        fi
    done <<< "$PIDS"
    if [ -z "$GATEWAY_PIDS" ]; then
        exit 1
    fi
    echo "Stopping existing gateway (PIDs: $GATEWAY_PIDS)..."
    sleep 1
    PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        while IFS= read -r pid; do
            if is_gateway_process "$pid"; then
                kill -9 "$pid" 2>/dev/null || true
            else
                echo "Port $PORT is still occupied by a non-gateway process (PID $pid)." >&2
            fi
        done <<< "$PIDS"
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
