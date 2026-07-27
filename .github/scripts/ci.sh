#!/usr/bin/env bash
set -euo pipefail

if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi

has_script() {
  [ -f package.json ] && node -e 'const p=require("./package.json"); process.exit(p.scripts?.[process.argv[1]] ? 0 : 1)' "$1"
}

has_javascript() {
  git ls-files -- '*.js' '*.jsx' '*.ts' '*.tsx' | grep -q .
}

has_python() {
  [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f requirements-dev.txt ] || \
    git ls-files -- '*.py' | grep -q .
}

package_manager() {
  if [ -f bun.lock ] || [ -f bun.lockb ]; then echo bun
  elif [ -f pnpm-lock.yaml ]; then echo pnpm
  elif [ -f yarn.lock ]; then echo yarn
  elif [ -f package-lock.json ]; then echo npm
  else echo bun
  fi
}

run_script() {
  if ! has_script "$1"; then echo "Skipping $1 (script not defined)"; return; fi
  case "$(package_manager)" in
    bun) bun run "$1" ;;
    pnpm) corepack pnpm run "$1" ;;
    yarn) corepack yarn run "$1" ;;
    npm) npm run "$1" ;;
  esac
}

run_package_tool() {
  case "$(package_manager)" in
    bun) bunx --no-install "$@" ;;
    pnpm) corepack pnpm exec "$@" ;;
    yarn) corepack yarn exec "$@" ;;
    npm) npx --no-install "$@" ;;
  esac
}

install() {
  if [ -f package.json ]; then
    case "$(package_manager)" in
      bun) bun install --frozen-lockfile ;;
      pnpm) corepack pnpm install --frozen-lockfile ;;
      yarn) corepack yarn install --immutable ;;
      npm) npm ci ;;
    esac
  fi
  if [ -f Cargo.toml ]; then cargo fetch --locked; fi
  if has_python; then python -m venv .venv; .venv/bin/python -m pip install --disable-pip-version-check --quiet ruff; fi
  if [ -f requirements.txt ]; then .venv/bin/python -m pip install --disable-pip-version-check -r requirements.txt; fi
  if [ -f requirements-dev.txt ]; then .venv/bin/python -m pip install --disable-pip-version-check -r requirements-dev.txt; fi
}

rust_component() {
  local component="$1"
  if command -v rustup >/dev/null 2>&1; then
    local toolchain="${RUSTUP_TOOLCHAIN:-$(rustup show active-toolchain | awk '{print $1}')}"
    rustup component add --toolchain "$toolchain" "$component" >/dev/null
  fi
}

has_rust_target() {
  local kind="$1"
  cargo metadata --no-deps --format-version 1 |
    jq -e --arg kind "$kind" 'any(.packages[].targets[]; (.kind | index($kind)) != null)' >/dev/null
}

format() {
  if has_script format:check; then run_script format:check
  elif has_javascript; then run_package_tool prettier --check .
  else echo "Skipping JavaScript/TypeScript formatting (no formatter script or source found)"; fi
  if [ -f Cargo.toml ]; then rust_component rustfmt; cargo fmt --check; fi
  if has_python && command -v ruff >/dev/null 2>&1; then ruff format --check .; fi
}

lint() {
  if has_script lint; then run_script lint
  elif has_javascript; then run_package_tool eslint .
  else echo "Skipping JavaScript/TypeScript lint (no lint script or source found)"; fi
  if [ -f Cargo.toml ]; then rust_component clippy; cargo clippy --all-targets -- -D warnings; fi
  if has_python && command -v ruff >/dev/null 2>&1; then ruff check .; fi
}

type_check() {
  if has_script type-check; then run_script type-check
  elif has_script typecheck; then run_script typecheck
  else echo "Skipping type-check (script not defined)"; fi
  if [ -f Cargo.toml ]; then cargo check; fi
  if has_python && command -v python >/dev/null 2>&1; then
    py_dirs=()
    for dir in tests src scripts; do [ -d "$dir" ] && py_dirs+=("$dir"); done
    if [ "${#py_dirs[@]}" -gt 0 ]; then python -m compileall -q "${py_dirs[@]}"; fi
  fi
}

build() {
  run_script build
  if [ -f Cargo.toml ]; then cargo build --all-targets --all-features; fi
}

unit() {
  # Bun repositories should expose test scripts backed by Bun's native runner.
  # Specialized repositories may keep their native runner (for example Matchstick or Hardhat).
  if has_script test:unit; then
    run_script test:unit
  elif has_script test:coverage; then
    run_script test:coverage
  elif has_script test && ! has_script test:integration; then
    run_script test
  else
    echo "Skipping JavaScript/TypeScript unit tests (script not defined)"
  fi
  if [ -f Cargo.toml ]; then
    if has_rust_target lib; then cargo test --lib --all-features
    elif has_rust_target bin; then cargo test --bins --all-features
    else echo "Skipping Rust unit tests (no library or binary target)"; fi
  fi
  if [ -d tests/unit ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests/unit --cov --cov-report=term-missing --cov-fail-under="${PYTHON_COVERAGE_MIN:-80}"
  elif [ -d tests ] && [ ! -d tests/integration ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests --cov --cov-report=term-missing --cov-fail-under="${PYTHON_COVERAGE_MIN:-80}"
  else
    echo "Skipping Python unit tests (no unit suite detected)"
  fi
}

integration() {
  if has_script test:integration; then
    run_script test:integration
  else
    echo "Skipping JavaScript/TypeScript integration tests (script not defined)"
  fi
  if [ -f Cargo.toml ] && [ -d tests ]; then cargo test --tests --all-features; fi
  if [ -d tests/integration ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests/integration
  else
    echo "Skipping Python integration tests (tests/integration not found)"
  fi
}

e2e() {
  if has_script test:e2e; then
    run_script test:e2e
  elif has_script e2e; then
    run_script e2e
  else
    echo "Skipping JavaScript/TypeScript E2E tests (script not defined)"
  fi
  if [ -d tests/e2e ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests/e2e
  else
    echo "Skipping Python E2E tests (tests/e2e not found)"
  fi
}

smoke() {
  if has_script test:smoke; then
    run_script test:smoke
  elif has_script smoke; then
    run_script smoke
  else
    echo "Skipping JavaScript/TypeScript smoke tests (script not defined)"
  fi
  if [ -d tests/smoke ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests/smoke
  else
    echo "Skipping Python smoke tests (tests/smoke not found)"
  fi
}

case "${1:-}" in
  install|format|lint|type_check|build|unit|integration|e2e|smoke) "$1" ;;
  *) echo "usage: $0 {install|format|lint|type_check|build|unit|integration|e2e|smoke}" >&2; exit 2 ;;
esac
