# model-gateway

Local Rust gateway for routing OpenAI-compatible clients to configured model providers. Designed for one developer running locally — not a hosted service.

## Quickstart

```bash
cargo run -- setup          # interactive one-time wizard
cargo run -- serve          # starts on http://127.0.0.1:8008
```

No `.env` loading — export keys before starting. Any recognized key auto-activates its provider:

```bash
export OPENROUTER_API_KEY="..."
cargo run -- serve
```

Or use the convenience scripts (sources `.env.local` automatically):

```bash
./scripts/start-server.sh     # build + run
./scripts/restart-server.sh   # stop + rebuild + start
```

> Environment variables must be visible to the gateway binary. For ad-hoc CLI commands (`catalog refresh`, etc.) outside the start scripts, use `set -a && source .env.local && set +a`.

## Docker Quickstart

```bash
mkdir -p .model-gateway
export MODEL_GATEWAY_UID="$(id -u)" MODEL_GATEWAY_GID="$(id -g)"
docker compose --profile setup run --rm setup
docker compose up --build gateway
```

Secrets live in a Docker named volume mounted read-only. Host port fixed to `127.0.0.1:8008`. For Ollama/LM Studio on the host, use `http://host.docker.internal:<port>/v1`. See [docs/getting-started.md](docs/getting-started.md) for details.

## Verification

```bash
curl http://127.0.0.1:8008/health/live
curl http://127.0.0.1:8008/v1/models
curl http://127.0.0.1:8008/v1/providers
```

## Built-in Routes

Each mode picks ONE model from the Pareto frontier. Session pinning keeps you on that model for the entire session — no cache misses from model switching. The Pareto frontier handles reasoning effort automatically (e.g., picks GPT 5.6 Sol over Sol Max for efficiency).

| Route | Quality Floor | Description | Benchmarks |
|---|---|---|---|
| `local` | — | Relays the only model from an OpenAI-compatible endpoint (default `127.0.0.1:8000`). | No |
| `auto-free` | Free quality bar | Best free model. Falls back to `local`. | Recommended |
| `auto-efficient` | 40 | Best bang-for-buck. Pareto ranks by composite quality, cost, latency. Falls back to `auto-free`, then `local`. | **Yes** |
| `auto-balanced` | 60 | Mid-range quality. Great models, affordable pricing. Falls back to `auto-free`, then `local`. | **Yes** |
| `auto-frontier` | 80 | Top tier. OpenAI/Anthropic canonical models only. Never falls back. | **Yes** |

Composite quality score: `0.5*intelligence + 0.3*coding + 0.2*agentic` — well-rounded, not task-specific.

See [docs/routing.md](docs/routing.md) for detailed routing logic and cache-aware design.

## Configuration

The gateway starts from safe defaults using only environment variables. For TOML-based config with keychain/file secrets, run `cargo run -- setup`. Config lives at `~/.config/model-gateway/config.toml`.

**Environment overrides** (take precedence over TOML):

```
MODEL_GATEWAY_BIND=127.0.0.1:8008
MODEL_GATEWAY_LOCAL_BASE_URL=http://localhost:8000/v1
MODEL_GATEWAY_LOCAL_MODEL=my-model
MODEL_GATEWAY_EXPOSURE=loopback          # loopback|private|docker-local
MODEL_GATEWAY_SECRET_STORE=environment   # environment|file|keychain
MODEL_GATEWAY_LOG_FORMAT=json            # text|json
MODEL_GATEWAY_STATE_PATH=~/.config/model-gateway/routing.sqlite3
```

Provider overrides use the normalized provider name (e.g., `MODEL_GATEWAY_OPENROUTER_BILLING_MODE=paid`). See [docs/configuration.md](docs/configuration.md) for the full list of supported overrides.

## Benchmarks

Quality benchmarks are sourced from [Artificial Analysis](https://artificialanalysis.ai/) and are **required** for `auto-efficient` and `auto-frontier` routing. Set up your API key:

```bash
export ARTIFICIAL_ANALYSIS_API_KEY="your-key"
model-gateway benchmarks refresh
```

The gateway auto-fetches on startup if the key is configured with no fresh data. View live rankings at `/v1/rankings?task=coding&limit=20`. See [docs/benchmarks.md](docs/benchmarks.md) for full details on setup, configuration, and attribution.

## Free Models

Query available free models:

```bash
curl /v1/free-models?provider=kilocode&limit=100&task=coding
```

Supported tasks: `general`, `coding`, `agentic`. Provider values match configured keys (e.g., `kilocode`, `opencode-zen`, `google-gemini`, `openrouter`). Unknown providers return `invalid_provider`. See [docs/providers.md](docs/providers.md) for free-tier eligibility rules.

## Paid Models

Query models from explicitly authorized paid providers:

```bash
curl /v1/paid-models?task=coding&limit=50
```

Only appears when at least one provider has `billing_mode = "paid"` or `"subscription"`. All providers default to free — enable paid with:

```bash
export MODEL_GATEWAY_PAID_BILLING_MODE=openai-api,deepseek
```

Or per-provider: `MODEL_GATEWAY_OPENAI_API_BILLING_MODE=paid`. See [docs/configuration.md](docs/configuration.md) for details.

## CLI Commands

| Command | Description |
|---|---|
| `setup` | Interactive configuration wizard |
| `serve` | Start the gateway server |
| `config check` | Validate current configuration |
| `config show` | Print resolved configuration |
| `credentials set <name>` | Store a credential |
| `credentials list` | List stored credential names |
| `catalog refresh` | Fetch live model catalogs from providers |
| `catalog status` | Check catalog cache age |
| `benchmarks refresh` | Fetch/update Artificial Analysis benchmarks |
| `benchmarks status` | Inspect active benchmark snapshots |
| `benchmarks import --file <path>` | Import benchmarks from a file |
| `benchmarks delete <source>` | Delete stale snapshots |
| `healthcheck` | Verify the server is running |

## Development

```bash
cargo test                          # run all tests
cargo fmt --check                   # formatting
cargo clippy -- -D warnings         # lint
cargo run -- --help                 # CLI help
```

## Installation

```bash
cargo install --locked --path .
```

Tagged releases publish checksummed native archives (Linux x86_64, macOS Intel, macOS ARM) plus multi-arch container images on GitHub Container Registry.

## Limits

- OpenAI Chat Completions wire protocol only
- No caller authentication (loopback-only bind)
- No config hot reload
- No native-protocol adapters

## License

Dual-licensed MIT / Apache 2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
