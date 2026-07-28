# Providers

## Provider Groups

Provider profiles are organized into three tiers matching `.env.example`:

| Tier | Providers |
|---|---|
| **Core** (recommended) | Anthropic, Google Gemini, Kilo Code, Ollama Cloud, OpenCode Zen, OpenRouter |
| **Secondary** (useful) | Groq, Mistral, Nous Portal, Novita, NVIDIA NIM, SiliconFlow |
| **Optional Paid** (subscriptions/credits) | DeepSeek, Fireworks, OpenAI API, OpenCode Go, OrcaRouter, Z.AI |

OpenCode Go and Zen share `OPENCODE_API_KEY` but use separate model catalogs and billing modes (subscription vs free). OpenCode Go enforces cost-based quota windows for spend control.

## Free-Tier Eligibility

Free access is classified independently from list price:

- `zero_price`: an explicitly zero-cost model such as an OpenCode Zen or Kilo free model
- `quota_limited_free_tier`: a normally priced model temporarily available through a provider's free account quota
- `paid`: a model that may incur charges under the configured account

`auto-free` treats a model as eligible when:

- **Direct catalog providers** (Google Gemini, Groq, Mistral, NVIDIA NIM, Ollama Cloud, SiliconFlow): zero-price models are `zero_price`; normally priced models are `quota_limited_free_tier` only while `billing_mode = "free"`
- **Kilo Code**: models whose IDs contain `free`, overridable with `MODEL_GATEWAY_KILOCODE_FREE_MODELS`
- **OpenCode Zen**: explicitly free models including `big-pickle` and IDs containing `free`
- **OpenCode Go**: subscription/paid-only — no models are treated as free-tier eligible
- **Nous Portal, OrcaRouter**: models whose catalog metadata or IDs explicitly indicate free access

Providers without an available API key are ignored. Optional paid providers require explicit `billing_mode = "paid"` overrides.

Quota-tier models retain their provider or benchmark list prices as reference prices. Their effective request price is zero only while free access remains available. A recorded account-limit snapshot with zero remaining quota removes them from `auto-free` until the account status is explicitly refreshed. The gateway never switches a quota-tier model to a paid route automatically; paid access requires explicit `billing_mode = "paid"` or `"subscription"` configuration. Providers without an account-status API rely on that billing-mode assertion plus configured/static quota rules, so `overage: "gateway_blocked"` describes gateway routing behavior rather than a provider-side billing guarantee.

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
