# Benchmarks

Benchmarks provide quality, cost, and latency measurements sourced from [Artificial Analysis](https://artificialanalysis.ai/). Fresh benchmark data is required for `auto-efficient`, `auto-balanced`, and `auto-frontier`. `auto-free` can still expose eligible unbenchmarked models, but benchmarked candidates are ranked first.

> **Attribution**: All benchmark data is sourced from Artificial Analysis (https://artificialanalysis.ai/). Redistribution must include this attribution. See `/v1/rankings` response `snapshots` for the exact attribution per snapshot.

## Setup

### 1. Get an API Key

Sign up at [Artificial Analysis](https://artificialanalysis.ai/) for a free API key.

### 2. Configure

```bash
model-gateway credentials set ARTIFICIAL_ANALYSIS_API_KEY
```

Or set the environment variable:

```bash
export ARTIFICIAL_ANALYSIS_API_KEY="your-key-here"
```

### 3. Auto-Fetch (Recommended)

The gateway starts a background benchmark refresh when:
- The API key is configured, **and**
- No fresh benchmark data exists

It retries on a background schedule derived from the configured freshness window. A failed refresh preserves the active snapshot.

### 4. Manual Refresh

```bash
model-gateway benchmarks refresh
```

This fetches the latest data from `https://artificialanalysis.ai/api/v2/language/models/free`.

### 5. Verify

```bash
model-gateway benchmarks status
```

Example output:
```
active snapshots:
  artificial-analysis: 512 models, fetched_at=1745612345, attribution=Artificial Analysis (https://artificialanalysis.ai/)
```

## What Benchmarks Provide

Each model may include quality scores plus pricing, latency, output-size, provenance, and raw metric fields:

| Field | Range | Description |
|---|---|---|
| `intelligence` | 0–100 | General quality score |
| `coding_quality` | 0–100 | Coding task quality score |
| `agentic_quality` | 0–100 | Agentic/tool-use quality score |
| `input_price_per_million` | $ | Price per million input tokens |
| `output_price_per_million` | $ | Price per million output tokens |
| `cache_read_price_per_million` | $ | Cache-hit price per million tokens |
| `cache_write_price_per_million` | $ | Cache-write price per million tokens |
| `cost_per_task_usd` | $ | Artificial Analysis measured cost per Intelligence Index task |
| `latency_seconds` | Seconds | Median time to first token |
| `time_to_first_answer_seconds` | Seconds | Median time to first answer token |
| `end_to_end_response_seconds` | Seconds | Median end-to-end response time |
| `output_tokens_per_second` | Tokens/s | Median output throughput |
| `output_tokens_per_task` | Tokens | Average output length |
| `reasoning_effort` | String | Reasoning variant (e.g., `low`, `high`) |
| `as_of` | Date | Benchmark measurement date |
| `release_date` | Date | Model release date |
| `raw_metrics` | Map | Raw unscaled metric values |

### Task-Specific Quality

The `classify()` function maps each request to one of three task types, and `quality_for()` selects the corresponding score:

| Request Classification | Quality Score Used |
|---|---|
| `General` — no coding or agentic keywords | `intelligence` |
| `Coding` — code/implement/debug/refactor/test keywords | `coding_quality` (falls back to `intelligence`) |
| `Agentic` — multi-step/tool/agent/workflow keywords or `tools` array | `agentic_quality` (falls back to `intelligence`) |

Task-specific quality is used for response headers and the `/v1/catalog/models` collection.

### Composite Quality (Used for Routing)

Routing uses a single **composite quality** score instead of task-specific scores:

```
composite_quality = 0.80 * intelligence + 0.10 * coding_quality + 0.10 * agentic_quality
```

If `coding_quality` or `agentic_quality` is None, the weight redistributes to `intelligence`. This gives a well-rounded score that doesn't favor any single task type — important since each mode recommends a single model that should handle all tasks well.

The Pareto frontier operates on ALL benchmark entries (including different
`reasoning_effort` levels). It uses measured task cost and end-to-end latency
when available, so high-effort variants are not treated as free merely because
their provider subscription has no per-request charge.

### Complexity Classification

The same `classify()` function also determines task complexity:

| Complexity | Criteria (score ≥ threshold) |
|---|---|
| `Simple` | Score 0–1 (basic questions, no tools, ≤4 messages, short text) |
| `Medium` | Score 2–3 |
| `Complex` | Score 4–5 (tools, keywords, longer context) |
| `VeryComplex` | Score 6+ (tools + keywords + long conversation + structured output) |

Complexity is used for response headers only. Routing uses composite quality with a single floor per mode.

## Ranking Endpoint

View live benchmark rankings at any time:

```bash
curl "http://127.0.0.1:8008/v1/rankings?task=coding&limit=20"
```

Parameters:

| Parameter | Default | Description |
|---|---|---|
| `task` | `general` | `general`, `coding`, or `agentic` |
| `limit` | `100` | Max models to return (1–1,000) |

Response:

```json
{
  "object": "benchmark.rankings",
  "task": "coding",
  "max_age_seconds": 604800,
  "snapshots": [{
    "source": "artificial-analysis",
    "fetched_at": 1745612345,
    "models": 512,
    "attribution": "Artificial Analysis (https://artificialanalysis.ai/)"
  }],
  "data": [{
    "rank": 1,
    "id": "gpt-4o",
    "creator": "OpenAI",
    "scores": {
      "intelligence": 95.0,
      "coding": 92.0,
      "agentic": 88.0
    },
    "input_price_per_million": 2.5,
    "output_price_per_million": 10.0,
    "latency_seconds": 1.2,
    "reasoning_effort": null,
    "as_of": "2025-06-01",
    "release_date": "2025-04-01"
  }]
}
```

Rankings are sorted by the selected task score (descending), then by combined standard input/output price (ascending), then model ID alphabetically. The endpoint only uses fresh persisted data; it never performs a live benchmark request.

## Route Usage

| Route | Benchmark Dependency | Quality Scoring |
|---|---|---|
| `auto-free` | Uses composite quality plus reference cost and latency when benchmark data exists. Eligible unbenchmarked models remain as lower-priority fallbacks. | Composite |
| `auto-efficient` | **Requires** benchmarks. Models without matching benchmark entries are excluded. | Composite |
| `auto-balanced` | **Requires** benchmarks. Same as auto-efficient with higher quality floor. | Composite |
| `auto-frontier` | **Requires** benchmarks. Highest quality floor. | Composite |

All paid routes use composite quality (`0.80*intelligence + 0.10*coding + 0.10*agentic`). The Pareto frontier operates on all benchmark entries including different `reasoning_effort` levels, using measured task cost and end-to-end latency when available.

## Configuration

| Env Variable | Default | Description |
|---|---|---|
| `MODEL_GATEWAY_BENCHMARK_MAX_AGE_SECONDS` | `604800` (7d) | Maximum age before data is considered stale |
| `MODEL_GATEWAY_EFFICIENT_QUALITY_FLOOR` | `35.0` | Composite quality floor for auto-efficient |
| `MODEL_GATEWAY_BALANCED_QUALITY_FLOOR` | `42.0` | Composite quality floor for auto-balanced |
| `MODEL_GATEWAY_FRONTIER_QUALITY_FLOOR` | `52.0` | Composite quality floor for auto-frontier |

See [configuration.md](configuration.md) for the full list of server settings.

## Importing Custom Benchmarks

Import benchmarks from any compatible JSON file:

```bash
model-gateway benchmarks import --file ./my-benchmarks.json
```

The file must follow the `BenchmarkImport` format:

```json
{
  "source": "my-source",
  "attribution": "My Source (https://example.com/)",
  "models": [
    {
      "id": "my-model",
      "intelligence": 85.0,
      "coding_quality": 78.0,
      "agentic_quality": 72.0,
      "input_price_per_million": 1.0,
      "output_price_per_million": 4.0,
      "latency_seconds": 0.8
    }
  ]
}
```

- `source` and `attribution` are required (1–1,024 chars)
- All scores are 0–100
- Validated on import: empty IDs, out-of-range scores, and excessive attribution length are rejected

### Pricing Sources and Overrides

Pricing is resolved by serving-provider scope. A complete price pair from the
provider catalog, including a temporary free or discounted price, is preserved
and beats creator or aggregate pricing. Models.dev and benchmark prices fill
gaps only when the serving provider has no complete pair. Prices are never
matched fuzzily.

Refresh the public provider-scoped pricing catalog:

```bash
model-gateway pricing refresh
model-gateway pricing status
model-gateway pricing explain opencode-go mimo-v2-pro
```

When no provider price is available, import an exact provider-scoped override:

```bash
model-gateway pricing import --file ./pricing-overrides.jsonl
```

Each non-empty line requires the runtime provider and exact model ID:

```jsonl
{"provider":"opencode-go","model":"mimo-v2-pro","input_price_per_million":1.0,"output_price_per_million":3.0}
```

Overrides are isolated from quality benchmarks and take precedence for that
exact provider/model pair. Records must contain a complete standard input and
output price pair. Models without a complete effective pair remain discoverable
but are excluded from paid auto-route Pareto ranking rather than treated as
free.

Delete a benchmark snapshot:

```bash
model-gateway benchmarks delete my-source
```

## How Benchmarks Power Routing

The Pareto ranking algorithm (`pareto_rank` in `src/benchmarks.rs`) uses three axes:

1. **Quality** — composite quality for auto routes (higher is better)
2. **Expected cost** — Artificial Analysis cost per Intelligence Index task when
   available, otherwise an estimate from model pricing and request tokens
   (lower is better)
3. **Latency** — end-to-end response seconds when available, otherwise time to
   first token (lower is better)

A candidate is **dominated** if another model is at least as good on all axes and strictly better on at least one. Dominated candidates are removed. The resulting frontier is exact and preserves every non-dominated quality/cost/latency tradeoff.

For `auto-frontier`, the non-dominated candidates are then ordered with a
latency-aware utility: 50% quality, 25% measured task-cost efficiency, and 25%
latency efficiency. Other auto modes retain their documented mode-specific
cost/latency/quality ordering.

Effective provider price remains zero for free and included-subscription
routes, but measured task cost is retained as the reference efficiency cost.
This prevents subscription models that consume substantially more reasoning
tokens from appearing artificially free in frontier calculations. A zero-price
model with no measured task cost still falls back to zero.

Price-derived task-cost scenarios are not treated as measured costs. When a
route has at least one measured task cost, models without one are ranked after
measured candidates and expose their price scenario separately. The scenario
is intentionally diagnostic only because token pricing cannot reproduce
provider-reported reasoning, cache, and agent-token usage.

## Quality Floor Validation

Quality floors are validated on config load. Each floor must be finite and
between 0.0 and 100.0; invalid values stop startup with a configuration error.
Setting a route floor to 0.0 allows every benchmarked candidate through that
floor.
