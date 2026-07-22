# model-gateway

Local Rust model gateway for routing OpenAI-compatible clients to configured
model providers. It is designed for one developer running locally, not as a
hosted service.

## Native quickstart

```bash
cargo run -- setup
cargo run -- serve
```

`setup` stores provider keys in the macOS Keychain or Linux Secret Service and
writes only non-secret routing configuration under
`~/.config/model-gateway/config.toml`. Use `--offline` to skip catalog checks.
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

The `auto-efficient` model classifies each request locally, filters the current
catalog against capabilities and a fresh benchmark snapshot, removes dominated
candidates, and chooses the lowest expected-cost model above the configured
quality floor. Paid and subscription offerings are considered only when their
provider has an explicit `billing_mode = "paid"` or `"subscription"`; providers
default to free-only. Configure `cost_microusd` quota windows to impose
transactional spend caps. Reservations expire after abandoned requests and are
reconciled against provider-reported token usage when available. If no benchmarked authorized candidate remains, the
route falls back once through `auto-free` and then `local`.

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
the state location with `MODEL_GATEWAY_STATE_PATH`.

Benchmark refresh is also explicit; serving never requests a benchmark site.
Store `ARTIFICIAL_ANALYSIS_API_KEY` through `model-gateway credentials set
ARTIFICIAL_ANALYSIS_API_KEY`, then run `model-gateway benchmarks refresh` to
load the authenticated Artificial Analysis API with required attribution.
Inspect snapshots with `model-gateway benchmarks status`. Arena automation is
disabled by its terms, and LLM Stats or DeepSWE/DataCurve data must be supplied
as a documented/licensed export rather than scraped. Import such normalized
exports with `model-gateway benchmarks import --file <path>`:

```json
{
  "source": "licensed-export",
  "attribution": "Required source attribution",
  "models": [{
    "id": "canonical-model-id",
    "creator": "Creator",
    "general_quality": 80.0,
    "coding_quality": 85.0,
    "agentic_quality": 75.0,
    "reasoning_quality": 82.0,
    "input_price_per_million": 1.0,
    "output_price_per_million": 3.0,
    "latency_seconds": 0.5,
    "output_tokens_per_task": 1024,
    "reasoning_effort": "high",
    "as_of": "2026-07-22",
    "harness": "source-harness-v1",
    "confidence": 0.95
  }]
}
```

Provider offering IDs map to canonical benchmark IDs only through exact IDs or
explicit `model_mappings`; similar names are never merged heuristically.
Successful `auto-efficient` responses include fixed classifier, complexity,
quality-floor, quality, and expected-cost headers without prompt content.

## Supported Profiles

The setup wizard uses one declarative registry. Provider recommendations follow
`.env.example`: CORE profiles are highly encouraged, SECONDARY profiles are
useful additions, and OPTIONAL profiles are neither fully tested nor
recommended. The registry currently covers all names in `.env.local`: Google
Gemini, Kilo Code, Ollama Cloud, OpenCode, OpenRouter, Cerebras, DeepSeek,
Fireworks, Mistral, Nous Portal, NVIDIA NIM, Cline, Gitlawb OpenGateway, Groq,
Novita, OrcaRouter, and SiliconFlow. The cloud profiles use OpenAI Chat
Completions with bearer secrets. They are contract-tested against deterministic
local fixtures; no provider credential is required for CI.

Secondary profiles add Ollama Cloud and the Cline API using their documented
OpenAI-compatible endpoints. `GITLAWB_API_GIT` remains intentionally unmapped:
it does not identify a documented LLM provider endpoint.

`gateway.core.example.toml`, `gateway.secondary.example.toml`, and
`gateway.optional.example.toml` mirror the three `.env.example` sections. Run
`./scripts/core-provider-check.sh` for an explicit one-time connection check
with `.env.local`. It checks all three configs, sends only documented
model-catalog or key-status GET requests, skips providers without a documented
zero-credit endpoint, never sends a completion, reports every provider before
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
