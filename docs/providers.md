# Providers

## Provider Groups

Provider profiles are organized into three tiers matching `.env.example`:

| Tier | Providers |
|---|---|
| **Core** (recommended) | Anthropic, Google Gemini, Kilo Code, Ollama Cloud, OpenCode Zen, OpenRouter |
| **Secondary** (useful) | Groq, Mistral, Nous Portal, Novita, NVIDIA NIM, SiliconFlow |
| **Optional Paid** (subscriptions/credits) | DeepSeek, Fireworks, OpenAI API, OpenCode Go, OrcaRouter, Z.AI |
| **Local OAuth Sidecar** | CLIProxyAPI (Claude Code and ChatGPT/Codex subscriptions) |

OpenCode Go and Zen share `OPENCODE_API_KEY` but use separate model catalogs and billing modes (subscription vs free). OpenCode Go enforces cost-based quota windows for spend control.

## Free-Tier Eligibility

Free access is classified independently from list price:

- `zero_price`: an explicitly zero-cost model such as an OpenCode Zen or Kilo free model
- `quota_limited_free_tier`: a normally priced model temporarily available through a provider's free account quota
- `subscription_included`: a normally priced model included by a profile with known subscription semantics (currently CLIProxyAPI and OpenCode Go)
- `paid`: a model that may incur charges under the configured account

`auto-free` treats a model as eligible when:

- **Direct catalog providers** (Google Gemini, Groq, Mistral, NVIDIA NIM, Ollama Cloud, SiliconFlow): zero-price models are `zero_price`; normally priced models are `quota_limited_free_tier` only while `billing_mode = "free"`
- **Kilo Code**: models whose IDs contain `free`, overridable with `MODEL_GATEWAY_KILOCODE_FREE_MODELS`
- **OpenCode Zen**: explicitly free models including `big-pickle` and IDs containing `free`
- **OpenCode Go**: subscription/paid-only — no models are treated as free-tier eligible
- **Nous Portal, OrcaRouter**: models whose catalog metadata or IDs explicitly indicate free access

Providers without an available API key are ignored. Optional paid providers require explicit `billing_mode = "paid"` overrides.

Quota-tier models retain their provider or benchmark list prices as reference prices. Their effective request price is zero only while free access remains available. A recorded account-limit snapshot with zero remaining quota removes them from `auto-free` until the account status is explicitly refreshed. The gateway never switches a quota-tier model to a paid route automatically; paid access requires explicit `billing_mode = "paid"` or `"subscription"` configuration. Providers without an account-status API rely on that billing-mode assertion plus configured/static quota rules, so `overage: "gateway_blocked"` describes gateway routing behavior rather than a provider-side billing guarantee.

Known included-subscription profiles use `source: "subscription"` for effective zero pricing and retain list prices under `reference_price_per_million`. They participate in paid auto routes using reference cost as a scarcity/efficiency signal, but are never included in `auto-free`. Setting an arbitrary custom or API-key provider to `billing_mode = "subscription"` does not imply included usage and does not zero its effective price.

## CLIProxyAPI OAuth Sidecar

CLIProxyAPI is integrated as a separate loopback process, not embedded into the Rust gateway and not used to replace existing provider endpoints. This keeps its OAuth tokens, account rotation, provider translations, retries, and rapid release cadence behind one optional HTTP boundary.

`model-gateway cli-proxy setup` installs exact version `v7.2.103` with platform-specific SHA-256 verification on macOS and Linux. It generates a private config under `~/.config/model-gateway/cli-proxy`, stores the frontend key through the gateway secret resolver, and adds `[providers.cli-proxy]` with `billing_mode = "subscription"`.

Use `./scripts/start-cli-proxy.sh` to run the sidecar in the background. Logs are written to `~/.config/model-gateway/cli-proxy/server.log`; pass `--follow` or `-f` to keep it in the foreground. Use `./scripts/restart-cli-proxy.sh` to restart the managed process. The lower-level `model-gateway cli-proxy serve` command remains a foreground runner.

CLIProxyAPI owns account-level round-robin selection, cooldown, token refresh, and pre-output credential failover. The gateway treats it as one provider and may fall back to a different gateway provider only after CLIProxyAPI returns a terminal response.

Security and policy boundaries:

- OAuth access and refresh tokens are stored as plaintext JSON in the private auth directory; use encrypted local storage and preserve `0700`/`0600` permissions.
- The generated serving config disables remote management, plugins, control-panel downloads, debug logs, and usage statistics.
- The sidecar is loopback-only and protected by a generated bearer key.
- Claude request cloaking is disabled so the gateway's system prompts are not silently replaced.
- Subscription automation and account pooling may be restricted by provider terms. Use only accounts you control and review current Claude/OpenAI policies before enabling it.
- Pin upgrades deliberately. CLIProxyAPI changes frequently in response to upstream protocol changes; the gateway never follows `latest` automatically.
- Use `MODEL_GATEWAY_CLI_PROXY_*` path overrides for a manually managed binary/config/auth directory.

Replacing all CLI/API endpoints with CLIProxyAPI is intentionally unsupported. Direct API-key adapters preserve authoritative pricing, quota, identity, and error semantics; Ollama and LM Studio avoid an unnecessary proxy hop; hosted routers already provide their own account and billing layers.

## Provider Profiles

All profiles use OpenAI Chat Completions with bearer secrets. They are contract-tested against deterministic local fixtures — no provider credential is required for CI.

The setup wizard uses one declarative registry at `src/providers.rs` (`BuiltinProvider` enum). Adding a provider requires a new variant there plus an entry in the example configs.

## Example Configs

| File | Contents |
|---|---|
| `gateway.core.example.toml` | Core providers with recommended defaults |
| `gateway.secondary.example.toml` | Secondary providers |
| `gateway.optional.example.toml` | Optional paid providers |
| `gateway.example.toml` | Full reference with all providers |

Run `./scripts/core-provider-check.sh` for a one-time connection check with `.env.local`. This sends only documented model-catalog or key-status GET requests and reports every provider before returning a failure summary.

## Compatibility

See [provider-compatibility.md](provider-compatibility.md) for the detailed compatibility matrix showing wire families, authentication, and integration test status for each provider.
