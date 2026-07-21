#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
HERMES_COMMIT=3ef6bbd201263d354fd83ec55b3c306ded2eb72a
PROVIDER_PORT=${MODEL_GATEWAY_SMOKE_PROVIDER_PORT:-39001}
GATEWAY_PORT=${MODEL_GATEWAY_SMOKE_GATEWAY_PORT:-39002}
STATE=$(mktemp -d)
HERMES_HOME="$STATE/hermes"

cleanup() {
    kill "${GATEWAY_PID:-}" "${PROVIDER_PID:-}" 2>/dev/null || true
    rm -rf "$STATE"
}
trap cleanup EXIT

mkdir -p "$HERMES_HOME"/{audio_cache,cron,hooks,image_cache,logs,memories,pairing,sandboxes,sessions,skills}

cat > "$STATE/gateway.toml" <<EOF
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
allow_model_passthrough = false
allow_insecure_http = false
connect_timeout_seconds = 2
response_header_timeout_seconds = 5
stream_idle_timeout_seconds = 5

[models.smoke]
[[models.smoke.targets]]
provider = "mock"
model = "upstream-smoke"
EOF

cat > "$HERMES_HOME/config.yaml" <<EOF
model:
  provider: custom
  default: smoke
  base_url: http://127.0.0.1:${GATEWAY_PORT}/v1
  api_key: ""
EOF

printf '%s\n' 'You are an isolated smoke-test assistant.' > "$HERMES_HOME/SOUL.md"

cargo build --release --manifest-path "$ROOT/Cargo.toml"
python3 "$ROOT/scripts/mock_provider.py" "$PROVIDER_PORT" &
PROVIDER_PID=$!
MODEL_GATEWAY_CONFIG="$STATE/gateway.toml" \
    MODEL_GATEWAY_SECRET_STORE=environment \
    "$ROOT/target/release/model-gateway" serve > "$STATE/gateway.log" 2>&1 &
GATEWAY_PID=$!

for _ in $(seq 1 50); do
    if curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null; then
        break
    fi
    sleep 0.1
done
curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null

curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/models" \
    | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"][0]["id"] == "smoke"'

curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"fixture"}}]}' \
    | python3 -c 'import json,sys; assert json.load(sys.stdin)["choices"][0]["message"]["content"] == "smoke-ok"'

curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","stream":true,"messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"fixture"}}]}' \
    | python3 -c 'import sys; assert "data: [DONE]" in sys.stdin.read()'

HERMES_OUTPUT=$(HERMES_HOME="$HERMES_HOME" uvx \
    --from "git+https://github.com/NousResearch/hermes-agent.git@${HERMES_COMMIT}" \
    hermes --oneshot 'Reply once without calling a tool.' --safe-mode)
case "$HERMES_OUTPUT" in
    *smoke-ok*) ;;
    *)
        printf 'Unexpected Hermes output: %s\n' "$HERMES_OUTPUT" >&2
        exit 1
        ;;
esac

printf 'Hermes v0.19.0 gateway smoke passed\n'
