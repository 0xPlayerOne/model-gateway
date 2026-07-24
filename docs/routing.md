# Routing

## Cache-Aware Design

**Prompt caching is provider-scoped.** Each provider (OpenAI, Anthropic, etc.) caches prompt prefixes per-model on their servers. Switching providers or models mid-session breaks the cache — the new provider has never seen your prompt prefix before.

This gateway is designed to **eliminate cache misses**:

1. **One model per mode** — each mode (auto-free, auto-efficient, auto-balanced, auto-frontier) picks ONE model from the Pareto frontier. No task-specific routing, no complexity tiers.
2. **Session pinning** — the first successful request pins the session to `(provider, model)`. All subsequent requests use the same model. Pin survives transient rate limits (429). Only permanent auth failures (401/403) destroy the pin.
3. **Composite quality score** — `0.5*intelligence + 0.3*coding + 0.2*agentic` gives a well-rounded model for any task. No re-routing based on task type.
4. **Pareto frontier handles reasoning effort** — for models with multiple variants (e.g., GPT 5.6 Luna/Sol/Sol Max), the Pareto frontier picks the most efficient one. Sol Max is dominated by Sol (higher cost, marginal quality gain).

**The result**: pick a mode, stay on the same model for the entire session. Cache is entirely in your hands — as long as you don't switch modes mid-session, you won't miss.

## Route Resolution Order

The gateway maps each request model to a route:

- Built-in routes (`local`, `auto-free`, `auto-efficient`, `auto-balanced`, `auto-frontier`)
- User-defined aliases from `[models]` in config

Disable individual routes with `server.auto_free_enabled`, `server.auto_efficient_enabled`, `server.auto_balanced_enabled`, or `server.auto_frontier_enabled`. Disabled routes are omitted from `/v1/models` and reject requests with `route_disabled`.

All routes filter candidates by:
- Provider availability (API key resolved)
- Model allowlist/denylist
- Capability requirements (tools, vision, structured output, context length)
- Global model denylist

## Quality Scoring

All paid routes use **composite quality** instead of task-specific scores:

```
composite_quality = 0.5 * intelligence + 0.3 * coding_quality + 0.2 * agentic_quality
```

Fallbacks: if `coding_quality` or `agentic_quality` is None, the weight redistributes to `intelligence`. This naturally filters out models with super low coding or agentic scores.

The Pareto frontier operates on ALL benchmark entries (including different reasoning_effort levels). It naturally picks the most efficient variant — e.g., GPT 5.6 Sol (medium effort) over Sol Max (high effort) because Sol has better quality/cost ratio.

## `local`

Relays the only model reported by an OpenAI-compatible endpoint. Default endpoint: `http://127.0.0.1:8000/v1`.

- Use `MODEL_GATEWAY_LOCAL_MODEL` when the endpoint reports multiple models
- Use `MODEL_GATEWAY_LOCAL_BASE_URL` for a different endpoint
- Results are cached for `local_model_cache_seconds` (configurable)
- Terminal assistant text is decorated with model, reasoning-effort, and provider line (not included in upstream token usage)

## `auto-free`

Selects the best free model. Filter + rank pipeline:

1. **`free_candidates`** — models from `catalog_models WHERE is_free = 1`
2. **Quality bar** — `free_models_quality.passes()` filters by minimum quality per task, max age, max price, min context length, min model size
3. **Composite quality** — `composite_quality()` for Pareto ranking
4. **Pareto ranking** — `pareto_rank(composite_quality, cost=0, latency)`
   - For free models (cost=0), degenerates to quality vs latency
   - Faster models with sufficient quality beat slower overqualified models
5. **Sort** — pinned first → cost → latency → quality
6. **Fallback** — unbenchmarked models → local

Free-tier eligibility rules are provider-specific. See [providers.md](providers.md).

## `auto-efficient`

Best bang-for-buck. Quality floor: **40**. Pipeline:

1. **`all_candidates`** — all models from `catalog_models`
2. **Availability filter** — remove unavailable providers and free-only providers when billing requires paid
3. **Capability filter** — context length, tools, vision, structured output
4. **Composite quality floor** — `efficient_quality_floor` (default 40.0)
5. **Pareto ranking** — `pareto_rank(composite_quality, cost_microusd, latency)`
   - Removes dominated candidates (worse on all three axes)
   - Sorts non-dominated by cost → latency → quality
6. **Session pin** — pinned models sort first
7. **Fallback** — `auto-free` → `local`

Expected cost is computed from the offering's input/output prices and estimated request tokens. Cost-based quota windows impose spend caps.

## `auto-balanced`

Mid-range quality. Quality floor: **60**. Same pipeline as auto-efficient with a higher quality floor. Targets models that are great quality but not the most expensive — DeepSeek V4 Pro, MiMo v2.5 Pro, GPT 5.6 Luna class.

- Quality floor: `balanced_quality_floor` (default 60.0)
- Falls back to `auto-free` → `local`
- Disable with `auto_balanced_enabled = false`

## `auto-frontier`

Top tier. Quality floor: **80**. Same pipeline as auto-efficient with additional constraints:

- Only OpenAI or Anthropic canonical creators (identified by benchmark entries)
- Requires explicit paid or subscription billing authorization
- Excludes preview/beta/experimental model IDs unless `allow_preview_models = true`
- Quality floor: `frontier_quality_floor_single` (default 80.0)
- **Never falls back** — returns a fixed frontier error when no candidate is safe and available

## Session Pinning

When a request succeeds, the session is pinned to `(provider, model)` for 30 minutes. Pinned models sort first on subsequent requests.

**Pin lifecycle**:
- Set on first successful request
- Refreshed on each subsequent success (same provider+model)
- NOT invalidated on 429/rate limits (cooldown handles temporary routing)
- NOT invalidated on quota exhaustion (temporary)
- Invalidated on 401/403 auth failures (permanent)
- Session identity: `session_id` body field → `x-session-id` header → first 2 system/user messages

**Why pins survive rate limits**: The cooldown mechanism already routes around rate-limited providers. Destroying the pin on top of that would waste prompt cache. When the cooldown expires, the session returns to the original provider where the cache is still warm.

## Listing Endpoints

### `/v1/free-models`

Returns all free models filtered by the quality bar. Supports `?provider=`, `?task=`, `?limit=` query parameters. Task filters are for discovery/rankings display — routing uses composite quality.

### `/v1/paid-models`

Returns all non-free models from paid/subscription providers. Supports `?task=`, `?limit=`, `?provider=` query parameters.

### `/v1/auto-models`

Shows exactly which model each mode would select, with primary + fallback per mode. Supports `?task=` and `?route=` query parameters.

### `/v1/rankings`

Read-only view of fresh benchmark data. Sorted by quality score (descending). Supports `?task=` and `?limit=` query parameters. Never performs live benchmark requests. See [benchmarks.md](benchmarks.md) for the full response format, setup, and attribution.
