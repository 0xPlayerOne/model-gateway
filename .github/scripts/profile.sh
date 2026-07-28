#!/usr/bin/env bash
set -euo pipefail

# Resolve repository settings with this precedence:
# explicit REPO_FOUNDRY_* values (CLI callers can export them), then
# .github/code-foundry.yml, then detected defaults. A legacy
# .github/template.yml is accepted during migration.

root="${REPO_FOUNDRY_ROOT:-$PWD}"
command="detect"
requested_key=""

usage() {
  cat <<'EOF'
Usage: profile.sh [detect|env|get KEY] [--root PATH]

Commands:
  detect       Print the resolved profile as key=value lines (default)
  env          Print REPO_FOUNDRY_* assignments
  get KEY      Print one resolved profile value
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    detect|env) command="$1"; shift ;;
    get) command=get; requested_key="${2:?missing profile key}"; shift 2 ;;
    --root) root="${2:?missing root path}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done

cd "$root"
template_file=.github/code-foundry.yml
[ -f "$template_file" ] || template_file=.github/template.yml

config_value() {
  local key="$1"
  [ -f "$template_file" ] || return 0
  awk -F': *' -v key="$key" '$1 == key { value=substr($0, index($0, ":") + 1); sub(/[[:space:]]+#.*/, "", value); print value; exit }' "$template_file" |
    sed -e 's/^ *//' -e 's/ *$//' -e 's/^['"'"'"]//' -e 's/['"'"'"]$//'
}

env_value() {
  local key="$1" env_key
  env_key="REPO_FOUNDRY_$(printf '%s' "$key" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!env_key+x}" ] && [ -n "${!env_key}" ]; then
    printf '%s\n' "${!env_key}"
  fi
}

raw_value() {
  local key="$1" default="${2:-}" value
  value="$(env_value "$key" || true)"
  [ -n "$value" ] || value="$(config_value "$key" || true)"
  [ -n "$value" ] && printf '%s\n' "$value" || printf '%s\n' "$default"
}

has_source() {
  find . -path './.git' -prune -o -type f -print |
    awk -v pattern="(^|/)[^/]+\\.($1)$" '$0 ~ pattern { found=1; exit } END { exit !found }'
}

detect_languages() {
  local detected=()
  if [ -f package.json ] || has_source 'js|jsx|mjs|cjs|ts|tsx|mts|cts' ||
    [ -f bun.lock ] || [ -f bun.lockb ] || [ -f pnpm-lock.yaml ] ||
    [ -f yarn.lock ] || [ -f package-lock.json ]; then
    detected+=(typescript)
  fi
  if [ -f Cargo.toml ] || has_source 'rs'; then detected+=(rust); fi
  if [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f requirements-dev.txt ] || has_source 'py'; then
    detected+=(python)
  fi
  if find . -path './.git' -prune -o -type f -name '*.sol' -print -quit | grep -q .; then
    detected+=(solidity)
  fi
  (IFS=,; printf '%s\n' "${detected[*]:-none}")
}

detect_package_manager() {
  if [ -f bun.lock ] || [ -f bun.lockb ]; then echo bun
  elif [ -f pnpm-lock.yaml ]; then echo pnpm
  elif [ -f yarn.lock ]; then echo yarn
  elif [ -f package-lock.json ]; then echo npm
  elif [ -f package.json ]; then echo bun
  else echo none
  fi
}

detect_profile() {
  if [ -f turbo.json ] || [ -f pnpm-workspace.yaml ] ||
    ( [ -f package.json ] && node -e 'const p=require("./package.json"); process.exit(Array.isArray(p.workspaces) || (p.workspaces && typeof p.workspaces === "object") ? 0 : 1)' 2>/dev/null ); then
    echo monorepo
  elif [ -f package.json ] || [ -f pyproject.toml ] || [ -f Cargo.toml ]; then
    echo application
  else
    echo minimal
  fi
}

resolve_languages() {
  local value
  value="$(raw_value languages auto)"
  [ "$value" = auto ] && value="$(detect_languages)"
  printf '%s\n' "$value"
}

resolve_profile() {
  local value
  value="$(raw_value profile auto)"
  [ "$value" = auto ] && value="$(detect_profile)"
  printf '%s\n' "$value"
}

resolve_package_manager() {
  local value
  value="$(raw_value package_manager auto)"
  [ "$value" = auto ] && value="$(detect_package_manager)"
  printf '%s\n' "$value"
}

resolve_release_type() {
  local value
  value="$(raw_value release_type auto)"
  if [ "$value" = auto ]; then
    if [ -f package.json ]; then value=node
    elif [ -f pyproject.toml ]; then value=python
    elif [ -f Cargo.toml ]; then value=rust
    elif [ -f version.txt ]; then value=simple
    else value=none
    fi
  fi
  printf '%s\n' "$value"
}

resolve_npm_publish() {
  local value release_type
  value="$(raw_value npm_publish false)"
  release_type="$(resolve_release_type)"
  [ "$release_type" = node ] || value=false
  printf '%s\n' "$value"
}

detect() {
  local profile languages package_manager release_type npm_publish features runner unit_runner
  profile="$(resolve_profile)"
  languages="$(resolve_languages)"
  package_manager="$(resolve_package_manager)"
  release_type="$(resolve_release_type)"
  npm_publish="$(resolve_npm_publish)"
  features="$(raw_value features all)"
  runner="$(raw_value runner ubuntu-latest)"
  unit_runner="$(raw_value unit_runner ubuntu-slim)"
  printf 'profile=%s\n' "$profile"
  printf 'languages=%s\n' "$languages"
  printf 'features=%s\n' "$features"
  printf 'package_manager=%s\n' "$package_manager"
  printf 'release_type=%s\n' "$release_type"
  printf 'npm_publish=%s\n' "$npm_publish"
  printf 'runner=%s\n' "$runner"
  printf 'unit_runner=%s\n' "$unit_runner"
  printf 'ci_runner=%s\n' "$(raw_value ci_runner "$runner")"
  printf 'test_runner=%s\n' "$(raw_value test_runner "$runner")"
  printf 'security_runner=%s\n' "$(raw_value security_runner ubuntu-slim)"
  printf 'codeql_runner=%s\n' "$(raw_value codeql_runner "$runner")"
  printf 'pr_runner=%s\n' "$(raw_value pr_runner ubuntu-slim)"
  printf 'release_runner=%s\n' "$(raw_value release_runner ubuntu-slim)"
  printf 'cache_packages=%s\n' "$(raw_value cache_packages auto)"
  printf 'cache_build=%s\n' "$(raw_value cache_build auto)"
  printf 'coverage_minimum=%s\n' "$(raw_value coverage_minimum 80)"
  printf 'turbo_remote=%s\n' "$(raw_value turbo_remote auto)"
  printf 'prune_standard=%s\n' "$(raw_value prune_standard false)"
}

case "$command" in
  detect) detect ;;
  get)
    detect | awk -F= -v key="$requested_key" '$1 == key { print substr($0, index($0, "=") + 1); found=1 } END { exit !found }'
    ;;
  env)
    detect | while IFS='=' read -r key value; do
      env_key="REPO_FOUNDRY_$(printf '%s' "$key" | tr '[:lower:]-' '[:upper:]_')"
      printf '%s=%q\n' "$env_key" "$value"
    done
    ;;
esac
