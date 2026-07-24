# Configuration

Config is loaded from `~/.config/model-gateway/config.toml` (override with `MODEL_GATEWAY_CONFIG`). If the file doesn't exist, the gateway starts from safe defaults using only environment variables.

Environment overrides are applied on every load and take precedence over TOML.

## Server Settings

| Env Variable | Default | Description |
|---|---|---|
| `MODEL_GATEWAY_BIND` | `127.0.0.1:8008` | Listen address |
| `MODEL_GATEWAY_EXPOSURE` | `loopback` | `loopback`, `private`, or `docker-local` |
| `MODEL_GATEWAY_LOCAL_BASE_URL` | `http://localhost:8000/v1` | Local model endpoint |
| `MODEL_GATEWAY_LOCAL_MODEL` | — | Explicit local model (required when endpoint reports multiple) |
| `MODEL_GATEWAY_LOCAL_MODEL_CACHE_SECONDS` | `300` | Local model discovery cache TTL |
| `MODEL_GATEWAY_MAX_BODY_BYTES` | `16777216` | Maximum request body size |
| `MODEL_GATEWAY_MAX_IN_FLIGHT` | `1024` | Concurrent request limit |
| `MODEL_GATEWAY_ADMISSION_TIMEOUT_MS` | `10000` | Admission wait timeout |
| `MODEL_GATEWAY_SHUTDOWN_GRACE_SECONDS` | `10` | Graceful shutdown timeout |
| `MODEL_GATEWAY_MAX_BODY_BYTES` | `16777216` | Max request body size |
| `MODEL_GATEWAY_SECRET_STORE` | `environment` | `environment`, `file`, or `keychain` |
| `MODEL_GATEWAY_STATE_PATH` | `~/.config/model-gateway/routing.sqlite3` | SQLite database path |
| `MODEL_GATEWAY_LOG_FORMAT` | `text` | `text` or `json` |
| `MODEL_GATEWAY_CATALOG_MAX_AGE_SECONDS` | `86400` | Catalog freshness window |
| `MODEL_GATEWAY_BENCHMARK_MAX_AGE_SECONDS` | `86400` | Benchmark freshness window |
| `MODEL_GATEWAY_AUTO_FRONTIER_ENABLED` | `true` | Enable/disable auto-frontier route |
| `MODEL_GATEWAY_AUTO_FREE_ENABLED` | `true` | Enable/disable auto-free route |
| `MODEL_GATEWAY_AUTO_EFFICIENT_ENABLED` | `true` | Enable/disable auto-efficient route |

## Quality Floors

| Env Variable | Default | Route |
|---|---|---|
| `MODEL_GATEWAY_QUALITY_FLOOR_SIMPLE` | `40.0` | auto-efficient |
| `MODEL_GATEWAY_QUALITY_FLOOR_MEDIUM` | `60.0` | auto-efficient |
| `MODEL_GATEWAY_QUALITY_FLOOR_COMPLEX` | `75.0` | auto-efficient |
| `MODEL_GATEWAY_FRONTIER_QUALITY_FLOOR_SIMPLE` | `50.0` | auto-frontier |
| `MODEL_GATEWAY_FRONTIER_QUALITY_FLOOR_MEDIUM` | `70.0` | auto-frontier |
| `MODEL_GATEWAY_FRONTIER_QUALITY_FLOOR_COMPLEX` | `85.0` | auto-frontier |
| `MODEL_GATEWAY_FREE_QUALITY_FLOOR_SIMPLE` | `30.0` | auto-free |
| `MODEL_GATEWAY_FREE_QUALITY_FLOOR_MEDIUM` | `45.0` | auto-free |
| `MODEL_GATEWAY_FREE_QUALITY_FLOOR_COMPLEX` | `60.0` | auto-free |

Quality floors are ordered: simple ≤ medium ≤ complex. Each must be 0–100.

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
| `QUOTAS` | `cost_microusd:1000000:86400` | Quota windows (semicolon-separated) |

Provider names used in overrides: `openrouter`, `google-gemini`, `groq`, `mistral`, `kilocode`, `opencode-zen`, `opencode-go`, `nous-portal`, `novita`, `nvidia-nim`, `ollama-cloud`, `orca-router`, `siliconflow`, `deepseek`, `fireworks`, `openai-api`, `z-ai`.

## Quota Format

```
kind:limit:window_seconds[:boundary]
```

- `kind` — `cost_microusd`, `requests`, `tokens_input`, `tokens_output`, `tokens_total`
- `limit` — maximum per window
- `window_seconds` — rolling window duration
- `boundary` — optional calendar alignment (`utc-day`, `utc-hour`)

Multiple quotas separated by semicolons: `requests:100:3600;cost_microusd:500000:86400:utc-day`
