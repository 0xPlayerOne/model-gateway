#!/usr/bin/env bash
set -euo pipefail

# Supervision smoke test for the CLIProxyAPI sidecar launcher scripts
# (start-cli-proxy.sh / restart-cli-proxy.sh). Uses a fake model-gateway
# binary that behaves like `model-gateway cli-proxy serve`: it binds the
# configured port and exits on SIGTERM. The fake's command line intentionally
# contains "model-gateway cli-proxy serve" so the supervision helpers'
# process recognition matches it, just like the real binary.
#
# Run: scripts/cli-proxy-supervision-smoke.sh

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
STATE=$(mktemp -d)
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
FOREIGN_PID=""

cleanup() {
    rc=${1:-$?}
    if [ -n "$FOREIGN_PID" ]; then
        kill "$FOREIGN_PID" 2>/dev/null || true
    fi
    if [ -f "$STATE/home/server.pid" ]; then
        kill "$(cat "$STATE/home/server.pid")" 2>/dev/null || true
    fi
    rm -rf "$STATE"
    return "$rc"
}
trap cleanup EXIT

mkdir -p "$STATE/home" "$STATE/bin"

cat > "$STATE/bin/model-gateway" <<'PYEOF'
#!/usr/bin/env python3
"""Fake model-gateway: `cli-proxy serve` binds the sidecar port and exits on SIGTERM."""
import http.server
import os
import signal
import sys

if len(sys.argv) >= 3 and sys.argv[1] == "cli-proxy" and sys.argv[2] == "serve":
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"ok": true}')

        def log_message(self, *_args):
            pass

    port = int(os.environ.get("MODEL_GATEWAY_CLI_PROXY_PORT", "8317"))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    # The launcher scripts stop the sidecar with SIGTERM. Exiting releases the
    # port so the supervision loop observes a clean stop.
    signal.signal(signal.SIGTERM, lambda *_args: os._exit(0))
    server.serve_forever()
sys.exit(1)
PYEOF
chmod +x "$STATE/bin/model-gateway"
touch "$STATE/home/config.yaml"  # presence signals a configured sidecar

export MODEL_GATEWAY_BIN="$STATE/bin/model-gateway"
export MODEL_GATEWAY_CLI_PROXY_HOME="$STATE/home"
export MODEL_GATEWAY_CLI_PROXY_PORT="$PORT"
export MODEL_GATEWAY_CLI_PROXY_LOG="$STATE/home/server.log"
export MODEL_GATEWAY_CLI_PROXY_PIDFILE="$STATE/home/server.pid"
export MODEL_GATEWAY_CLI_PROXY_CONFIG="$STATE/home/config.yaml"

wait_for_port() {
    for _ in $(seq 1 40); do
        if lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

wait_for_port_free() {
    for _ in $(seq 1 40); do
        if ! lsof -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

echo "== 1. start: backgrounds the sidecar, writes the pidfile, opens the port"
if ! "$ROOT/scripts/start-cli-proxy.sh" >"$STATE/start1.out" 2>&1; then
    echo "start-cli-proxy.sh failed:" >&2
    cat "$STATE/start1.out" >&2
    exit 1
fi
grep -q "CLIProxyAPI started" "$STATE/start1.out"
grep -q "Secret store:" "$STATE/start1.out"
[ -s "$STATE/home/server.pid" ]
wait_for_port
curl --noproxy '*' --silent --fail "http://127.0.0.1:$PORT/" >/dev/null
FIRST_PID=$(cat "$STATE/home/server.pid")

echo "== 2. duplicate start is refused without a second process"
if "$ROOT/scripts/start-cli-proxy.sh" >"$STATE/start2.out" 2>&1; then
    echo "expected a duplicate start to exit non-zero" >&2
    exit 1
fi
grep -q "CLIProxyAPI is already running" "$STATE/start2.out"
[ "$(cat "$STATE/home/server.pid")" = "$FIRST_PID" ]

echo "== 3. --if-configured duplicate start is a no-op success"
"$ROOT/scripts/start-cli-proxy.sh" --if-configured >"$STATE/start3.out" 2>&1
grep -q "already running" "$STATE/start3.out"
[ "$(cat "$STATE/home/server.pid")" = "$FIRST_PID" ]

echo "== 4. restart stops the old listener and starts a fresh one"
if ! "$ROOT/scripts/restart-cli-proxy.sh" >"$STATE/restart.out" 2>&1; then
    echo "restart-cli-proxy.sh failed:" >&2
    cat "$STATE/restart.out" >&2
    exit 1
fi
grep -q "Stopping CLIProxyAPI" "$STATE/restart.out"
grep -q "CLIProxyAPI started" "$STATE/restart.out"
[ "$(cat "$STATE/home/server.pid")" != "$FIRST_PID" ]
wait_for_port
curl --noproxy '*' --silent --fail "http://127.0.0.1:$PORT/" >/dev/null

echo "== 5. a foreign process on the port is never killed or replaced"
kill "$(cat "$STATE/home/server.pid")" 2>/dev/null || true
if ! wait_for_port_free; then
    echo "sidecar did not release the port after SIGTERM" >&2
    exit 1
fi
rm -f "$STATE/home/server.pid"
python3 -c '
import http.server
server = http.server.ThreadingHTTPServer(("127.0.0.1", int(__import__("os").environ["MODEL_GATEWAY_CLI_PROXY_PORT"])), http.server.SimpleHTTPRequestHandler)
server.serve_forever()
' &
FOREIGN_PID=$!
disown "$FOREIGN_PID" 2>/dev/null || true
if ! wait_for_port; then
    echo "foreign listener did not open the port" >&2
    exit 1
fi
if "$ROOT/scripts/start-cli-proxy.sh" >"$STATE/foreign.out" 2>&1; then
    echo "expected refusal when the port is held by a non-CLIProxy process" >&2
    exit 1
fi
grep -q "occupied by a non-CLIProxy process" "$STATE/foreign.out"
kill "$FOREIGN_PID" 2>/dev/null || true
FOREIGN_PID=""

printf 'CLIProxy supervision smoke passed\n'
