# model-gateway

Local Rust model gateway for routing OpenAI-compatible clients to configured
model providers. It is designed for one developer running locally, not as a
hosted service.

## Native quickstart

```bash
cargo run -- setup          # interactive one-time wizard
cargo run -- serve          # starts the server
```

The gateway does not load `.env` files itself. API keys must be exported before
starting, and every recognized API key automatically activates its built-in
provider without running `setup`:

```bash
export OPENROUTER_API_KEY="..."
cargo run -- serve
```

Or use the convenience scripts, which source `.env.local` automatically (with
`set -a` so variables are exported to the child process):

```bash
./scripts/start-server.sh     # fresh start (builds + runs)
./scripts/restart-server.sh   # stop old process + rebuild + start
```

When running CLI commands (`catalog refresh`, `benchmarks refresh`, etc.)
outside the start scripts, use `set -a && source .env.local && set +a` so
environment variables are visible to the gateway binary.

If the config file does not exist, the gateway starts from safe defaults and
uses the environment-derived providers. `setup` remains available for users
who want keychain/file secret storage and a generated TOML file. It writes only
non-secret routing configuration under `~/.config/model-gateway/config.toml`.
Use `--offline` to skip catalog checks.
Set `MODEL_GATEWAY_SECRET_STORE=file` to explicitly use protected `0700`/`0600`
storage, or `MODEL_GATEWAY_SECRET_STORE=environment` for environment-only
resolution. The gateway never silently falls back when a keychain is
unavailable.

Hermes can use the gateway as a custom endpoint:

```yaml
model:
  provider: custom
  base_url: http://127.0.0.1:8008/v1
  default: local
```

## Docker quickstart

Create the ignored state directory, then run the interactive setup container:

```bash
mkdir -p .model-gateway
export MODEL_GATEWAY_UID="$(id -u)"
export MODEL_GATEWAY_GID="$(id -g)"
docker compose --profile setup run --rm setup
docker compose up --build gateway
```

The provider secrets live in a Docker named volume mounted read-only by the
server. The host port is fixed to `127.0.0.1:8008`; do not broaden it without
designing caller authentication. `docker compose down -v` deletes the local
credential volume.

For Ollama or LM Studio on the host, configure the endpoint as
`http://host.docker.internal:<port>/v1` from the setup container. Container
`localhost` is not the host machine.

## Verification

```bash
curl http://127.0.0.1:8008/health/live
curl http://127.0.0.1:8008/v1/models
curl http://127.0.0.1:8008/v1/providers
```

Set `MODEL_GATEWAY_LOG_FORMAT=json` for structured logs. Both text and JSON
formats contain fixed request metadata only; prompts, responses, tools,
credentials, and arbitrary upstream errors are never logged.

The built-in `local` model relays the only model reported by an OpenAI-compatible
endpoint at `http://127.0.0.1:8000/v1`. Set `MODEL_GATEWAY_LOCAL_BASE_URL` and
`MODEL_GATEWAY_LOCAL_MODEL` when the endpoint or loaded model is different, and
`MODEL_GATEWAY_BIND` to override the gateway bind. If the local catalog reports
multiple models, an explicit model is required. Terminal assistant text is
decorated with one final model, reasoning-effort, and provider line; this
gateway-added text is not included in upstream token usage.

The `auto-free` model selects only models proven free by zero-price catalog
metadata, an official free-tier rule, or a `free_models` provider override.
Providers without an available API key are ignored. Request and token windows,
cooldowns, catalog snapshots, and opaque session pins are stored locally in a
protected SQLite database; prompts and responses are never stored. If all free
capacity is exhausted, routing falls back to `local`.

Free-tier provider rules are provider-specific: Google Gemini, Groq, Mistral,
NVIDIA NIM, Ollama Cloud, and SiliconFlow treat their
cataloged models as free-tier eligible and enforce the provider's configured or
published limits. Kilo Code refreshes its live model list and includes models
whose IDs contain `free`; Kilo's free model names change periodically. OpenCode
Zen uses `https://opencode.ai/zen/v1` and recognizes its explicitly free models,
including `big-pickle` and IDs containing `free`. OpenCode Go uses the same
`OPENCODE_API_KEY` against `https://opencode.ai/zen/go/v1`, but remains
subscription/paid-only (no models are treated as free-tier eligible). Nous Portal
and OrcaRouter only qualify models whose catalog metadata or IDs explicitly
indicate free access.

The `auto-efficient` model classifies each request locally, filters the current
catalog against capabilities and a fresh benchmark snapshot, removes dominated
candidates, and chooses the lowest expected-cost model above the configured
quality floor. Paid and subscription offerings are considered only when their
provider has an explicit `billing_mode = "paid"` or `"subscription"`; providers
default to free-only. Configure `cost_microusd` quota windows to impose
transactional spend caps. Reservations expire after abandoned requests and are
reconciled against provider-reported token usage when available. If no
benchmarked authorized candidate remains, the route falls back once through
`auto-free` and then `local`. Each automatic route can be independently
disabled with `server.auto_free_enabled`, `server.auto_efficient_enabled`, or
`server.auto_frontier_enabled`; disabled routes are omitted from `/v1/models`
and reject requests with `route_disabled`.

The `auto-frontier` model applies the same selector with an additional canonical
creator constraint: only benchmark entries identified exactly as OpenAI or
Anthropic are eligible, regardless of which configured provider carries the
offering. It uses independent frontier quality floors, requires explicit paid
or subscription authorization, and excludes preview/beta/experimental model
IDs unless that provider sets `allow_preview_models = true`. If no eligible
candidate is safe and available, the gateway returns a fixed OpenAI-shaped
frontier error; it never falls back to `auto-efficient`, `auto-free`, or
`local`. Set `server.auto_frontier_enabled = false` to hide and disable the
route during a controlled rollout.

Refresh dynamic provider catalogs explicitly with `model-gateway catalog
refresh`, and inspect cache age with `model-gateway catalog status`. Override
the state location with `MODEL_GATEWAY_STATE_PATH`. Catalog refresh collects
individual provider errors and reports all failures at the end rather than
aborting on the first one. Embedding models are filtered from provider catalogs
at refresh time.

Environment overrides are applied on every load and take precedence over TOML.
Provider overrides use the normalized provider name, for example
`MODEL_GATEWAY_OPENROUTER_BILLING_MODE`,
`MODEL_GATEWAY_OPENROUTER_MODEL_ALLOWLIST`, and
`MODEL_GATEWAY_OPENROUTER_MODEL_DENYLIST`. Supported provider overrides also
include `BASE_URL`, `API_KEY_SECRET`, `ACCOUNT_SCOPE`, `FREE_MODELS`,
`ALLOW_PREVIEW_MODELS`, `ALLOW_MODEL_PASSTHROUGH`, `ALLOW_INSECURE_HTTP`,
`MAX_IN_FLIGHT`, timeout fields, `EXTRA_HEADERS`, `MODEL_MAPPINGS`, and
`QUOTAS`. Quotas use `kind:limit:window_seconds[:boundary]` entries separated
by semicolons. Server settings use the corresponding `MODEL_GATEWAY_` names,
including `BIND`, `LOCAL_BASE_URL`, `LOCAL_MODEL`, `EXPOSURE`, `MAX_BODY_BYTES`,
`MAX_IN_FLIGHT`, `ADMISSION_TIMEOUT_MS`, `SHUTDOWN_GRACE_SECONDS`,
`LOCAL_MODEL_CACHE_SECONDS`, `CATALOG_MAX_AGE_SECONDS`,
`BENCHMARK_MAX_AGE_SECONDS`, `QUALITY_FLOOR_SIMPLE`,
`QUALITY_FLOOR_MEDIUM`, `QUALITY_FLOOR_COMPLEX`,
`FRONTIER_QUALITY_FLOOR_SIMPLE`, `FRONTIER_QUALITY_FLOOR_MEDIUM`,
`FRONTIER_QUALITY_FLOOR_COMPLEX`, `AUTO_FRONTIER_ENABLED`,
`AUTO_FREE_ENABLED`, and `AUTO_EFFICIENT_ENABLED`.

Benchmark data is required for `auto-efficient` and `auto-frontier` routing.
Get a free [Artificial Analysis API key](https://artificialanalysis.ai/), then:

```bash
model-gateway credentials set ARTIFICIAL_ANALYSIS_API_KEY
```

The server **auto-fetches benchmarks on startup** if the API key is configured
and no fresh data exists, and keeps it updated on a background schedule
(default: every ~3.5 days). You can also trigger a refresh manually:

```bash
model-gateway benchmarks refresh
```

This fetches verified quality, pricing, and latency scores for 500+ models with
required attribution. The `auto-efficient` and `auto-frontier` routes use
benchmark-backed Pareto selection and fall back only when no benchmarked
candidate is safely available.

Inspect active snapshots with `model-gateway benchmarks status`, view live
rankings at `/v1/rankings?task=coding&limit=50`, and delete stale snapshots
with `model-gateway benchmarks delete <source>`. Supported tasks are `general`,
`coding`, `agentic`, and `reasoning`. Import from other licensed sources with
`model-gateway benchmarks import --file <path>`.

Inspect free models for one provider with
`/v1/free-models?provider=kilocode&limit=100`. The provider value is the
configured provider key, such as `kilocode`, `opencode-zen`, `opencode-go`,
`openrouter`, or `google-gemini`. Use `provider=all` for the same unfiltered
result as omitting the parameter. Unknown provider keys return
`invalid_provider`.

Free-model records include a `scores` object with the matched Artificial
Analysis `general`, `coding`, and `agentic` scores when available. The
`task=general|coding|agentic` query selects the corresponding score for
ranking; matching normalizes provider prefixes, punctuation, and common model
version suffixes. Results sort benchmarked models first (descending by quality
score), then models with known limit status, then by provider and model name.

List currently configured providers with available API keys at
`/v1/providers`. The response includes each provider's code name, display name,
endpoint, billing mode, secret source, model catalog counts, allowlist/denylist
counts, and `available` status; credential values are never returned. Use
`?available=true` or `?available=false` to filter providers by resolved key
availability. Catalog and free-model counts reflect the current fresh catalog
snapshot.

Query free models for every provider at once with
`GET /v1/free-models?limit=50`. The response includes a `providers` map with
each provider's display name, billing mode, rate limits, and limit reference
URL.

## Supported Profiles

The setup wizard uses one declarative registry. Provider recommendations follow
`.env.example`: CORE profiles are highly encouraged, SECONDARY profiles are
useful additions, and OPTIONAL PAID profiles require subscriptions or credits.
The CORE profiles are Google Gemini, Kilo Code, Ollama Cloud, OpenCode Zen, and
OpenRouter. SECONDARY profiles are Groq, Mistral, Nous Portal, NVIDIA
NIM, and SiliconFlow. OPTIONAL PAID profiles are DeepSeek, Fireworks, OpenAI
API, OpenCode Go, OrcaRouter, and Z.AI. All profiles use OpenAI Chat
Completions with bearer secrets. They are contract-tested against deterministic
local fixtures; no provider credential is required for CI.

OpenCode Go and Zen share `OPENCODE_API_KEY` but use separate model catalogs
and billing modes (subscription vs free). OpenCode Go enforces cost-based
quota windows for spend control.

Kilo Code now supports live catalog discovery — models are fetched from its
OpenAI-compatible endpoint instead of being backend-only. The Anthropic key
remains documented as an optional paid credential, but its native Messages
adapter is not implemented yet.

`gateway.core.example.toml`, `gateway.secondary.example.toml`, and
`gateway.optional.example.toml` mirror the three `.env.example` sections. Run
`./scripts/core-provider-check.sh` for an explicit one-time connection check
with `.env.local`. It checks all three configs, sends only documented
model-catalog or key-status GET requests, reports every provider before
returning a failure summary, and is intentionally not part of CI.

## Limitations

The gateway supports the OpenAI Chat Completions wire protocol only. It has no
caller authentication or public/LAN bind, does not retry ambiguous transport
failures, does not call providers at startup, and has no config hot reload,
native-protocol adapters, or OAuth-managed credentials.

## Development

```bash
cargo test
cargo run -- --help
```

## Installation

Build from source with the pinned toolchain:

```bash
cargo install --locked --path .
```

Tagged releases publish checksummed native archives for Linux x86_64,
macOS Intel, and macOS Apple Silicon, plus a multi-architecture container
image. Pull the container from GitHub Container Registry with the release tag,
then follow the Docker quickstart above. Release publication is tag-gated;
ordinary branches only run the packaging and container dry-runs.

## License

This project is dual-licensed under the MIT License or Apache License 2.0, at
your option. See `LICENSE-MIT` and `LICENSE-APACHE`.
