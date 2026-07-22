#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
STATE=$(mktemp -d)
GATEWAY_PORT=${MODEL_GATEWAY_NATIVE_SMOKE_PORT:-$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')}
PROVIDER_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')

cleanup() {
    rc=${1:-$?}
    kill "${GATEWAY_PID:-}" "${PROVIDER_PID:-}" 2>/dev/null || true
    wait "${GATEWAY_PID:-}" "${PROVIDER_PID:-}" 2>/dev/null || true
    rm -rf "$STATE"
    return "$rc"
}
trap cleanup EXIT

mkdir -p "$STATE/home" "$STATE/config"
cat > "$STATE/config/config.toml" <<EOF
[server]
bind = "127.0.0.1:${GATEWAY_PORT}"
exposure = "loopback"
max_body_bytes = 33554432
max_in_flight = 8
admission_timeout_ms = 250
shutdown_grace_seconds = 2

[providers.mock]
adapter = "openai_chat"
base_url = "http://127.0.0.1:${PROVIDER_PORT}/v1"
api_key_secret = "LOCAL_API_KEY"
allow_model_passthrough = false
allow_insecure_http = true
connect_timeout_seconds = 2
response_header_timeout_seconds = 5
stream_idle_timeout_seconds = 5

[models.smoke]
[[models.smoke.targets]]
provider = "mock"
model = "upstream-smoke"
EOF

MOCK_PROVIDER_API_KEY=fixture-secret python3 "$ROOT/scripts/mock_provider.py" "$PROVIDER_PORT" &
PROVIDER_PID=$!
for _ in $(seq 1 100); do
    if curl --noproxy '*' --silent --fail "http://127.0.0.1:${PROVIDER_PORT}/v1/models" >/dev/null; then
        break
    fi
    sleep 0.1
done
curl --noproxy '*' --silent --fail "http://127.0.0.1:${PROVIDER_PORT}/v1/models" >/dev/null
HOME="$STATE/home" \
MODEL_GATEWAY_CONFIG="$STATE/config/config.toml" \
MODEL_GATEWAY_STATE_PATH="$STATE/routing.sqlite3" \
MODEL_GATEWAY_SECRET_STORE=environment \
LOCAL_API_KEY=fixture-secret \
NO_PROXY=127.0.0.1,localhost \
no_proxy=127.0.0.1,localhost \
HTTP_PROXY= \
http_proxy= \
HTTPS_PROXY= \
https_proxy= \
ALL_PROXY= \
all_proxy= \
    "$ROOT/target/release/model-gateway" serve >"$STATE/gateway.log" 2>&1 &
GATEWAY_PID=$!

for _ in $(seq 1 100); do
    if curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null; then
        break
    fi
    sleep 0.1
done
curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null
curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/models" \
    | python3 -c 'import json,sys; assert [item["id"] for item in json.load(sys.stdin)["data"]][:3] == ["local", "auto-free", "smoke"]'
curl --silent --show-error --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","messages":[]}' \
    | python3 -c 'import json,sys; assert json.load(sys.stdin)["model"] == "upstream-smoke"'
curl --silent --show-error --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","stream":true,"messages":[]}' \
    | python3 -c 'import sys; assert "data: [DONE]" in sys.stdin.read()'

kill -TERM "$GATEWAY_PID"
wait "$GATEWAY_PID"
GATEWAY_PID=

printf 'Native gateway smoke passed\n'
