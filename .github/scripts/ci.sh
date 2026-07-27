#!/usr/bin/env bash
set -euo pipefail

source .github/scripts/changed-files.sh

if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi

# Build one tracked-file inventory for the detectors below. These functions are
# called repeatedly by parallel workflow jobs, so avoid starting Git for each
# language check.
tracked_files="$(git ls-files)"

has_script() {
  [ -f package.json ] && node -e 'const p=require("./package.json"); process.exit(p.scripts?.[process.argv[1]] ? 0 : 1)' "$1"
}

has_turbo_script() {
  [ -f package.json ] && node -e 'const p=require("./package.json"); const script=p.scripts?.[process.argv[1]] || ""; process.exit(/(^|[\s;&|])turbo(\s|$)/.test(script) ? 0 : 1)' "$1"
}

has_bun_native_coverage() {
  [ -f package.json ] && node -e 'const p=require("./package.json"); process.exit(p.scripts?.["test:coverage"]?.includes("bun test") ? 0 : 1)'
}

has_javascript() {
  printf '%s\n' "$tracked_files" | grep -Eq '(^|/)[^/]+\.(js|jsx|mjs|cjs|ts|tsx|mts|cts)$'
}

has_python() {
  [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f requirements-dev.txt ] || \
    printf '%s\n' "$tracked_files" | awk '!/^\.github\// && /(^|\/)[^\/]+\.py$/ { found=1; exit } END { exit !found }'
}

has_python_dependencies() {
  [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f requirements-dev.txt ] ||
    [ -f Pipfile.lock ] || [ -f poetry.lock ] || [ -f uv.lock ]
}

needs_python_environment() {
  has_python_dependencies || ! command -v ruff >/dev/null 2>&1
}

has_javascript_dependencies() {
  [ -f bun.lock ] || [ -f bun.lockb ] || [ -f pnpm-lock.yaml ] ||
    [ -f yarn.lock ] || [ -f package-lock.json ] || {
      [ -f package.json ] || return 1
      REPO_FOUNDRY_TRACKED_FILES="$tracked_files" node <<'NODE'
const files = (process.env.REPO_FOUNDRY_TRACKED_FILES || '')
  .split('\n')
  .filter((file) => /(^|\/)package\.json$/.test(file));
const groups = ['dependencies', 'devDependencies', 'optionalDependencies', 'peerDependencies'];
const hasDependencies = files.some((file) => {
  try {
    const packageJson = JSON.parse(require('node:fs').readFileSync(file, 'utf8'));
    return groups.some((group) => packageJson[group] && Object.keys(packageJson[group]).length > 0) ||
      Array.isArray(packageJson.workspaces) ||
      (packageJson.workspaces && typeof packageJson.workspaces === 'object');
  } catch {
    return true;
  }
});
process.exit(hasDependencies ? 0 : 1);
NODE
    }
}

has_graph_project() {
  [ -f package.json ] && node -e 'const p=require("./package.json"); process.exit(p.devDependencies?.["@graphprotocol/graph-cli"] ? 0 : 1)'
}

has_browser_dependencies() {
  if [ -f package.json ] && node <<'NODE'
const fs = require('node:fs');
const p = JSON.parse(fs.readFileSync('package.json', 'utf8'));
const groups = [p.dependencies, p.devDependencies, p.optionalDependencies, p.peerDependencies];
const names = groups.flatMap((group) => Object.keys(group || {}));
const scripts = Object.values(p.scripts || {});
process.exit([...names, ...scripts].some((value) => /playwright|cypress|puppeteer/i.test(value)) ? 0 : 1);
NODE
  then
    return 0
  fi

  for manifest in pyproject.toml requirements.txt requirements-dev.txt; do
    if [ -f "$manifest" ] && grep -Eiq 'playwright|cypress|puppeteer' "$manifest"; then
      return 0
    fi
  done
  return 1
}

package_manager() {
  local configured=""
  if [ -f .github/template.yml ]; then
    configured="$(awk -F': ' '/^package_manager:/ {print $2; exit}' .github/template.yml)"
    case "$configured" in
      bun|pnpm|yarn|npm) echo "$configured"; return ;;
    esac
  fi
  if [ -f bun.lock ] || [ -f bun.lockb ]; then echo bun
  elif [ -f pnpm-lock.yaml ]; then echo pnpm
  elif [ -f yarn.lock ]; then echo yarn
  elif [ -f package-lock.json ]; then echo npm
  else echo bun
  fi
}

cargo_run() {
  local command="$1"
  shift
  local -a cargo_args=("$@")
  if [ -f Cargo.lock ]; then
    cargo_args+=(--locked)
  fi
  if [ "${CARGO_NET_OFFLINE:-false}" = true ]; then
    cargo_args+=(--offline)
    cargo "$command" "${cargo_args[@]}"
  else
    cargo "$command" "${cargo_args[@]}"
  fi
}

run_script() {
  if ! has_script "$1"; then echo "Skipping $1 (script not defined)"; return; fi
  if [ "${REPO_FOUNDRY_TURBO_REMOTE_ONLY:-false}" = true ] && has_turbo_script "$1"; then
    case "$(package_manager)" in
      bun) bun run "$1" -- --remote-only ;;
      pnpm) corepack pnpm run "$1" -- --remote-only ;;
      yarn) corepack yarn run "$1" -- --remote-only ;;
      npm) npm run "$1" -- --remote-only ;;
    esac
    return
  fi
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

run_bun_coverage() {
  local log_file coverage_line functions lines minimum
  log_file="$(mktemp)"
  if ! run_script test:coverage 2>&1 | tee "$log_file"; then
    rm -f "$log_file"
    return 1
  fi
  coverage_line="$(grep -E '^All files[[:space:]]*\|' "$log_file" | tail -n 1 || true)"
  if [ -z "$coverage_line" ]; then
    echo "Coverage summary not found in test:coverage output" >&2
    rm -f "$log_file"
    return 1
  fi
  read -r functions lines < <(printf '%s\n' "$coverage_line" | awk -F'|' '{for (i = 1; i <= NF; i++) gsub(/[[:space:]]/, "", $i); if (NF >= 5) print $4, $5; else if (NF >= 3) print $2, $3}')
  minimum="${BUN_COVERAGE_MIN:-80}"
  if ! awk -v functions="$functions" -v lines="$lines" -v minimum="$minimum" \
    'BEGIN { exit !(functions + 0 >= minimum && lines + 0 >= minimum) }'; then
    echo "Coverage below ${minimum}%: functions=${functions}% lines=${lines}%" >&2
    rm -f "$log_file"
    return 1
  fi
  echo "Coverage threshold passed: functions=${functions}% lines=${lines}% (minimum ${minimum}%)"
  rm -f "$log_file"
}

python_coverage_args() {
  if [ -f pyproject.toml ]; then
    python - <<'PY'
import tomllib
from pathlib import Path

config = tomllib.loads(Path("pyproject.toml").read_text())
for source in config.get("tool", {}).get("coverage", {}).get("run", {}).get("source", []):
    print(f"--cov={source}")
PY
  fi
}

install_javascript() {
  if [ -n "${REPO_FOUNDRY_TASK:-}" ] && ! task_uses_javascript "$REPO_FOUNDRY_TASK"; then
    echo "Skipping JavaScript dependency install (no $REPO_FOUNDRY_TASK JavaScript suite)"
    return
  fi
  if [ "${REPO_FOUNDRY_INSTALL_JAVASCRIPT:-true}" = true ] && [ -f package.json ] && has_javascript_dependencies &&
    { [ "${REPO_FOUNDRY_JAVASCRIPT_CACHE_HIT:-false}" != true ] || [ ! -d node_modules ]; }; then
    case "$(package_manager)" in
      bun) bun install --frozen-lockfile ;;
      pnpm) corepack pnpm install --frozen-lockfile --prefer-offline ;;
      yarn) corepack yarn install --immutable ;;
      npm) npm ci --prefer-offline --no-audit --fund=false ;;
    esac
  elif [ -f package.json ] && ! has_javascript_dependencies; then
    echo "Skipping JavaScript dependency install (no dependencies found)"
  elif [ -f package.json ]; then
    echo "Using cached JavaScript dependencies"
  fi
}

install_rust() {
  if [ -n "${REPO_FOUNDRY_TASK:-}" ] && ! task_uses_rust "$REPO_FOUNDRY_TASK"; then
    echo "Skipping Rust dependency install (no $REPO_FOUNDRY_TASK Rust suite)"
    return
  fi
  if [ "${REPO_FOUNDRY_INSTALL_RUST:-true}" = true ] && [ -f Cargo.toml ] && [ "${REPO_FOUNDRY_RUST_CACHE_HIT:-false}" != true ]; then
    cargo_run fetch
  elif [ "${REPO_FOUNDRY_INSTALL_RUST:-true}" = true ] && [ -f Cargo.toml ]; then
    echo "Using cached Rust packages"
  fi
}

install_python() {
  if [ -n "${REPO_FOUNDRY_TASK:-}" ] && ! task_uses_python "$REPO_FOUNDRY_TASK"; then
    echo "Skipping Python dependency install (no $REPO_FOUNDRY_TASK Python suite)"
    return
  fi
  if [ "${REPO_FOUNDRY_INSTALL_PYTHON:-true}" = true ] && has_python &&
    needs_python_environment &&
    { [ "${REPO_FOUNDRY_PYTHON_CACHE_HIT:-false}" != true ] || [ ! -x .venv/bin/python ]; }; then
    install_python_with_uv() {
      if [ -f pyproject.toml ] && [ -f uv.lock ]; then
        uv sync --frozen --python "$(command -v python)"
        return
      fi
      uv venv --python "$(command -v python)" .venv
      local -a python_packages=()
      command -v ruff >/dev/null 2>&1 || python_packages+=(ruff)
      if [ -f requirements.txt ]; then python_packages+=(-r requirements.txt); fi
      if [ -f requirements-dev.txt ]; then python_packages+=(-r requirements-dev.txt); fi
      if [ "${#python_packages[@]}" -gt 0 ]; then
        uv pip install --python .venv/bin/python "${python_packages[@]}"
      fi
    }
    if command -v uv >/dev/null 2>&1 && install_python_with_uv; then
      echo "Installed Python dependencies with uv"
    else
      python -m venv .venv
      local -a python_packages=()
      command -v ruff >/dev/null 2>&1 || python_packages+=(ruff)
      if [ -f requirements.txt ]; then python_packages+=(-r requirements.txt); fi
      if [ -f requirements-dev.txt ]; then python_packages+=(-r requirements-dev.txt); fi
      if [ "${#python_packages[@]}" -gt 0 ]; then
        .venv/bin/python -m pip install --disable-pip-version-check "${python_packages[@]}"
      fi
    fi
  elif [ "${REPO_FOUNDRY_INSTALL_PYTHON:-true}" = true ] && has_python && needs_python_environment; then
    echo "Using cached Python environment"
  elif [ "${REPO_FOUNDRY_INSTALL_PYTHON:-true}" = true ] && has_python; then
    echo "Using shared Python tools; project environment not required"
  fi
}

task_uses_javascript() {
  local task="$1"
  case "$task" in
    unit) has_script test:unit || has_script test:coverage || { has_script test && ! has_script test:integration; } ;;
    integration) has_script test:integration ;;
    e2e) has_script test:e2e || has_script e2e ;;
    smoke) has_script test:smoke || has_script smoke ;;
    *) return 0 ;;
  esac
}

task_uses_rust() {
  local task="$1"
  [ -f Cargo.toml ] || return 1
  case "$task" in
    unit) has_rust_target lib || has_rust_target bin ;;
    integration) [ -d tests ] ;;
    e2e|smoke) return 1 ;;
    *) return 0 ;;
  esac
}

task_uses_python() {
  local task="$1"
  has_python_tests() {
    local directory="$1"
    [ -d "$directory" ] && find "$directory" -type f -name '*.py' -print -quit | grep -q .
  }
  has_python || return 1
  case "$task" in
    unit) has_python_tests tests/unit || { [ ! -d tests/unit ] && [ ! -d tests/integration ] && has_python_tests tests; } ;;
    integration) has_python_tests tests/integration ;;
    e2e) has_python_tests tests/e2e ;;
    smoke) has_python_tests tests/smoke ;;
    *) return 0 ;;
  esac
}

task_profile() {
  local task="${1:?missing test task}"
  local output applicable
  case "$task" in
    unit|integration|e2e|smoke) ;;
    *) echo "unknown test task: $task" >&2; return 2 ;;
  esac
  output="$(should_run "$task")"
  applicable="$(printf '%s\n' "$output" | awk -F= '$1 == "applicable" {print $2; exit}')"
  printf 'applicable=%s\n' "${applicable:-false}"
  if [ "$task" = e2e ]; then
    printf 'browser=%s\n' "$(printf '%s\n' "$output" | awk -F= '$1 == "browser" {print $2; exit}' | awk 'NF {print; found=1} END {if (!found) print "false"}')"
  fi
  if [ "${applicable:-false}" = true ]; then
    task_uses_javascript "$task" && printf 'javascript=true\n' || printf 'javascript=false\n'
    task_uses_python "$task" && printf 'python=true\n' || printf 'python=false\n'
    task_uses_rust "$task" && printf 'rust=true\n' || printf 'rust=false\n'
  else
    printf '%s\n' 'javascript=false' 'python=false' 'rust=false'
  fi
}

install() {
  run_parallel install_javascript install_rust install_python
}

rust_component() {
  local component="$1"
  if command -v rustup >/dev/null 2>&1; then
    local toolchain="${RUSTUP_TOOLCHAIN:-$(rustup show active-toolchain | awk '{print $1}')}"
    if rustup component list --toolchain "$toolchain" 2>/dev/null |
      grep -q "^${component}.*(installed)"; then
      return
    fi
    rustup component add --toolchain "$toolchain" "$component" >/dev/null
  fi
}

has_rust_target() {
  local kind="$1"
  cargo metadata --no-deps --format-version 1 |
    jq -e --arg kind "$kind" 'any(.packages[].targets[]; (.kind | index($kind)) != null)' >/dev/null
}

run_parallel() {
  local status=0 pid task
  local -a pids=()
  for task in "$@"; do
    "$task" &
    pids+=("$!")
  done
  for pid in "${pids[@]}"; do
    if ! wait "$pid"; then status=1; fi
  done
  return "$status"
}

format_javascript() {
  if has_script format:check; then run_script format:check
  elif has_javascript; then run_package_tool prettier --check . --cache --cache-strategy content
  else echo "Skipping JavaScript/TypeScript formatting (no formatter script or source found)"; fi
}

format_rust() {
  if [ -f Cargo.toml ]; then rust_component rustfmt; cargo fmt --check; fi
}

format_python() {
  if has_python && command -v ruff >/dev/null 2>&1; then ruff format --check .; fi
}

format() {
  run_parallel format_javascript format_rust format_python
}

lint_javascript() {
  if has_script lint; then run_script lint
  elif has_javascript; then run_package_tool eslint --cache --cache-strategy content .
  else echo "Skipping JavaScript/TypeScript lint (no lint script or source found)"; fi
}

lint_rust() {
  if [ -f Cargo.toml ]; then
    rust_component clippy
    clippy_args=(--all-targets)
    if [ -f Cargo.lock ]; then clippy_args+=(--locked); fi
    if [ "${CARGO_NET_OFFLINE:-false}" = true ]; then clippy_args+=(--offline); fi
    cargo clippy "${clippy_args[@]}" -- -D warnings
  fi
}

lint_python() {
  if has_python && command -v ruff >/dev/null 2>&1; then ruff check .; fi
}

lint() {
  run_parallel lint_javascript lint_rust lint_python
}

typecheck_javascript() {
  if has_graph_project; then
    echo "Skipping TypeScript type-check (Graph AssemblyScript project uses graph build/codegen)"
  elif has_script type-check; then run_script type-check
  elif has_script typecheck; then run_script typecheck
  else echo "Skipping type-check (script not defined)"; fi
}

typecheck_rust() {
  if [ -f Cargo.toml ]; then cargo_run check; fi
}

typecheck_python() {
  if has_python && command -v python >/dev/null 2>&1; then
    py_dirs=()
    for dir in tests src scripts; do [ -d "$dir" ] && py_dirs+=("$dir"); done
    if [ "${#py_dirs[@]}" -gt 0 ]; then python -m compileall -q "${py_dirs[@]}"; fi
  fi
}

type_check() {
  run_parallel typecheck_javascript typecheck_rust typecheck_python
}

build_javascript() {
  run_script build
}

build_rust() {
  if [ -f Cargo.toml ]; then cargo_run build --all-targets --all-features; fi
}

build() {
  # Some frameworks validate session secrets while statically collecting pages.
  # Keep CI builds deterministic without weakening runtime/deployment validation.
  if [ "${CI:-}" = true ] && [ -z "${NEXTAUTH_SECRET:-}" ]; then
    export NEXTAUTH_SECRET="ci-only-build-secret-not-for-runtime-0123456789"
  fi
  run_parallel build_javascript build_rust
}

unit_javascript() {
  # Bun repositories should expose test scripts backed by Bun's native runner.
  # Specialized repositories may keep their native runner (for example Matchstick or Hardhat).
  if has_script test:unit; then
    run_script test:unit
  elif has_script test:coverage; then
    if [ "$(package_manager)" = bun ] && has_bun_native_coverage; then
      run_bun_coverage
    else
      run_script test:coverage
    fi
  elif has_script test && ! has_script test:integration; then
    run_script test
  else
    echo "Skipping JavaScript/TypeScript unit tests (script not defined)"
  fi
}

unit_rust() {
  if [ -f Cargo.toml ]; then
    if has_rust_target lib; then cargo_run test --lib --all-features
    elif has_rust_target bin; then cargo_run test --bins --all-features
    else echo "Skipping Rust unit tests (no library or binary target)"; fi
  fi
}

unit_python() {
  if [ -d tests/unit ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    coverage_args=()
    while IFS= read -r arg; do [ -n "$arg" ] && coverage_args+=("$arg"); done < <(python_coverage_args)
    [ "${#coverage_args[@]}" -gt 0 ] || coverage_args=(--cov)
    env -u MISE_GITHUB_TOKEN -u MISE_TRUSTED_CONFIG_PATHS -u MISE_YES -u MISE_LOG_LEVEL -u PYTHONHOME PYTHONPATH="$PWD/.github/scripts" python -m pytest -q tests/unit "${coverage_args[@]}" --cov-report=term-missing --cov-fail-under="${PYTHON_COVERAGE_MIN:-80}"
  elif [ -d tests ] && [ ! -d tests/integration ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    coverage_args=()
    while IFS= read -r arg; do [ -n "$arg" ] && coverage_args+=("$arg"); done < <(python_coverage_args)
    [ "${#coverage_args[@]}" -gt 0 ] || coverage_args=(--cov)
    env -u MISE_GITHUB_TOKEN -u MISE_TRUSTED_CONFIG_PATHS -u MISE_YES -u MISE_LOG_LEVEL -u PYTHONHOME PYTHONPATH="$PWD/.github/scripts" python -m pytest -q tests "${coverage_args[@]}" --cov-report=term-missing --cov-fail-under="${PYTHON_COVERAGE_MIN:-80}"
  else
    echo "Skipping Python unit tests (no unit suite detected)"
  fi
}

unit() {
  run_parallel unit_javascript unit_rust unit_python
}

integration_javascript() {
  if has_script test:integration; then
    run_script test:integration
  else
    echo "Skipping JavaScript/TypeScript integration tests (script not defined)"
  fi
}

integration_rust() {
  if [ -f Cargo.toml ] && [ -d tests ]; then cargo_run test --tests --all-features; fi
}

integration_python() {
  if [ -d tests/integration ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests/integration
  else
    echo "Skipping Python integration tests (tests/integration not found)"
  fi
}

integration() {
  run_parallel integration_javascript integration_rust integration_python
}

e2e_javascript() {
  if has_script test:e2e; then
    run_script test:e2e
  elif has_script e2e; then
    run_script e2e
  else
    echo "Skipping JavaScript/TypeScript E2E tests (script not defined)"
  fi
}

e2e_python() {
  if [ -d tests/e2e ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests/e2e
  else
    echo "Skipping Python E2E tests (tests/e2e not found)"
  fi
}

e2e() {
  run_parallel e2e_javascript e2e_python
}

smoke_javascript() {
  if has_script test:smoke; then
    run_script test:smoke
  elif has_script smoke; then
    run_script smoke
  else
    echo "Skipping JavaScript/TypeScript smoke tests (script not defined)"
  fi
}

smoke_python() {
  if [ -d tests/smoke ] && python -c 'import importlib.util; raise SystemExit(importlib.util.find_spec("pytest") is None)' 2>/dev/null; then
    python -m pytest -q tests/smoke
  else
    echo "Skipping Python smoke tests (tests/smoke not found)"
  fi
}

smoke() {
  run_parallel smoke_javascript smoke_python
}

should_run() {
  local task="$1"
  if repo_foundry_governance_only; then
    printf '%s\n' 'applicable=false'
    [ "$task" = e2e ] && printf '%s\n' 'browser=false'
    return 0
  fi
  case "$task" in
    format)
      if has_script format:check || has_javascript || [ -f Cargo.toml ] || has_python; then
        printf '%s\n' 'applicable=true'
      else
        printf '%s\n' 'applicable=false'
      fi
      ;;
    lint)
      if has_script lint || has_javascript || [ -f Cargo.toml ] || has_python; then
        printf '%s\n' 'applicable=true'
      else
        printf '%s\n' 'applicable=false'
      fi
      ;;
    type_check)
      if has_script type-check || has_script typecheck || [ -f Cargo.toml ] || has_python; then
        printf '%s\n' 'applicable=true'
      else
        printf '%s\n' 'applicable=false'
      fi
      ;;
    build)
      if has_script build || [ -f Cargo.toml ]; then
        printf '%s\n' 'applicable=true'
      else
        printf '%s\n' 'applicable=false'
      fi
      ;;
    unit)
      if has_script test:unit || has_script test:coverage || has_script test || [ -f Cargo.toml ] ||
        [ -d tests ] || [ -d test ]; then
        printf '%s\n' 'applicable=true'
      else
        printf '%s\n' 'applicable=false'
      fi
      ;;
    integration)
      if has_script test:integration || [ -f Cargo.toml ] && [ -d tests ] || [ -d tests/integration ]; then
        printf '%s\n' 'applicable=true'
      else
        printf '%s\n' 'applicable=false'
      fi
      ;;
    e2e)
      if has_script test:e2e || has_script e2e || [ -d tests/e2e ]; then
        printf '%s\n' 'applicable=true'
        if has_browser_dependencies; then
          printf '%s\n' 'browser=true'
        else
          printf '%s\n' 'browser=false'
        fi
      else
        printf '%s\n' 'applicable=false'
        printf '%s\n' 'browser=false'
      fi
      ;;
    smoke)
      if has_script test:smoke || has_script smoke || [ -d tests/smoke ]; then
        printf '%s\n' 'applicable=true'
      else
        printf '%s\n' 'applicable=false'
      fi
      ;;
    *)
      echo "unknown CI task: $task" >&2
      return 2
      ;;
  esac
}

case "${1:-}" in
  install|format|lint|type_check|build|unit|integration|e2e|smoke|should_run) "$1" "${2:-}" ;;
  task_profile) task_profile "${2:-}" ;;
  *) echo "usage: $0 {install|format|lint|type_check|build|unit|integration|e2e|smoke|should_run|task_profile}" >&2; exit 2 ;;
esac
