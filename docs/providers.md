# Providers

## Provider Groups

Provider profiles are organized into three tiers matching `.env.example`:

| Tier | Providers |
|---|---|
| **Core** (recommended) | Google Gemini, Kilo Code, Ollama Cloud, OpenCode Zen, OpenRouter |
| **Secondary** (useful) | Groq, Mistral, Nous Portal, Novita, NVIDIA NIM, SiliconFlow |
| **Optional Paid** (subscriptions/credits) | DeepSeek, Fireworks, OpenAI API, OpenCode Go, OrcaRouter, Z.AI |

OpenCode Go and Zen share `OPENCODE_API_KEY` but use separate model catalogs and billing modes (subscription vs free). OpenCode Go enforces cost-based quota windows for spend control.

## Free-Tier Eligibility

`auto-free` treats a model as free when:

- **Direct catalog providers** (Google Gemini, Groq, Mistral, NVIDIA NIM, Ollama Cloud, SiliconFlow): any model with zero-price catalog metadata or a provider-specific free-tier rule
- **Kilo Code**: models whose IDs contain `free`, overridable with `MODEL_GATEWAY_KILOCODE_FREE_MODELS`
- **OpenCode Zen**: explicitly free models including `big-pickle` and IDs containing `free`
- **OpenCode Go**: subscription/paid-only — no models are treated as free-tier eligible
- **Nous Portal, OrcaRouter**: models whose catalog metadata or IDs explicitly indicate free access

Providers without an available API key are ignored. Optional paid providers require explicit `billing_mode = "paid"` overrides.

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
