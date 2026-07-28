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
OPTIONAL=false
if [ "${1:-}" = "--follow" ] || [ "${1:-}" = "-f" ]; then
    FOLLOW=true
elif [ "${1:-}" = "--if-configured" ]; then
    OPTIONAL=true
elif [ -n "${1:-}" ]; then
    echo "Usage: $0 [--follow|-f|--if-configured]" >&2
    exit 2
fi

mkdir -p "$CLI_PROXY_HOME"

CONFIG="${MODEL_GATEWAY_CLI_PROXY_CONFIG:-$CLI_PROXY_HOME/config.yaml}"
PORT="${MODEL_GATEWAY_CLI_PROXY_PORT:-8317}"
if [ "$OPTIONAL" = true ] && [ ! -f "$CONFIG" ]; then
    echo "CLIProxyAPI is not configured; skipping sidecar startup."
    exit 0
fi

LISTENER_PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
if [ -n "$LISTENER_PIDS" ]; then
    LISTENER_PID="$(printf '%s\n' "$LISTENER_PIDS" | head -n 1)"
    printf '%s\n' "$LISTENER_PID" > "$PIDFILE"
    echo "CLIProxyAPI is already running (PID $LISTENER_PID, port $PORT)."
    if [ "$OPTIONAL" = true ]; then
        exit 0
    fi
    exit 1
fi

if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE")
    if kill -0 "$OLD_PID" 2>/dev/null; then
        for _ in $(seq 1 20); do
            LISTENER_PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
            if [ -n "$LISTENER_PIDS" ]; then
                LISTENER_PID="$(printf '%s\n' "$LISTENER_PIDS" | head -n 1)"
                printf '%s\n' "$LISTENER_PID" > "$PIDFILE"
                echo "CLIProxyAPI is already running (PID $LISTENER_PID, port $PORT)."
                if [ "$OPTIONAL" = true ]; then
                    exit 0
                fi
                exit 1
            fi
            sleep 0.25
        done
        echo "CLIProxyAPI is already running (PID $OLD_PID). Use 'scripts/restart-cli-proxy.sh' to restart."
        if [ "$OPTIONAL" = true ]; then
            exit 0
        fi
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
for _ in $(seq 1 40); do
    LISTENER_PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
    if [ -n "$LISTENER_PIDS" ]; then
        LISTENER_PID="$(printf '%s\n' "$LISTENER_PIDS" | head -n 1)"
        printf '%s\n' "$LISTENER_PID" > "$PIDFILE"
        echo "CLIProxyAPI started (PID $LISTENER_PID, port $PORT, log: $LOG)"
        exit 0
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
        break
    fi
    sleep 0.25
done
echo "CLIProxyAPI failed to listen on port $PORT; check $LOG" >&2
exit 1
