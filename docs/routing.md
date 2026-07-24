# Routing

## Route Resolution Order

The gateway maps each request model to a route:

- Built-in routes (`local`, `auto-free`, `auto-efficient`, `auto-frontier`)
- User-defined aliases from `[models]` in config

Disable individual routes with `server.auto_free_enabled`, `server.auto_efficient_enabled`, or `server.auto_frontier_enabled`. Disabled routes are omitted from `/v1/models` and reject requests with `route_disabled`.

All routes filter candidates by:
- Provider availability (API key resolved)
- Model allowlist/denylist
- Capability requirements (tools, vision, structured output, context length)

## `local`

Relays the only model reported by an OpenAI-compatible endpoint. Default endpoint: `http://127.0.0.1:8000/v1`.

- Use `MODEL_GATEWAY_LOCAL_MODEL` when the endpoint reports multiple models
- Use `MODEL_GATEWAY_LOCAL_BASE_URL` for a different endpoint
- Results are cached for `local_model_cache_seconds` (configurable)
- Terminal assistant text is decorated with model, reasoning-effort, and provider line (not included in upstream token usage)

## `auto-free`

Selects the best free model for the request. Filter + rank pipeline:

1. **`free_candidates`** — models from `catalog_models WHERE is_free = 1`
2. **Quality bar** — `free_models_quality.passes()` filters by:
   - Minimum quality index per task (general: 25, coding: 35, agentic: 15)
   - Maximum age (18 months)
   - Maximum price ($5/M input, $15/M output)
   - Minimum context length (8,192)
   - Minimum model size (27B)
3. **Complexity floor** — `free_quality_floor_{simple,medium,complex}` (defaults 30/45/60)
   - Simple tasks floor at 30 — fast adequate models pass
   - Complex tasks floor at 60 — only the best models qualify
4. **Pareto ranking** — `pareto_rank(quality, cost=0, latency)`
   - For free models (cost=0), degenerates to quality vs latency
   - Dominated candidates (worse on all axes) are pruned
5. **Sort** — pinned first → cost → latency → quality
6. **Fallback** — unbenchmarked models → local

Free-tier eligibility rules are provider-specific. See [providers.md](providers.md).

### Session Pinning

When a request succeeds, the session is pinned to `(provider, model)` for 30 minutes. Pinned models sort first on subsequent requests. Pins survive transient rate limits (429) but are destroyed on auth failures (401/403). Session identity is derived from `session_id` body field, `x-session-id` header, or the first 2 system/user messages.

## `auto-efficient`

Pareto-ranks all benchmarked models by quality, cost, and latency. Pipeline:

1. **`all_candidates`** — all models from `catalog_models`
2. **Availability filter** — remove unavailable providers and free-only providers when billing requires paid
3. **Capability filter** — context length, tools, vision, structured output
4. **Benchmark filter** — candidates must have a matching benchmark entry
5. **Complexity floor** — `quality_floor_{simple,medium,complex}` (defaults 40/60/75)
6. **Pareto ranking** — `pareto_rank(quality, cost_microusd, latency)`
   - Removes dominated candidates (worse on all three axes)
   - Sorts non-dominated by cost → latency → quality
7. **Session pin** — pinned models sort first (same mechanism as auto-free)
8. **Fallback** — `auto-free` → `local`

Expected cost is computed from the offering's input/output prices and estimated request tokens. Cost-based quota windows (`cost_microusd`) impose spend caps.

### Benchmarks Required

Benchmarks are required for eligibility. Models without benchmarks are excluded. Get a free [Artificial Analysis API key](https://artificialanalysis.ai/), then:

```bash
model-gateway credentials set ARTIFICIAL_ANALYSIS_API_KEY
```

The server auto-fetches on startup if no fresh data exists. Manual: `model-gateway benchmarks refresh`.

## `auto-frontier`

Same pipeline as `auto-efficient` with additional constraints:

- Only OpenAI or Anthropic canonical creators (identified by benchmark entries)
- Requires explicit paid or subscription billing authorization
- Excludes preview/beta/experimental model IDs unless `allow_preview_models = true`
- Independent quality floors: `frontier_quality_floor_{simple,medium,complex}` (defaults 50/70/85)
- **Never falls back** — returns a fixed frontier error when no candidate is safe and available

## `/v1/free-models`

Listing endpoint for discovery. Returns all free models filtered by the quality bar, sorted by quality score (descending), then limit status, then provider/model name. Does NOT apply complexity floors or Pareto ranking — those are routing decisions. Supports `?provider=`, `?task=`, and `?limit=` query parameters.

## `/v1/rankings`

Read-only view of fresh benchmark data. Sorted by quality score (descending). Supports `?task=` and `?limit=` query parameters. Never performs live benchmark requests.
