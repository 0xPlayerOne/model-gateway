#!/usr/bin/env bash
set -euo pipefail

CLI_PROXY_HOME="${MODEL_GATEWAY_CLI_PROXY_HOME:-$HOME/.config/model-gateway/cli-proxy}"
PIDFILE="${MODEL_GATEWAY_CLI_PROXY_PIDFILE:-$CLI_PROXY_HOME/server.pid}"
CONFIG="${MODEL_GATEWAY_CLI_PROXY_CONFIG:-$CLI_PROXY_HOME/config.yaml}"

read_config_port() {
    if [ ! -f "$CONFIG" ]; then
        return 0
    fi
    sed -nE 's/^[[:space:]]*port:[[:space:]]*([0-9]+)[[:space:]]*$/\1/p' "$CONFIG" | head -n 1
}

CONFIG_PORT="$(read_config_port)"
if [ -n "${MODEL_GATEWAY_CLI_PROXY_PORT:-}" ]; then
    PORT="$MODEL_GATEWAY_CLI_PROXY_PORT"
    if [ -n "$CONFIG_PORT" ] && [ "$PORT" != "$CONFIG_PORT" ]; then
        echo "MODEL_GATEWAY_CLI_PROXY_PORT=$PORT does not match the configured CLIProxy port $CONFIG_PORT in $CONFIG; update the config or unset the override." >&2
        exit 1
    fi
else
    PORT="${CONFIG_PORT:-8317}"
fi
if ! [[ "$PORT" =~ ^[1-9][0-9]{0,4}$ ]] || [ "$PORT" -gt 65535 ]; then
    echo "Invalid CLIProxy port '$PORT'; expected an integer from 1 to 65535." >&2
    exit 2
fi

is_cli_proxy_process() {
    local pid="$1"
    local command
    command=$(ps -p "$pid" -o command= 2>/dev/null || true)
    [[ "$command" == *"model-gateway cli-proxy serve"* ||
        "$command" == "$CLI_PROXY_HOME"/bin/*/cli-proxy-api\ -config\ "$CONFIG"* ]]
}

if [ -f "$PIDFILE" ]; then
    PID=$(cat "$PIDFILE")
    if kill -0 "$PID" 2>/dev/null && is_cli_proxy_process "$PID"; then
        echo "Stopping CLIProxyAPI (PID $PID)..."
        kill "$PID" 2>/dev/null || true
        for _ in $(seq 1 15); do
            if ! kill -0 "$PID" 2>/dev/null; then
                break
            fi
            sleep 0.5
        done
        if kill -0 "$PID" 2>/dev/null; then
            kill -9 "$PID" 2>/dev/null || true
        fi
    fi
    rm -f "$PIDFILE"
fi

PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    CLI_PROXY_PIDS=""
    while IFS= read -r pid; do
        if is_cli_proxy_process "$pid"; then
            CLI_PROXY_PIDS="$CLI_PROXY_PIDS${CLI_PROXY_PIDS:+ }$pid"
            kill "$pid" 2>/dev/null || true
        else
            echo "Port $PORT is occupied by a non-CLIProxy process (PID $pid); refusing to stop it." >&2
        fi
    done <<< "$PIDS"
    if [ -z "$CLI_PROXY_PIDS" ]; then
        exit 1
    fi
    echo "Stopping CLIProxyAPI listener(s) on port $PORT (PIDs: $CLI_PROXY_PIDS)..."
    sleep 1
    PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        while IFS= read -r pid; do
            if is_cli_proxy_process "$pid"; then
                kill -9 "$pid" 2>/dev/null || true
            else
                echo "Port $PORT is still occupied by a non-CLIProxy process (PID $pid)." >&2
            fi
        done <<< "$PIDS"
    fi
fi

rm -f "$PIDFILE"

exec "$(cd "$(dirname "$0")" && pwd)/start-cli-proxy.sh" "$@"
