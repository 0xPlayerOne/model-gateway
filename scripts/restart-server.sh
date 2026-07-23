#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${MODEL_GATEWAY_PORT:-8008}"

# Kill ALL model-gateway processes
PIDS=$(pgrep -f "model-gateway" 2>/dev/null || true)
if [ -n "$PIDS" ]; then
    echo "Stopping existing gateway (PIDs: $PIDS)..."
    echo "$PIDS" | xargs kill 2>/dev/null || true
    sleep 1
    PIDS=$(pgrep -f "model-gateway" 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        echo "$PIDS" | xargs kill -9 2>/dev/null || true
        sleep 1
    fi
fi

# Wait for port to be free
for i in $(seq 1 30); do
    if ! lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
        break
    fi
    echo "Waiting for port $PORT... ($i)"
    sleep 0.5
done

exec "$ROOT/scripts/start-server.sh"
