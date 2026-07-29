#!/usr/bin/env bash
set -euo pipefail

CLI_PROXY_HOME="${MODEL_GATEWAY_CLI_PROXY_HOME:-$HOME/.config/model-gateway/cli-proxy}"
PIDFILE="${MODEL_GATEWAY_CLI_PROXY_PIDFILE:-$CLI_PROXY_HOME/server.pid}"
PORT="${MODEL_GATEWAY_CLI_PROXY_PORT:-8317}"

is_cli_proxy_process() {
    local pid="$1"
    local command
    command=$(ps -p "$pid" -o command= 2>/dev/null || true)
    case "$command" in
        *model-gateway[[:space:]]cli-proxy[[:space:]]serve*) return 0 ;;
        *) return 1 ;;
    esac
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
