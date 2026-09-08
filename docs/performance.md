# Performance audit

The performance audit provides a reproducible baseline for cold builds and the
local request path. It uses the deterministic loopback mock provider and never
contacts a real model provider or reads provider credentials.

## Run it

```bash
python3 -m unittest discover -s tests -p 'test_performance_audit.py'
python3 scripts/performance_audit.py --check --output performance-results.json
```

The default run builds debug and release profiles from empty temporary target
directories with `cargo build --locked` and incremental compilation disabled.
It then starts the release binary, waits for `/health/ready`, warms up both
request paths, and records 100 direct-provider and 100 gateway-routed samples.
Temporary builds, state, logs, and processes are removed on exit.

Use `--binary target/release/model-gateway` for a fast runtime-only diagnostic.
Runtime-only results intentionally omit build timings and therefore cannot pass
`--check`; their JSON reports `thresholds_checked: false` and `passed: null`.
The full audit is the regression gate.

## Metrics and thresholds

The committed limits in [`performance/thresholds.json`](../performance/thresholds.json)
are portability budgets, not marketing targets. They are deliberately above the
recorded baseline so normal shared-runner variance does not make the gate flaky.

| Metric | Definition | Maximum |
|---|---|---:|
| `debug_build_seconds` | Locked, non-incremental debug build in an empty target directory | 600 s |
| `release_build_seconds` | Locked, non-incremental release build after the debug build | 900 s |
| `startup_ready_ms` | Release process spawn until `/health/ready` succeeds | 5,000 ms |
| `gateway_request_p95_ms` | p95 fixed-route Chat Completions latency over loopback | 100 ms |
| `provider_routing_overhead_p50_ms` | Gateway fixed-route p50 minus direct mock-provider p50 | 25 ms |
| `resident_memory_mib` | Gateway RSS after warmup and measured requests | 128 MiB |
| `release_binary_mib` | Uncompressed release executable size | 32 MiB |

`provider_routing_overhead_p50_ms` includes request parsing, fixed-alias target
resolution, exact provider/model lookup, SQLite-backed effective-price lookup,
proxying, response validation, and gateway response headers. It excludes the
mock provider's own measured latency by using the direct path as the control.

Code Foundry's shared `Validation / Test / Performance` job runs the audit on
pull requests and scheduled or manual validation. Its ordered command contract
first tests the audit calculations, then enforces the thresholds. Its JSON result
and shared summary are uploaded as workflow artifacts so regressions can be
compared without copying log text.

## Baseline and findings

The initial M0 baseline was recorded on 2026-09-08 UTC from the M0 working tree
based on `17ef8d2`, using Rust 1.97.1 on Apple Silicon. The full audit passed:

| Metric | Baseline | Budget | Headroom |
|---|---:|---:|---:|
| Locked debug build | 25.994 s | 600 s | 95.7% |
| Locked release build | 57.168 s | 900 s | 93.6% |
| Startup to ready | 471.404 ms | 5,000 ms | 90.6% |
| Gateway request p50 | 0.482 ms | diagnostic | — |
| Gateway request p95 | 0.592 ms | 100 ms | 99.4% |
| Provider-routing overhead p50 | 0.181 ms | 25 ms | 99.3% |
| Resident memory | 16.062 MiB | 128 MiB | 87.5% |
| Release binary | 13.570 MiB | 32 MiB | 57.6% |

These figures describe this machine and runner conditions, not universal
product claims. Refresh the table only when the harness or an intentional
performance change materially shifts the same-machine baseline; CI artifacts
remain the source for shared-runner comparisons.

Dependency inspection found no removable duplicate runtime dependency: the two
`syn` major versions are independently required by current proc-macro trees.
The direct Tokio dependency previously enabled the broad `full` feature set;
M0 narrows it to the runtime's actual macros, networking, multi-thread runtime,
signals, synchronization, and timing needs. Cargo still unifies any transitive
features required by Axum and Reqwest.

The fixed-route hot path performs an effective-price lookup before proxying.
That preserves accounting metadata and is covered by API tests. Caching or
skipping the lookup could reduce routing overhead, but is intentionally deferred
until the measured delta approaches the budget because either change needs an
explicit invalidation contract. The safe current action is to retain the shared
Reqwest client and the bounded blocking-database boundary, measure the whole
path, and fail CI before an accidental regression ships.

## Interpreting results

Cold build timings are useful only when compared on the same runner class.
Loopback latency is not upstream model latency; it isolates gateway overhead.
RSS is a post-warmup process snapshot rather than a peak-memory measurement.
Artifact size is the native executable, not a compressed release archive or
container image. Provider behavior and public API compatibility remain governed
by the full Rust unit, integration, smoke, OpenAPI, and release validation suites.
