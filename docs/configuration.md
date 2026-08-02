# Configuration

Config is loaded from `~/.config/model-gateway/config.toml` (override with `MODEL_GATEWAY_CONFIG`). `MODEL_GATEWAY_HOME` changes the configuration and state home. If the file does not exist, the gateway starts from safe defaults using environment variables.

Environment overrides are applied on every load and take precedence over TOML.

## Server Settings

| Env Variable | Default | Description |
|---|---|---|
| `MODEL_GATEWAY_BIND` | `127.0.0.1:8008` | Listen address |
| `MODEL_GATEWAY_EXPOSURE` | `loopback` | `loopback` or `local_container`; non-loopback binds are accepted only in container mode |
| `MODEL_GATEWAY_LOCAL_BASE_URL` | `http://localhost:8000/v1` | Local model endpoint |
| `MODEL_GATEWAY_LOCAL_MODEL` | — | Explicit local model (required when endpoint reports multiple) |
| `MODEL_GATEWAY_LOCAL_MODEL_CACHE_SECONDS` | `60` | Local model discovery cache TTL |
| `MODEL_GATEWAY_MAX_BODY_BYTES` | `33554432` | Maximum request body size (32MB) |
| `MODEL_GATEWAY_MAX_IN_FLIGHT` | `64` | Concurrent request limit |
| `MODEL_GATEWAY_ADMISSION_TIMEOUT_MS` | `250` | Admission wait timeout |
| `MODEL_GATEWAY_SHUTDOWN_GRACE_SECONDS` | `30` | Graceful shutdown timeout |
| `MODEL_GATEWAY_SECRET_STORE` | `file` | `file`, `keychain`, or `environment` |

The application default is the protected-file store so direct and unattended
startup never prompts for an OS keychain. The modes are exclusive: `file`
reads `MODEL_GATEWAY_SECRET_DIR`, `keychain` reads only the OS keychain, and
`environment` reads only exported variables. Set
`MODEL_GATEWAY_SECRET_STORE=keychain` explicitly when using the OS keychain.
The launcher scripts print the effective store before starting. See
[getting-started.md](getting-started.md).

| `MODEL_GATEWAY_SECRET_DIR` | `~/.config/model-gateway/secrets` | Directory for `file` store values (`0700`, files `0600`) |
| `MODEL_GATEWAY_STATE_PATH` | `~/.config/model-gateway/routing.sqlite3` | SQLite database path |
| `MODEL_GATEWAY_LOG_FORMAT` | `text` | `text` or `json` |
| `MODEL_GATEWAY_CONFIG` | `~/.config/model-gateway/config.toml` | Configuration file path |
| `MODEL_GATEWAY_CATALOG_MAX_AGE_SECONDS` | `86400` | Catalog freshness window |
| `MODEL_GATEWAY_BENCHMARK_MAX_AGE_SECONDS` | `604800` | Benchmark freshness window (7 days) |
| `MODEL_GATEWAY_PRICING_MAX_AGE_SECONDS` | `604800` | Pricing freshness window (7 days) |
| `MODEL_GATEWAY_AUTO_FRONTIER_ENABLED` | `true` | Enable/disable auto-frontier route |
| `MODEL_GATEWAY_AUTO_FREE_ENABLED` | `true` | Enable/disable auto-free route |
| `MODEL_GATEWAY_AUTO_EFFICIENT_ENABLED` | `true` | Enable/disable auto-efficient route |
| `MODEL_GATEWAY_AUTO_BALANCED_ENABLED` | `true` | Enable/disable auto-balanced route |
| `MODEL_GATEWAY_MODEL_DENYLIST` | — | Comma-separated model IDs to exclude globally |

### CLIProxyAPI Sidecar

`model-gateway cli-proxy setup` creates a `cli-proxy` provider automatically. Optional path overrides:

| Env Variable | Default | Description |
|---|---|---|
| `MODEL_GATEWAY_CLI_PROXY_HOME` | `~/.config/model-gateway/cli-proxy` | Sidecar installation and state root |
| `MODEL_GATEWAY_CLI_PROXY_BINARY` | versioned binary under the sidecar root | Manually managed executable |
| `MODEL_GATEWAY_CLI_PROXY_CONFIG` | `<home>/config.yaml` | Sidecar YAML configuration |
| `MODEL_GATEWAY_CLI_PROXY_AUTH_DIR` | `<home>/auth` | OAuth credential directory |
| `MODEL_GATEWAY_CLI_PROXY_PORT` | `8317` during setup | Optional listener port; set it before `cli-proxy setup`, then keep it unset or equal to the generated config at launch |
| `CLI_PROXY_API_KEY` | generated secret-store value | Frontend bearer key for manual/environment-only setup |

`model-gateway cli-proxy setup` reports which store received the generated frontend key (`Stored the CLIProxyAPI frontend key in the <source> secret store`). In a non-interactive environment set `MODEL_GATEWAY_SECRET_STORE=file` (and optionally `MODEL_GATEWAY_SECRET_DIR`) before running setup.

Use `MODEL_GATEWAY_CLI_PROXY_MODEL_ALLOWLIST` to expose only reviewed sidecar models. The provider defaults to `billing_mode = "subscription"`; changing it to `paid` authorizes per-token billing semantics and should not be done for OAuth subscription accounts.

## Quality Floors

Routing uses a single composite quality floor per mode (not per-task or per-complexity):

```
composite_quality = 0.80 * intelligence + 0.10 * coding_quality + 0.10 * agentic_quality
```

| Env Variable | Default | Route |
|---|---|---|
| `MODEL_GATEWAY_EFFICIENT_QUALITY_FLOOR` | `35.0` | auto-efficient |
| `MODEL_GATEWAY_BALANCED_QUALITY_FLOOR` | `42.0` | auto-balanced |
| `MODEL_GATEWAY_FRONTIER_QUALITY_FLOOR` | `52.0` | auto-frontier |

Each floor must be 0–100. Higher floors select higher-quality models. The Pareto frontier picks the most efficient model above the floor (best quality/cost/latency tradeoff).

## Free Models Quality Bar

Filters low-quality, stale, or expensive models from the `access=free` view of `/v1/catalog/models` and from the ranked portion of auto-free routing. Uses composite quality (not per-task). Models without benchmark data are not rejected solely for being unbenchmarked.

| Env Variable | Default | Description |
|---|---|---|
| `MODEL_GATEWAY_FREE_QUALITY_MIN_COMPOSITE` | `30.0` | Minimum composite quality score (0–100) |
| `MODEL_GATEWAY_FREE_QUALITY_MIN_CONTEXT` | `8192` | Minimum context length |
| `MODEL_GATEWAY_FREE_QUALITY_MIN_MODEL_SIZE` | `27` | Minimum parameter count in billions |
| `MODEL_GATEWAY_FREE_QUALITY_MAX_AGE_MONTHS` | `18` | Maximum model age |
| `MODEL_GATEWAY_FREE_QUALITY_MAX_INPUT_PRICE` | `2.0` | Maximum input price per million tokens |
| `MODEL_GATEWAY_FREE_QUALITY_MAX_OUTPUT_PRICE` | `10.0` | Maximum output price per million tokens |
| `MODEL_GATEWAY_FREE_QUALITY_MAX_REGRET` | `8.0` | Maximum quality gap from the best available free candidate |

Set any value to 0 to disable that filter. Models without benchmark data always pass quality/age filters (new models are not penalized).

## Billing Mode

Providers default to **free billing**, except the CLIProxyAPI profile, which defaults to **subscription** because its frontend key represents an already-configured local OAuth sidecar. To enable other paid/subscription models, use:

- **Global**: `MODEL_GATEWAY_PAID_BILLING_MODE=openai-api,deepseek,opencode-go` (comma-separated provider names)
- **Per-provider**: `MODEL_GATEWAY_OPENAI_API_BILLING_MODE=paid` (takes precedence)

Provider names in the global var must match config keys (lowercase with hyphens). Unknown names produce a config error.

## Pricing Resolution

Run `model-gateway pricing refresh` explicitly to update public pricing from
models.dev. A complete provider catalog price, including a temporary zero or
discounted price, is preserved before creator or aggregate fallback data.
Use `model-gateway pricing import` for exact provider/model overrides and
`model-gateway pricing explain <provider> <model>` to inspect the selected
source. Auto routes exclude targets without a complete effective price.

## Provider Overrides

Use the normalized provider name as prefix, e.g., `MODEL_GATEWAY_OPENROUTER_BILLING_MODE=paid`.

| Override | Example | Description |
|---|---|---|
| `BILLING_MODE` | `paid` | Override billing mode (`free`, `paid`, `subscription`) |
| `BASE_URL` | `https://custom.example.com/v1` | Override the provider endpoint |
| `API_KEY_SECRET` | `my-key-name` | Override the secret reference |
| `ACCOUNT_SCOPE` | `my-account` | Scope for quota tracking |
| `FREE_MODELS` | `model-a,model-b` | Explicit free model overrides |
| `MODEL_ALLOWLIST` | `gpt-4,claude-3` | Only these models are routable |
| `MODEL_DENYLIST` | `gpt-3.5` | These models are excluded |
| `ALLOW_PREVIEW_MODELS` | `true` | Allow preview/beta/experimental models |
| `ALLOW_MODEL_PASSTHROUGH` | `true` | Allow unlisted models |
| `ALLOW_INSECURE_HTTP` | `true` | Allow HTTP connections |
| `MAX_IN_FLIGHT` | `50` | Per-provider concurrency limit |
| `RESPONSE_HEADER_TIMEOUT_SECONDS` | `30` | Header timeout |
| `STREAM_IDLE_TIMEOUT_SECONDS` | `300` | Stream idle timeout |
| `EXTRA_HEADERS` | `X-Custom:value` | Additional request headers |
| `MODEL_MAPPINGS` | `provider/model:canonical` | Model ID mappings |
| `pricing_profile` (TOML) | `models.dev provider key` | Provider-scoped public pricing namespace |
| `QUOTAS` | `cost_microusd:1000000:86400` | Quota windows (semicolon-separated) |

Provider names used in overrides: `cli-proxy`, `openrouter`, `google-gemini`, `groq`, `mistral`, `kilocode`, `opencode-zen`, `opencode-go`, `nous-portal`, `nvidia-nim`, `ollama-cloud`, `orcarouter`, `silicon-flow`, `deepseek`, `fireworks`, `openai-api`, `zai`.

## Quota Format

```
kind:limit:window_seconds[:boundary]
```

- `kind` — `requests`, `tokens`, `cost_microusd`, `concurrency`
- `limit` — maximum per window
- `window_seconds` — rolling window duration
- `boundary` — optional calendar alignment (`utc-day`, `utc-hour`)

Multiple quotas separated by semicolons: `requests:100:3600;cost_microusd:500000:86400:utc-day`
