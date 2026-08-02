# Routing

## Cache-Aware Design

**Prompt caching is provider-scoped.** Each provider (OpenAI, Anthropic, etc.) caches prompt prefixes per-model on their servers. Switching providers or models mid-session breaks the cache — the new provider has never seen your prompt prefix before.

The gateway minimizes avoidable provider/model changes:

1. **One primary model per mode** — each mode returns one primary and up to two failure fallbacks. Benchmark-backed paid routes use the Pareto frontier; auto-free can use eligible unbenchmarked candidates after its benchmarked candidates.
2. **Session pinning** — the first successful request pins the session to `(provider, model)`. All subsequent requests use the same model. Pin survives transient rate limits (429). Only permanent auth failures (401/403) destroy the pin.
3. **Composite quality score** — `0.8*intelligence + 0.1*coding + 0.1*agentic` gives a well-rounded model for any task. No re-routing based on task type.
4. **Pareto frontier handles reasoning effort** — benchmark entries for different reasoning efforts are evaluated as separate candidates. The selected effort is included in the model ID metadata and applied to the upstream request.

**The result**: pick a mode and the gateway keeps a successful session on the same provider/model for 30 minutes when it can derive a session identity. The gateway cannot guarantee a provider's cache behavior, but avoiding unnecessary model switches preserves the best chance of a cache hit.

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

The Pareto frontier operates on ALL benchmark entries (including different
`reasoning_effort` levels). It uses measured task cost and end-to-end latency
when available, so the selected effort reflects actual reasoning consumption
instead of treating every subscription request as equally free.

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

Cost-first automatic selection. Quality floor: **35**. Pipeline:

1. **`all_candidates`** — all models from `catalog_models`
2. **Availability filter** — remove unavailable providers and free-only providers when billing requires paid
3. **Capability filter** — context length, tools, vision, structured output
4. **Composite quality floor** — `efficient_quality_floor` (default 35.0)
5. **Pareto ranking** — `pareto_rank(composite_quality, measured_task_cost, end_to_end_latency)`
   - Removes dominated candidates (worse on all three axes)
   - Sorts non-dominated by cost → latency → quality
6. **Eligible fallback fill** — after the Pareto candidates, retain dominated
   candidates that still satisfy the mode's quality, capability, identity, and
   pricing requirements
7. **Session pin** — pinned models sort first within their rank group
8. **Fallback** — `auto-free` → `local`

`auto-efficient` retains cost-first ordering. `auto-frontier` uses the same
strict Pareto filtering but applies the latency-aware selection policy described
above.

Artificial Analysis measured task cost is authoritative for efficiency ranking.
When at least one candidate has measured task cost, candidates without it are
ranked after measured candidates; their price-derived scenario is exposed only
for diagnostics. If no measured costs exist, the route may use the scenario as
a provisional fallback. Cost-based quota windows still use request-specific
token estimates for admission, not as a claim about benchmark task cost.

Models from profiles with known included-subscription semantics report effective cost zero with `source: "subscription"`, while reference prices remain available for Pareto efficiency ranking and diagnostics. They are eligible for efficient/balanced/frontier routes, never `auto-free`. A generic provider configured with `billing_mode = "subscription"` remains priced unless its profile explicitly supports included inference.

### Frontier selection policy

`auto-frontier` preserves the exact three-axis Pareto frontier, then orders
those non-dominated candidates with a transparent latency-aware utility:

- quality: 50%
- measured task-cost efficiency: 25%
- latency efficiency: 25%

Cost and latency efficiencies are normalized against the best frontier
candidate, so a very slow model cannot win solely because it is cheaper. This
does not remove a legitimate cost/quality tradeoff from the frontier; it only
prevents cost-first sorting from always selecting the cheapest point.

## `auto-balanced`

Mid-range quality. Quality floor: **42**. Same pipeline as auto-efficient with a higher quality floor. Targets models that are great quality but not the most expensive — DeepSeek V4 Pro, MiMo v2.5 Pro, GPT 5.6 Luna class.

- Quality floor: `balanced_quality_floor` (default 42.0)
- Falls back to `auto-free` → `local`
- Disable with `auto_balanced_enabled = false`

## `auto-frontier`

Highest quality-floor automatic selection. Quality floor: **52**. It uses the same candidate filters as the other paid routes, then applies the latency-aware frontier ordering described above.

- Quality floor: `frontier_quality_floor_single` (default 52.0)
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

### `/v1/catalog/models`

The canonical model discovery collection. Use `?access=free|paid|all`, `?task=`, `?provider=`, `?limit=`, `?cursor=`, `?view=summary|full`, `?variants=collapsed|all`, and optional `?fields=`. The default is a compact summary of at most 25 models; each item contains only `id`, separate `reasoning_effort`, `quality`, and an absolute clickable `links.self` URL. Use the linked model resource or `?view=full&variants=all` to inspect benchmark metrics and every reasoning-effort variant.

`fields` is a strict, comma-separated allowlist of exact top-level item fields: `id`, `object`, `links`, `model`, `composite`, `scores`, `capabilities`, `price_per_million`, `reference_price_per_million`, `access`, `benchmark_match`, `benchmark_id`, `benchmarks`, `quality`, and `reasoning_effort`. It is projection-only: it never changes filtering, ranking, total counts, or snapshot-bound cursors. Duplicate names are removed and equivalent orderings are canonicalized for stable links and ETags. Without `fields`, `view` chooses the default summary or full representation. With `fields`, the requested allowlisted fields are returned exactly, so `view=full&fields=id,links,benchmarks` is a smaller diagnostic projection; unknown names return `400 invalid_fields`. The response records an explicit projection in `meta.fields`.

Cursors are bound to the catalog snapshot; a refresh invalidates an old cursor with `409 stale_cursor`. Keep `limit` small for interactive clients and follow the returned `links.next` URL for large catalogs. Responses include `meta.snapshot`, `meta.total`, navigable links, `ETag`, `Last-Modified`, and conditional `304 Not Modified` support. `If-None-Match` takes precedence over `If-Modified-Since`; a matching validator returns `304` with no response body.

The gateway briefly reuses assembled catalog snapshots for repeated requests, while source timestamps and snapshot fingerprints keep validators tied to changed data. ETags reduce transfer and JSON parsing when a client revalidates an unchanged representation.

### `/v1/catalog/models/{provider}/{model}`

Returns one complete model resource, including benchmark metrics, benchmark matching, cache pricing, reference pricing, access limits, freshness, and provenance. The model portion may contain additional path segments; use the exact encoded resource link returned by the collection.

The catalog collection is the model-discovery listing surface. New integrations should use the canonical catalog paths and `/openapi.json`.

### `/v1/auto-models`

Shows the current routing mode configuration with the top model selections for each mode. The default summary includes only `id`, `quality`, separate `reasoning_effort`, and an absolute clickable `links.self` URL to the complete catalog resource. Use `?view=full` when route diagnostics need benchmark matching and per-million pricing details. Returns a Pareto-frontier primary plus up to two additional eligible candidates as fallbacks. A dominated candidate may be a fallback but never displaces a non-dominated primary. Supports `?route=free|efficient|balanced|frontier` to filter a single mode.

Capability fields are included only when the provider reports authoritative values. An omitted capability means unknown; the gateway never infers capabilities from a model name. The complete model resource is the source for all available metadata.

### `/v1/rankings`

Read-only view of fresh benchmark data. Sorted by quality score (descending). Supports `?task=` and `?limit=` query parameters. Never performs live benchmark requests. See [benchmarks.md](benchmarks.md) for the full response format, setup, and attribution.

### Health endpoints

`/health/live` only confirms that the process is responding. `/health/ready`
confirms that the routing database is readable; it intentionally remains green
when an individual provider credential or catalog is unavailable so local-only
and partially configured gateways can still start. Use `/health/diagnostics`
for startup and deployment checks. It reports each configured provider's
credential state (`present`, `missing`, or `not_required`), credential source,
catalog freshness, benchmark status, and whether the provider is dispatchable.
It never returns credential values and excludes the internal local runtime
provider.

Automatic routes also expose `latency_observed` in `/v1/auto-models`. A model
with missing benchmark latency has `latency_available: false` and
`latency_seconds: null`; it is never treated as a zero-latency model. Routed
chat responses expose the same condition through the
`x-model-gateway-benchmark-latency-observed` header.
