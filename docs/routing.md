# Routing

## Cache-Aware Design

**Prompt caching is provider-scoped.** Each provider (OpenAI, Anthropic, etc.) caches prompt prefixes per-model on their servers. Switching providers or models mid-session breaks the cache — the new provider has never seen your prompt prefix before.

This gateway is designed to **eliminate cache misses**:

1. **One primary model per mode** — each mode (auto-free, auto-efficient, auto-balanced, auto-frontier) picks one primary from the Pareto frontier. Eligible dominated candidates remain available only as failure fallbacks. No task-specific routing or complexity tiers.
2. **Session pinning** — the first successful request pins the session to `(provider, model)`. All subsequent requests use the same model. Pin survives transient rate limits (429). Only permanent auth failures (401/403) destroy the pin.
3. **Composite quality score** — `0.8*intelligence + 0.1*coding + 0.1*agentic` gives a well-rounded model for any task. No re-routing based on task type.
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

## Model Identity

Runtime routing never consumes heuristic fuzzy matches. A catalog offering may
receive benchmark quality only from a normalized exact identity, a configured
`model_mappings` entry, or an operator-approved provider-scoped mapping.

Fuzzy matching is restricted to the offline reconciliation workflow:

```bash
model-gateway matching reconcile --json
model-gateway matching reconcile --check
model-gateway matching refresh
model-gateway matching explain opencode-go mimo-v2.5
model-gateway matching approve opencode-go mimo-v2.5 mimo-v2-5-0424
model-gateway pricing coverage --json
```

Reconciliation classifies fresh offerings as `exact`, `configured`,
`approved`, `suggested`, `ambiguous`, or `unmatched`. Suggested and ambiguous
identities never affect routing until approved. If an approved or configured
benchmark disappears after refresh, reconciliation reports the mapping as
unmatched and runtime routing excludes it.

Identity mappings affect benchmark quality only. Provider-scoped pricing is
resolved independently, so approving a benchmark identity never overwrites a
gateway's direct, promotional, or aggregate price.

`pricing coverage` reports every fresh catalog offering as `complete`,
`incomplete`, or `missing`. It includes direct catalog rates and, when
available, the effective provider-profile or canonical fallback source. A
profile fallback can make a model complete even when its catalog record has no
direct rates; partial direct rates remain visible in the report.

`matching refresh` stores source-backed entities and provider aliases from
models.dev and OpenRouter. Exact Hugging Face repository IDs form canonical
entities. An operator may link one of those entities with
`matching approve-entity`; the benchmark link then applies only to aliases that
reference that exact canonical entity. Family, display-name, and release-date
evidence remains suggestion-only.

## Quality Scoring

All paid routes use **composite quality** instead of task-specific scores:

```
composite_quality = 0.8 * intelligence + 0.1 * coding_quality + 0.1 * agentic_quality
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

1. **Access classification** — derive `zero_price`, `quota_limited_free_tier`, `subscription_included`, or `paid` from the catalog signal and current provider billing mode
2. **Availability** — exclude quota-tier models when a recorded account snapshot reports no remaining free quota or a paid account; exhausted state remains blocking until explicitly refreshed
3. **Quality bar** — `free_models_quality.passes()` filters by minimum composite quality (default 30), max age, reference price ($2 input, $10 output), min context length, and min model size
4. **Quality regret** — exclude benchmarked models more than `max_quality_regret` points (default 8) below the best currently available candidate
5. **Pareto ranking** — `pareto_rank(composite_quality, reference quota cost, latency)`
   - Explicit zero-price models use cost 0
   - Quota-tier models use their list-price expected cost as a scarcity proxy
   - Latency chooses among models with reasonably comparable quality
6. **Sort** — pinned first → reference quota cost → latency → quality
7. **Fallback** — Pareto candidates → unbenchmarked models → local

Free-tier eligibility rules are provider-specific. See [providers.md](providers.md).
Free catalog and route responses expose effective input/output prices as zero.
`zero_price` uses `source: "provider_free"`; quota-tier access uses
`source: "free_tier"`. `reference_price_per_million` preserves provider or
benchmark list prices for price caps and quota-conservation ranking. Reference
prices never classify a free offering as paid.

## `auto-efficient`

Best bang-for-buck. Quality floor: **40**. Pipeline:

1. **`all_candidates`** — all models from `catalog_models`
2. **Availability filter** — remove unavailable providers and free-only providers when billing requires paid
3. **Capability filter** — context length, tools, vision, structured output
4. **Composite quality floor** — `efficient_quality_floor` (default 40.0)
5. **Pareto ranking** — `pareto_rank(composite_quality, cost_microusd, latency)`
   - Removes dominated candidates (worse on all three axes)
   - Sorts non-dominated by cost → latency → quality
6. **Eligible fallback fill** — after the Pareto candidates, retain dominated
   candidates that still satisfy the mode's quality, capability, identity, and
   pricing requirements
7. **Session pin** — pinned models sort first within their rank group
8. **Fallback** — `auto-free` → `local`

Expected cost is computed from the offering's input/output prices and estimated request tokens. Cost-based quota windows impose spend caps.

Models from profiles with known included-subscription semantics report effective cost zero with `source: "subscription"`, while reference prices remain available for Pareto efficiency ranking and diagnostics. They are eligible for efficient/balanced/frontier routes, never `auto-free`. A generic provider configured with `billing_mode = "subscription"` remains priced unless its profile explicitly supports included inference.

## `auto-balanced`

Mid-range quality. Quality floor: **60**. Same pipeline as auto-efficient with a higher quality floor. Targets models that are great quality but not the most expensive — DeepSeek V4 Pro, MiMo v2.5 Pro, GPT 5.6 Luna class.

- Quality floor: `balanced_quality_floor` (default 60.0)
- Falls back to `auto-free` → `local`
- Disable with `auto_balanced_enabled = false`

## `auto-frontier`

Top tier. Quality floor: **50**. Same pipeline as auto-efficient/balanced — all paid models, differentiated by quality floor.

- Quality floor: `frontier_quality_floor_single` (default 50.0)
- **Never falls back** — returns a generic error when no candidate is available

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

Returns all currently eligible free models filtered by the quality bar. Supports `?provider=`, `?task=`, `?limit=` query parameters. Task filters are for discovery/rankings display — routing uses composite quality. Each result includes `access.kind`, `overage: "gateway_blocked"`, effective zero pricing, reference pricing, and recorded account remaining/free-tier/fetch-time values when available.

### `/v1/paid-models`

Returns a compact, paginated summary of non-free models from paid/subscription providers. Supports `?task=`, `?limit=`, `?page=`, and `?provider=` query parameters. `limit` defaults to 25 and is capped at 100. Every result includes a `links.self` URL for its complete resource.

Use `?view=full` when a collection response must include the complete pricing, benchmark, capability, access, and provenance fields. Prefer the individual resource for one model:

### `/v1/models/{provider}/{model}`

Returns the complete model resource, including benchmark matching, cache pricing, reference pricing, access limits, freshness, and provenance. The `{model}` portion may contain additional path segments; the resource ID is the exact `provider/model` identifier returned by `/v1/paid-models`.

Collection responses include `page`, `per_page`, `total`, `pages`, and `links.self`/`links.prev`/`links.next` metadata. Links preserve the active filters and view, so clients can follow them without reconstructing request parameters.

### `/v1/auto-models`

Shows the current routing mode configuration with the top model selections for each mode. Returns a Pareto-frontier primary plus up to two additional eligible candidates as fallbacks. A dominated candidate may be a fallback but never displaces a non-dominated primary. Supports `?route=free|efficient|balanced|frontier` to filter a single mode.

### `/v1/rankings`

Read-only view of fresh benchmark data. Sorted by quality score (descending). Supports `?task=` and `?limit=` query parameters. Never performs live benchmark requests. See [benchmarks.md](benchmarks.md) for the full response format, setup, and attribution.
