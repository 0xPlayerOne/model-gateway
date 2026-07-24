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
- `environment` — read from env vars at runtime (default)
- `file` — written to protected `0700`/`0600` files
- `keychain` — stored in the OS keychain

Run with `--offline` to skip catalog checks during setup.

### Using the Start Scripts

```bash
./scripts/start-server.sh     # builds + sources .env.local + runs
./scripts/restart-server.sh   # stops old process + rebuilds + starts
```

The scripts use `set -a` so `.env.local` variables are exported to the child process.

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

Provider catalogs are not fetched at startup. Refresh explicitly:

```bash
model-gateway catalog refresh
model-gateway catalog status
```

This collects individual provider errors and reports all failures at the end. Embedding models are filtered at refresh time.

## Benchmarks

Benchmarks are required for `auto-efficient` and `auto-frontier` routing:

```bash
model-gateway credentials set ARTIFICIAL_ANALYSIS_API_KEY
```

The server auto-fetches benchmarks on startup if the key is configured and no fresh data exists. Manual refresh:

```bash
model-gateway benchmarks refresh
model-gateway benchmarks status
```

Benchmark data is parsed from [Artificial Analysis](https://artificialanalysis.ai/). Import from other sources with `model-gateway benchmarks import --file <path>`.
