#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
STATE=$(mktemp -d)
PROVIDER_PORT=${MODEL_GATEWAY_SMOKE_PROVIDER_PORT:-39001}
GATEWAY_PORT=${MODEL_GATEWAY_SMOKE_GATEWAY_PORT:-$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')}
if [ "$GATEWAY_PORT" = "$PROVIDER_PORT" ]; then
    GATEWAY_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
fi

cleanup() {
    rc=${1:-$?}
    if [ "$rc" -ne 0 ] && [ -f "$STATE/compose.yml" ]; then
        docker compose -f "$STATE/compose.yml" ps >&2 || true
        docker compose -f "$STATE/compose.yml" logs --no-color >&2 || true
    fi
    docker compose -f "$STATE/compose.yml" down -v --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$STATE"
    return "$rc"
}
trap cleanup EXIT

mkdir -p "$STATE/state"
cat > "$STATE/state/config.toml" <<EOF
[server]
bind = "0.0.0.0:11434"
exposure = "local_container"
max_body_bytes = 33554432
max_in_flight = 8
admission_timeout_ms = 250
shutdown_grace_seconds = 2

[providers.mock]
adapter = "openai_chat"
base_url = "http://host.docker.internal:${PROVIDER_PORT}/v1"
api_key_secret = "MOCK_API_KEY"
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

cat > "$STATE/compose.yml" <<EOF
services:
  gateway:
    build:
      context: "$ROOT"
      args:
        MODEL_GATEWAY_UID: "$(id -u)"
        MODEL_GATEWAY_GID: "$(id -g)"
    read_only: true
    tmpfs:
      - /tmp:rw,noexec,nosuid,size=16m
    cap_drop:
      - ALL
    security_opt:
      - no-new-privileges:true
    environment:
      MODEL_GATEWAY_CONFIG: /app/state/config.toml
      MODEL_GATEWAY_CONTAINER_MODE: "1"
      MODEL_GATEWAY_SECRET_DIR: /run/model-gateway/secrets
    ports:
       - "127.0.0.1:${GATEWAY_PORT}:11434"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    volumes:
      - "$STATE/state:/app/state:ro"
      - secrets:/run/model-gateway/secrets:ro
  setup:
    build:
      context: "$ROOT"
      args:
        MODEL_GATEWAY_UID: "$(id -u)"
        MODEL_GATEWAY_GID: "$(id -g)"
    profiles: ["setup"]
    environment:
      MODEL_GATEWAY_CONFIG: /app/state/config.toml
      MODEL_GATEWAY_CONTAINER_MODE: "1"
      MODEL_GATEWAY_SECRET_DIR: /run/model-gateway/secrets
    volumes:
      - "$STATE/state:/app/state"
      - secrets:/run/model-gateway/secrets
volumes:
  secrets:
EOF

MOCK_PROVIDER_API_KEY=fixture-secret MOCK_PROVIDER_HOST=0.0.0.0 \
    python3 "$ROOT/scripts/mock_provider.py" "$PROVIDER_PORT" &
PROVIDER_PID=$!
trap 'rc=$?; kill "$PROVIDER_PID" 2>/dev/null || true; cleanup "$rc"' EXIT

docker compose -f "$STATE/compose.yml" --profile setup run --rm --no-deps --entrypoint sh setup -c \
    'mkdir -p /run/model-gateway/secrets && printf %s fixture-secret > /run/model-gateway/secrets/MOCK_API_KEY && chmod 700 /run/model-gateway/secrets && chmod 600 /run/model-gateway/secrets/MOCK_API_KEY'

docker compose -f "$STATE/compose.yml" up --build --detach
for _ in $(seq 1 100); do
    if curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null; then
        break
    fi
    sleep 0.2
done
curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/health/ready" >/dev/null
curl --silent --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/models" \
    | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"][0]["id"] == "smoke"'

curl --silent --show-error --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","messages":[],"tools":[{"type":"function","function":{"name":"fixture"}}]}' \
    | python3 -c 'import json,sys; assert json.load(sys.stdin)["model"] == "upstream-smoke"'

curl --silent --show-error --fail "http://127.0.0.1:${GATEWAY_PORT}/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"smoke","stream":true,"messages":[]}' \
    | python3 -c 'import sys; assert "data: [DONE]" in sys.stdin.read()'

docker compose -f "$STATE/compose.yml" --profile setup run --rm --no-deps --entrypoint sh setup -c \
    'cp /app/state/config.toml /app/state/config.toml.tmp && chmod 600 /app/state/config.toml.tmp && mv /app/state/config.toml.tmp /app/state/config.toml && mkdir -p /run/model-gateway/secrets && chmod 700 /run/model-gateway/secrets && touch /run/model-gateway/secrets/fixture && chmod 600 /run/model-gateway/secrets/fixture && test "$(stat -c %a /run/model-gateway/secrets)" = 700 && test "$(stat -c %a /run/model-gateway/secrets/fixture)" = 600'

if docker compose -f "$STATE/compose.yml" run --rm --no-deps --entrypoint touch gateway \
    /run/model-gateway/secrets/should-not-write; then
    printf 'Gateway secret mount unexpectedly allowed writes\n' >&2
    exit 1
fi

if docker compose -f "$STATE/compose.yml" run --rm --no-deps \
    -e MODEL_GATEWAY_CONTAINER_MODE=0 --entrypoint model-gateway gateway serve; then
    printf 'local_container mode started without the container marker\n' >&2
    exit 1
fi

docker compose -f "$STATE/compose.yml" run --rm --no-deps --entrypoint sh gateway -c \
    'test -z "${OPENROUTER_API_KEY:-}" && test ! -e /app/state/provider-key && test -e /run/model-gateway/secrets/MOCK_API_KEY && test "$(cat /run/model-gateway/secrets/MOCK_API_KEY)" = fixture-secret'

docker compose -f "$STATE/compose.yml" run --rm --no-deps --entrypoint sh gateway -c \
    'test "$(id -u)" != 0 && test "$(awk "/^CapEff:/{print \$2}" /proc/self/status)" = 0000000000000000 && ! touch /app/should-not-write && test -d /tmp'

printf 'Docker gateway smoke passed\n'
