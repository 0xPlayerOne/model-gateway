#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${MODEL_GATEWAY_PORT:-8008}"

# Source .env.local if it exists
ENV_LOCAL="$ROOT/.env.local"
if [ -f "$ENV_LOCAL" ]; then
    set -a
    source "$ENV_LOCAL"
    set +a
fi

echo "Starting model-gateway on port $PORT..."
exec cargo run --release --manifest-path "$ROOT/Cargo.toml" -- serve
