# Getting Started

## Native Setup

### Quickstart (Environment-Only)

The fastest way to start: export API keys and run. Every recognized key activates its built-in provider:

```bash
export OPENROUTER_API_KEY="sk-or-v1-..."
export GOOGLE_GEMINI_API_KEY="..."
export MISTRAL_API_KEY="..."
cargo run -- serve
```

### Interactive Setup (TOML + Secrets)

```bash
cargo run -- setup              # prompts for providers and secrets
cargo run -- serve
```

`setup` writes non-secret configuration to `~/.config/model-gateway/config.toml`. Secrets are stored according to `MODEL_GATEWAY_SECRET_STORE`:
- `file` — written to protected `0700`/`0600` files (default)
- `keychain` — stored in the OS keychain (explicit opt-in)
- `environment` — read from environment variables at runtime; credentials cannot be persisted by the gateway

Run with `--offline` to skip catalog checks during setup.

### Using the Start Scripts

```bash
./scripts/start-server.sh       # builds, refreshes data, and starts
./scripts/start-server.sh -f    # same, but keep the gateway in the foreground
./scripts/restart-server.sh     # safely stops this gateway and restarts it
```

The scripts use `set -a` so `.env.local` variables are exported to the child process.

Unattended startup never depends on an interactive keychain session. When
`MODEL_GATEWAY_SECRET_STORE` is unset, both direct `cargo run -- serve` and
the launcher scripts use protected files under `MODEL_GATEWAY_SECRET_DIR` or
`~/.config/model-gateway/secrets`. To use the OS keychain, set
`MODEL_GATEWAY_SECRET_STORE=keychain` explicitly before running setup or the
server.

## Docker Setup

```bash
mkdir -p .model-gateway
export MODEL_GATEWAY_UID="$(id -u)" MODEL_GATEWAY_GID="$(id -g)"
docker compose --profile setup run --rm setup
docker compose up --build gateway
```

- Secrets live in a Docker named volume mounted read-only
- Host port: `127.0.0.1:8008`
- `docker compose down -v` deletes the credential volume
- For Ollama/LM Studio on the host: `http://host.docker.internal:<port>/v1`

## First-Run Checklist

1. Start the server: `cargo run -- serve`
2. Verify health: `curl http://127.0.0.1:8008/health/live`
3. List models: `curl http://127.0.0.1:8008/v1/models`
4. List providers: `curl http://127.0.0.1:8008/v1/providers`

### Using with Hermes

```yaml
model:
  provider: custom
  base_url: http://127.0.0.1:8008/v1
  default: local
```

## Refreshing Catalogs

The start script attempts a refresh before serving. A failed refresh does not delete the last known good snapshots; the server still starts so cached data remains available. Refresh explicitly when adding a provider or forcing new data:

```bash
model-gateway catalog refresh
model-gateway catalog status
```

This collects individual provider errors and reports all failures at the end. Embedding models are filtered at refresh time.

## Benchmarks

Benchmarks from [Artificial Analysis](https://artificialanalysis.ai/) are required for `auto-efficient`, `auto-balanced`, and `auto-frontier` routing. `auto-free` can operate with partial or missing benchmark coverage, but benchmarked candidates rank first. Set up:

```bash
model-gateway credentials set ARTIFICIAL_ANALYSIS_API_KEY
model-gateway benchmarks refresh
```

When the key is configured, the server polls Artificial Analysis on the
configured `data_refresh_interval_seconds` (one hour by default). It preserves
the last-known-good snapshot if a refresh fails. See [benchmarks.md](benchmarks.md)
for the ranking endpoint, freshness rules, configuration, and attribution.

The same server process also polls configured provider catalogs and the public
Models.dev pricing feed on that interval. No separate ingestion service is
required. Content fingerprints keep unchanged polls from creating duplicate
snapshots, while a changed catalog, price, cache rate, or benchmark score is
picked up automatically.
