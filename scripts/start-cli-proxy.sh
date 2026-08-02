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
CONFIG="${MODEL_GATEWAY_CLI_PROXY_CONFIG:-$CLI_PROXY_HOME/config.yaml}"

is_cli_proxy_process() {
    local pid="$1"
    local command
    command=$(ps -p "$pid" -o command= 2>/dev/null || true)
    [[ "$command" == *"model-gateway cli-proxy serve"* ||
        "$command" == "$CLI_PROXY_HOME"/bin/*/cli-proxy-api\ -config\ "$CONFIG"* ]]
}

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
if [ "$OPTIONAL" = true ] && [ ! -f "$CONFIG" ]; then
    echo "CLIProxyAPI is not configured; skipping sidecar startup."
    exit 0
fi

# The sidecar launcher is unattended by design: default to the deterministic
# protected-file secret store unless the operator chose another store
# explicitly. The sidecar reads its frontend key from config.yaml, but the
# gateway process it serves inherits this environment.
if [ -z "${MODEL_GATEWAY_SECRET_STORE:-}" ]; then
    export MODEL_GATEWAY_SECRET_STORE=file
    echo "Secret store: protected-file (non-interactive default; set MODEL_GATEWAY_SECRET_STORE=keychain to use the OS keychain)" >&2
else
    echo "Secret store: $MODEL_GATEWAY_SECRET_STORE (MODEL_GATEWAY_SECRET_STORE is set explicitly)" >&2
fi

LISTENER_PIDS=$(lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true)
if [ -n "$LISTENER_PIDS" ]; then
    LISTENER_PID="$(printf '%s\n' "$LISTENER_PIDS" | head -n 1)"
    if ! is_cli_proxy_process "$LISTENER_PID"; then
        echo "CLIProxy port $PORT is occupied by a non-CLIProxy process (PID $LISTENER_PID); refusing to start." >&2
        exit 1
    fi
    printf '%s\n' "$LISTENER_PID" > "$PIDFILE"
    echo "CLIProxyAPI is already running (PID $LISTENER_PID, port $PORT)."
    if [ "$OPTIONAL" = true ]; then
        exit 0
    fi
    exit 1
fi

if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE")
    if kill -0 "$OLD_PID" 2>/dev/null && is_cli_proxy_process "$OLD_PID"; then
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
if kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null || true
    for _ in $(seq 1 10); do
        if ! kill -0 "$PID" 2>/dev/null; then
            break
        fi
        sleep 0.1
    done
    if kill -0 "$PID" 2>/dev/null; then
        kill -9 "$PID" 2>/dev/null || true
    fi
fi
wait "$PID" 2>/dev/null || true
if [ -f "$PIDFILE" ] && [ "$(cat "$PIDFILE" 2>/dev/null || true)" = "$PID" ]; then
    rm -f "$PIDFILE"
fi
exit 1
