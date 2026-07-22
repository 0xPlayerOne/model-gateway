# AGENTS.md — model-gateway

## Stack
- Rust 2024 edition, toolchain pinned to **1.87.0** via `rust-toolchain.toml`
- Single crate: lib (`src/lib.rs`) + bin (`src/main.rs`)
- Axum HTTP server, Tokio async runtime, rusqlite state, reqwest HTTP client

## Commands

```bash
# Core validation (what CI runs on every push/PR):
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release

# Dependency policy (requires cargo-deny on +stable, NOT pinned 1.87.0):
cargo +stable install cargo-deny --version 0.20.2 --locked
cargo +stable deny check

# Smoke tests (require release binary already built):
./scripts/native-smoke.sh
./scripts/docker-smoke.sh           # needs Docker
./scripts/hermes-smoke.sh           # needs uv/uvx + Python
./scripts/release-smoke.sh <dir>    # validates release archives

# Provider connectivity check (needs .env.local sourced):
./scripts/core-provider-check.sh
```

## Source layout

| Module | Role |
|---|---|
| `src/gateway.rs` | Axum routes, request handling, SSE streaming, fallback logic (~2500 lines) |
| `src/config.rs` | TOML config parsing, validation, provider/model structures |
| `src/routing.rs` | SQLite-backed routing store: catalogs, benchmarks, quotas, cooldowns |
| `src/providers.rs` | Provider profiles (`BuiltinProvider`), catalog fetching, request preparation |
| `src/benchmarks.rs` | Benchmark import/parse, task classification, quality/cost selection |
| `src/secrets.rs` | Secret resolution: keyring, file, or environment |
| `src/storage.rs` | Private module: atomic file writes, SQLite helpers |
| `src/main.rs` | CLI (clap): `setup`, `serve`, `config`, `credentials`, `catalog`, `benchmarks`, `healthcheck` |

## Architecture notes

- Built-in routes `local`, `auto-free`, `auto-efficient`, `auto-frontier` exist alongside user-defined model aliases. `auto-frontier` can be disabled via `server.auto_frontier_enabled = false`.
- Gateway binds `127.0.0.1:8008` by default. No caller authentication exists; do not broaden the bind without designing auth first.
- Config file default path: `~/.config/model-gateway/config.toml`. Override with `MODEL_GATEWAY_CONFIG`.
- The gateway **never** loads `.env` files automatically. For `MODEL_GATEWAY_SECRET_STORE=environment`, export vars before running.
- Secrets are never logged. Config diffs, request logs, and error responses never contain credential values.
- Provider catalogs and benchmarks are refreshed explicitly (`catalog refresh`, `benchmarks refresh`); serving never makes outbound catalog requests.
- Routing state (catalogs, quotas, cooldowns, session pins) lives in SQLite at `MODEL_GATEWAY_STATE_PATH` (default `~/.config/model-gateway/routing.sqlite3`). Prompts and responses are never stored.

## Testing

- **Unit tests**: inline `#[cfg(test)]` modules in each source file.
- **CLI integration tests** (`tests/cli.rs`): spawn the real binary with isolated temp dirs and `MODEL_GATEWAY_SECRET_STORE=environment`.
- **Gateway integration tests** (`tests/gateway_smoke.rs`, ~1400 lines): spin up ephemeral Axum mock providers on random ports, then exercise routing, fallback, streaming, rate-limit cooldown, catalog capability filtering, benchmark-driven selection, and frontier constraints. Uses `tempfile::tempdir()` for state isolation.
- All tests are hermetic — no network calls, no real credentials, no shared state.
- CI also runs `./scripts/native-smoke.sh` (macOS) and `./scripts/docker-smoke.sh` + `./scripts/hermes-smoke.sh` (Linux).

## Docker

- `Dockerfile`: multi-stage build from `rust:1.87-bookworm`, runs as non-root `model-gateway` user.
- `Dockerfile.release-runtime`: copies pre-built native binary (no Rust compilation). Used by release CI for multi-arch images.
- `docker-compose.yml`: `gateway` service (always) + `setup` service (profile `setup`). Mounts `.model-gateway/` as read-only config.
- Host port fixed to `127.0.0.1:8008`. The setup container needs `MODEL_GATEWAY_UID`/`MODEL_GATEWAY_GID` exported.
- Docker smoke tests verify: non-root UID, dropped capabilities, read-only filesystem, secret mount permissions (700/600), and that `local_container` mode refuses to start without the container marker.

## Release

- Tag-gated: pushing `v*` triggers publish. The release workflow validates `Cargo.toml` version matches the git tag.
- Packages native archives for x86_64-linux, aarch64-linux, x86_64-macos, aarch64-macos, plus multi-arch GHCR container.
- `cargo install --locked --path .` for local install from source.

## Conventions

- `Cargo.lock` is committed (binary crate convention).
- `.env.local` contains real API keys for manual testing; it is gitignored (`.env.*` pattern). Never reference it in code or CI.
- `gateway.*.example.toml` files are tiered: `core` (recommended), `secondary` (useful), `optional` (untested). The `gateway.example.toml` is the full reference.
- Provider profiles are defined in `BuiltinProvider` enum in `src/providers.rs`. Adding a provider requires a new variant there plus entry in the example configs.
- Error responses always use OpenAI-shaped JSON: `{"error": {"message": "...", "type": "...", "code": "..."}}`.
