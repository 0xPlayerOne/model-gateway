#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
HERMES_COMMIT=3ef6bbd201263d354fd83ec55b3c306ded2eb72a
PROVIDER_PORT=${MODEL_GATEWAY_SMOKE_PROVIDER_PORT:-$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')}
GATEWAY_PORT=${MODEL_GATEWAY_SMOKE_GATEWAY_PORT:-$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')}
if [ "$GATEWAY_PORT" = "$PROVIDER_PORT" ]; then
    GATEWAY_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
fi
STATE=$(mktemp -d)
HERMES_HOME="$STATE/hermes"
PROVIDER_LOG="$STATE/provider.log"

cleanup() {
    status=$?
    if [ "$status" -ne 0 ] && [ -f "$STATE/gateway.log" ]; then
        printf '%s\n' 'Gateway log after smoke failure:' >&2
        cat "$STATE/gateway.log" >&2
    fi
    kill "${GATEWAY_PID:-}" "${PROVIDER_PID:-}" 2>/dev/null || true
    rm -rf "$STATE"
    exit "$status"
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
MOCK_PROVIDER_LOG="$PROVIDER_LOG" python3 "$ROOT/scripts/mock_provider.py" "$PROVIDER_PORT" &
PROVIDER_PID=$!
MODEL_GATEWAY_CONFIG="$STATE/gateway.toml" \
    MODEL_GATEWAY_SECRET_STORE=environment \
    "$ROOT/target/release/model-gateway" serve > "$STATE/gateway.log" 2>&1 &
GATEWAY_PID=$!

for _ in $(seq 1 50); do
    kill -0 "$GATEWAY_PID" "$PROVIDER_PID"
    if curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null; then
        break
    fi
    sleep 0.1
done
curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null

MODELS=$(curl --silent --show-error --fail --retry 3 \
    "http://127.0.0.1:${GATEWAY_PORT}/v1/models")
python3 -c 'import json,sys; assert json.loads(sys.argv[1])["data"][0]["id"] == "smoke"' "$MODELS"

NON_STREAMING=$(curl --silent --show-error --fail --retry 3 \
    "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"fixture"}}]}')
python3 -c 'import json,sys; assert json.loads(sys.argv[1])["choices"][0]["message"]["content"] == "smoke-ok"' "$NON_STREAMING"

curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","stream":true,"messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"fixture"}}]}' \
    | python3 -c 'import sys; assert "data: [DONE]" in sys.stdin.read()'

: > "$PROVIDER_LOG"
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

python3 - "$PROVIDER_LOG" <<'PY'
import json
import sys

entries = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
assert entries, "Hermes did not reach the provider"
assert any(entry["tools"] for entry in entries), entries
PY

printf 'Hermes v0.19.0 gateway smoke passed\n'
