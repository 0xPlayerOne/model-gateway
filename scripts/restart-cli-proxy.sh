#!/usr/bin/env bash
set -euo pipefail

CLI_PROXY_HOME="${MODEL_GATEWAY_CLI_PROXY_HOME:-$HOME/.config/model-gateway/cli-proxy}"
PIDFILE="${MODEL_GATEWAY_CLI_PROXY_PIDFILE:-$CLI_PROXY_HOME/server.pid}"
PORT="${MODEL_GATEWAY_CLI_PROXY_PORT:-8317}"

if [ -f "$PIDFILE" ]; then
    PID=$(cat "$PIDFILE")
    if kill -0 "$PID" 2>/dev/null; then
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
    echo "Stopping CLIProxyAPI listener(s) on port $PORT..."
    echo "$PIDS" | xargs kill 2>/dev/null || true
    sleep 1
    PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        echo "$PIDS" | xargs kill -9 2>/dev/null || true
    fi
fi

rm -f "$PIDFILE"

exec "$(cd "$(dirname "$0")" && pwd)/start-cli-proxy.sh" "$@"
