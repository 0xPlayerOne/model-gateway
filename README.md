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

## Claude and Codex OAuth

CLIProxyAPI can run as an optional loopback sidecar for Claude Code and ChatGPT/Codex subscriptions with multi-account rotation:

```bash
model-gateway cli-proxy setup
model-gateway cli-proxy login claude
model-gateway cli-proxy login codex --device
./scripts/start-server.sh
```

Repeat either login command to add accounts to the pool. The setup command downloads checksum-pinned CLIProxyAPI `v7.2.103`, binds it to `127.0.0.1:8317` by default, disables remote management/plugins/control-panel updates, and creates a `subscription` provider. To use another sidecar port, set `MODEL_GATEWAY_CLI_PROXY_PORT` before setup; keep it unset or equal to the generated config when launching. It does not replace direct APIs, Ollama, LM Studio, or the built-in local endpoint. See [docs/providers.md](docs/providers.md#cliproxyapi-oauth-sidecar) for security and provider-policy limitations.

The launcher backgrounds CLIProxyAPI and writes logs to its sidecar directory. Once CLIProxyAPI is configured, `./scripts/start-server.sh` and `./scripts/restart-server.sh` start and restart both services automatically. Use `./scripts/start-server.sh --follow` (or `-f`) to keep the gateway in the foreground; the sidecar remains managed in the background. Use `./scripts/start-cli-proxy.sh --follow` when you specifically want to follow only sidecar logs.

For unattended startup the launchers default `MODEL_GATEWAY_SECRET_STORE` to `file` (non-interactive protected-file store) when it is unset and print the effective store; set it explicitly (e.g. `keychain` in `.env.local`) to opt into the OS keychain.

## Verification

```bash
curl http://127.0.0.1:8008/health/live
curl http://127.0.0.1:8008/v1/models
curl http://127.0.0.1:8008/v1/providers
```

## Built-in Routes

Each automatic mode selects one primary model and up to two fallbacks. Session pinning keeps successful requests on the same provider/model for 30 minutes when a session identity is available. Reasoning-effort variants are ranked independently when benchmark data distinguishes them.

| Route | Quality Floor | Description | Benchmarks |
|---|---|---|---|
| `local` | — | Relays the only model from an OpenAI-compatible endpoint (default `127.0.0.1:8000`). | No |
| `auto-free` | Free quality bar | Best eligible free model. Falls back to unbenchmarked free candidates, then `local`. | Optional |
| `auto-efficient` | 35 | Cost-first selection among models that meet the quality floor, with latency as a tie-breaking axis. Falls back to `auto-free`, then `local`. | **Yes** |
| `auto-balanced` | 42 | Higher-quality selection with the same measured-cost and latency safeguards. Falls back to `auto-free`, then `local`. | **Yes** |
| `auto-frontier` | 52 | Highest quality floor; the frontier is ordered with 50% quality, 25% measured task-cost efficiency, and 25% latency efficiency. | **Yes** |

Composite quality score: `0.80*intelligence + 0.10*coding + 0.10*agentic`, with missing task scores redistributed to intelligence. The score is used by automatic routes; catalog task filters use the requested task score when available.

See [docs/routing.md](docs/routing.md) for detailed routing logic and cache-aware design.

## Configuration

The gateway starts from safe defaults using only environment variables. For TOML-based config with keychain/file secrets, run `cargo run -- setup`. Config lives at `~/.config/model-gateway/config.toml`.

**Environment overrides** (take precedence over TOML):

```
MODEL_GATEWAY_BIND=127.0.0.1:8008
MODEL_GATEWAY_LOCAL_BASE_URL=http://localhost:8000/v1
MODEL_GATEWAY_LOCAL_MODEL=my-model
MODEL_GATEWAY_EXPOSURE=loopback          # loopback|local_container
MODEL_GATEWAY_SECRET_STORE=file         # file|keychain|environment; keychain is explicit opt-in
MODEL_GATEWAY_LOG_FORMAT=json            # text|json
MODEL_GATEWAY_STATE_PATH=~/.config/model-gateway/routing.sqlite3
```

Provider overrides use the normalized provider name (e.g., `MODEL_GATEWAY_OPENROUTER_BILLING_MODE=paid`). See [docs/configuration.md](docs/configuration.md) for the full list of supported overrides.

## Benchmarks

Quality benchmarks are sourced from [Artificial Analysis](https://artificialanalysis.ai/) and are required for `auto-efficient`, `auto-balanced`, and `auto-frontier`. `auto-free` can use benchmarked models and retains eligible unbenchmarked free models as lower-priority fallbacks. Set up the API key:

```bash
export ARTIFICIAL_ANALYSIS_API_KEY="your-key"
model-gateway benchmarks refresh
```

The gateway auto-fetches on startup if the key is configured with no fresh data. View live rankings at `/v1/rankings?task=coding&limit=20`. See [docs/benchmarks.md](docs/benchmarks.md) for full details on setup, configuration, and attribution.

## Free Models

Query the canonical model catalog:

```bash
curl '/v1/catalog/models?access=free&provider=kilocode&limit=25&task=coding'
```

Supported tasks: `general`, `coding`, `agentic`. Provider values match configured keys (e.g., `kilocode`, `opencode-zen`, `google-gemini`, `openrouter`). Unknown providers return `invalid_provider`. See [docs/providers.md](docs/providers.md) for free-tier eligibility rules.

## Paid Models

Query models from explicitly authorized paid providers:

```bash
curl '/v1/catalog/models?access=paid&task=coding&limit=25'

# Fetch complete metadata for one model from its summary link
curl /v1/catalog/models/provider/model

# Inspect the machine-readable API contract
curl /openapi.json
```

Only appears when at least one provider has `billing_mode = "paid"` or `"subscription"`. Providers default to free except the generated CLIProxyAPI subscription profile. Enable paid APIs with:

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
| `pricing refresh` | Fetch provider-scoped public pricing from models.dev |
| `pricing import --file <path>` | Import exact provider/model pricing overrides |
| `pricing status` | Inspect active pricing snapshots |
| `pricing coverage [--provider <name>] [--json]` | Report complete, incomplete, and missing pricing per catalog model |
| `pricing explain <provider> <model>` | Show the selected effective price source |
| `matching reconcile [--provider <name>] [--json] [--check]` | Report identity coverage; fail on mapping drift or ambiguity |
| `matching refresh` | Refresh source-backed model identities from models.dev and OpenRouter |
| `matching status` | Inspect active identity source snapshots |
| `matching approve <provider> <catalog-model> <benchmark-model>` | Approve a provider-scoped benchmark identity |
| `matching approve-entity <entity-id> <benchmark-model>` | Link a canonical entity to a benchmark for deterministic propagation |
| `matching link-alias <provider-key> <provider-model-id> <entity-id>` | Approve a source-backed provider alias for a canonical entity |
| `matching remove <provider> <catalog-model>` | Remove an approved identity mapping |
| `matching remove-entity <entity-id> <benchmark-model>` | Remove a canonical entity benchmark link |
| `matching unlink-alias <provider-key> <provider-model-id>` | Remove an approved canonical provider alias |
| `matching explain <provider> <catalog-model>` | Explain one model's identity resolution |
| `cli-proxy setup [--force]` | Install and configure the pinned OAuth sidecar |
| `cli-proxy login claude` | Add a Claude OAuth account |
| `cli-proxy login codex [--device]` | Add a ChatGPT/Codex OAuth account |
| `cli-proxy serve` | Run the loopback CLIProxyAPI sidecar |
| `cli-proxy status` | Check authenticated sidecar readiness and model count |
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

Licensed under the GNU Affero General Public License v3.0 or later. See `LICENSE` and `NOTICE`.
