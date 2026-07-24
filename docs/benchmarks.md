# Benchmarks

Benchmarks provide quality, cost, and latency scores for 500+ models sourced from [Artificial Analysis](https://artificialanalysis.ai/). They are **required** for `auto-efficient` and `auto-frontier` routing, and used for quality-aware ranking in `auto-free`.

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

The gateway auto-fetches benchmarks on startup when:
- The API key is configured, **and**
- No fresh benchmark data exists

It keeps data updated on a background refresh schedule (approximately every ~3.5 days).

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

Each model has up to five scored fields:

| Field | Range | Description |
|---|---|---|
| `intelligence` | 0–100 | General quality score |
| `coding_quality` | 0–100 | Coding task quality score |
| `agentic_quality` | 0–100 | Agentic/tool-use quality score |
| `input_price_per_million` | $ | Price per million input tokens |
| `output_price_per_million` | $ | Price per million output tokens |
| `latency_seconds` | Seconds | Time to first token |
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

Task-specific quality is used for response headers and listing endpoints (`/v1/free-models`, `/v1/paid-models`).

### Composite Quality (Used for Routing)

Routing uses a single **composite quality** score instead of task-specific scores:

```
composite_quality = 0.5 * intelligence + 0.3 * coding_quality + 0.2 * agentic_quality
```

If `coding_quality` or `agentic_quality` is None, the weight redistributes to `intelligence`. This gives a well-rounded score that doesn't favor any single task type — important since each mode recommends a single model that should handle all tasks well.

The Pareto frontier operates on ALL benchmark entries (including different reasoning_effort levels). It naturally picks the most efficient variant — e.g., GPT 5.6 Sol (medium effort) over Sol Max (high effort) because Sol has better quality/cost ratio.

### Complexity Classification

The same `classify()` function also determines task complexity:

| Complexity | Criteria (score ≥ threshold) |
|---|---|
| `Simple` | Score 0–1 (basic questions, no tools, ≤4 messages, short text) |
| `Medium` | Score 2–3 |
| `Complex` | Score 4–5 (tools, keywords, longer context) |
| `VeryComplex` | Score 6+ (tools + keywords + long conversation + structured output) |

Complexity is used for response headers and the `/v1/auto-models` display. Routing uses composite quality with a single floor per mode.

## Ranking Endpoint

View live benchmark rankings at any time:

```bash
curl "http://127.0.0.1:8008/v1/rankings?task=coding&limit=20"
```

Parameters:

| Parameter | Default | Description |
|---|---|---|
| `task` | `general` | `general`, `coding`, `agentic`, or `reasoning` |
| `limit` | `100` | Max models to return (1–1,000) |

Response:

```json
{
  "object": "benchmark.rankings",
  "task": "coding",
  "max_age_seconds": 86400,
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

Rankings are sorted by quality score (descending), then by combined price (ascending), then model ID (alphabetically). The endpoint only uses **fresh persisted** data — never performs live benchmark requests.

## Route Usage

| Route | Benchmark Dependency | Quality Scoring |
|---|---|---|
| `auto-free` | Uses composite quality for Pareto ranking (quality × latency). Falls back to unbenchmarked models if none exist. | Composite |
| `auto-efficient` | **Requires** benchmarks. Models without matching benchmark entries are excluded. | Composite |
| `auto-balanced` | **Requires** benchmarks. Same as auto-efficient with higher quality floor. | Composite |
| `auto-frontier` | **Requires** benchmarks. Also filters by canonical creator (OpenAI/Anthropic only). | Composite |

All paid routes use composite quality (`0.5*intelligence + 0.3*coding + 0.2*agentic`). The Pareto frontier operates on ALL benchmark entries including different reasoning_effort levels, naturally picking the most efficient variant.

## Configuration

| Env Variable | Default | Description |
|---|---|---|
| `MODEL_GATEWAY_BENCHMARK_MAX_AGE_SECONDS` | `604800` (7d) | Maximum age before data is considered stale |
| `MODEL_GATEWAY_EFFICIENT_QUALITY_FLOOR` | `40.0` | Composite quality floor for auto-efficient |
| `MODEL_GATEWAY_BALANCED_QUALITY_FLOOR` | `60.0` | Composite quality floor for auto-balanced |
| `MODEL_GATEWAY_FRONTIER_QUALITY_FLOOR` | `80.0` | Composite quality floor for auto-frontier |

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

Delete a snapshot:

```bash
model-gateway benchmarks delete my-source
```

## How Benchmarks Power Routing

The Pareto ranking algorithm (`pareto_rank` in `src/benchmarks.rs`) uses three axes:

1. **Quality** — the task-specific score (higher is better)
2. **Expected cost** — estimated USD per request from model pricing (lower is better, always 0 for free models)
3. **Latency** — seconds to first token (lower is better)

A candidate is **dominated** if another model is at least as good on all axes and strictly better on at least one. Dominated candidates are removed. The surviving frontier is sorted by cost → latency → quality.

For free models, cost is always 0, so the comparison degenerates to quality vs latency — a fast model with sufficient quality beats a slow overqualified one.

## Quality Floor Validation

Quality floors are validated on config load:

- Each floor must be 0.0–100.0
- Floors must be ordered: `simple ≤ medium ≤ complex`
- Violations produce a clear config error at startup

Setting a floor to 0.0 disables it (all models pass).
