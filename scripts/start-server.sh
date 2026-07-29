#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${MODEL_GATEWAY_PORT:-8008}"
LOG="$ROOT/server.log"
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

# Source .env.local if it exists
ENV_LOCAL="$ROOT/.env.local"
if [ -f "$ENV_LOCAL" ]; then
    set -a
    source "$ENV_LOCAL"
    set +a
fi

FOLLOW=false
if [ "${1:-}" = "--follow" ] || [ "${1:-}" = "-f" ]; then
    FOLLOW=true
fi

if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE")
    if kill -0 "$OLD_PID" 2>/dev/null && is_gateway_process "$OLD_PID"; then
        echo "Server is already running (PID $OLD_PID). Use 'scripts/restart-server.sh' to restart."
        exit 1
    fi
    rm -f "$PIDFILE"
fi

echo "Building release binary..."
cargo build --release --manifest-path "$ROOT/Cargo.toml" 2>&1

BIN="$ROOT/target/release/model-gateway"

echo "Starting configured CLIProxyAPI sidecar..."
"$ROOT/scripts/start-cli-proxy.sh" --if-configured

echo "Refreshing catalogs, benchmarks, and pricing..."
if ! "$BIN" refresh; then
    echo "Refresh completed with failures; starting with last-known-good data." >&2
fi

if [ "$FOLLOW" = true ]; then
    echo "Starting server on port $PORT (foreground)..."
    exec "$BIN" serve
else
    echo "Starting server on port $PORT (background, PID file: $PIDFILE)..."
    nohup "$BIN" serve > "$LOG" 2>&1 &
    PID=$!
    echo "$PID" > "$PIDFILE"

    # Wait for the port to be ready
    for i in $(seq 1 30); do
        if lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
            LISTENER_PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
            LISTENER_PID=""
            while IFS= read -r candidate; do
                LISTENER_PID="$candidate"
                break
            done <<< "$LISTENER_PIDS"
            printf '%s\n' "$LISTENER_PID" > "$PIDFILE"
            echo "Server is ready (PID $LISTENER_PID, logs: $LOG)"
            exit 0
        fi
        sleep 0.5
    done

    echo "Server started but port $PORT is not yet listening. Check logs: $LOG"
    exit 1
fi
