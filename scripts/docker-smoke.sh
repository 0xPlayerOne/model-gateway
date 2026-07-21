#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
STATE=$(mktemp -d)

cleanup() {
    docker compose -f "$STATE/compose.yml" down -v --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$STATE"
}
trap cleanup EXIT

mkdir -p "$STATE/state"
cat > "$STATE/state/config.toml" <<'EOF'
[server]
bind = "0.0.0.0:11434"
exposure = "local_container"
max_body_bytes = 33554432
max_in_flight = 8
admission_timeout_ms = 250
shutdown_grace_seconds = 2

[providers.mock]
adapter = "openai_chat"
base_url = "http://host.docker.internal:39001/v1"
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
    environment:
      MODEL_GATEWAY_CONFIG: /app/state/config.toml
      MODEL_GATEWAY_CONTAINER_MODE: "1"
      MODEL_GATEWAY_SECRET_DIR: /run/model-gateway/secrets
    ports:
      - "127.0.0.1:39003:11434"
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

python3 "$ROOT/scripts/mock_provider.py" 39001 &
PROVIDER_PID=$!
trap 'kill "$PROVIDER_PID" 2>/dev/null || true; cleanup' EXIT

docker compose -f "$STATE/compose.yml" up --build --detach
for _ in $(seq 1 100); do
    if curl --silent --fail http://127.0.0.1:39003/health/ready >/dev/null; then
        break
    fi
    sleep 0.2
done
curl --silent --fail http://127.0.0.1:39003/health/ready >/dev/null
curl --silent --fail http://127.0.0.1:39003/v1/models \
    | python3 -c 'import json,sys; assert json.load(sys.stdin)["data"][0]["id"] == "smoke"'

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
    'test -z "${OPENROUTER_API_KEY:-}" && test ! -e /app/state/provider-key'

printf 'Docker gateway smoke passed\n'
