#!/usr/bin/env bash
set -euo pipefail

source .github/scripts/changed-files.sh

has_javascript_dependencies() {
  [ -f bun.lock ] || [ -f bun.lockb ] || [ -f pnpm-lock.yaml ] ||
    [ -f yarn.lock ] || [ -f package-lock.json ] ||
    { [ -f package.json ] && node -e 'const p=require("./package.json"); const groups=[p.dependencies,p.devDependencies,p.optionalDependencies,p.peerDependencies]; process.exit(groups.some((g)=>g && Object.keys(g).length) ? 0 : 1)' 2>/dev/null; }
}

has_dependency_manifest() {
  has_javascript_dependencies || [ -f Cargo.toml ] || [ -f requirements.txt ] ||
    [ -f requirements-dev.txt ] || [ -f pyproject.toml ]
}

should_run() {
  if repo_foundry_governance_only; then
    printf '%s\n' 'applicable=false'
    return 0
  fi
  if repo_foundry_pr_dependencies_unchanged "${1:-all}"; then
    printf '%s\n' 'applicable=false'
    return 0
  fi
  case "${1:-all}" in
    javascript) has_javascript_dependencies || return 1 ;;
    rust) [ -f Cargo.toml ] || return 1 ;;
    python) [ -f requirements.txt ] || [ -f requirements-dev.txt ] || [ -f pyproject.toml ] || return 1 ;;
    all) has_dependency_manifest || return 1 ;;
    *) echo "unknown ecosystem: ${1:-}" >&2; return 2 ;;
  esac
  printf '%s\n' 'applicable=true'
}

if [ "${1:-audit}" = should_run ]; then
  if should_run "${2:-all}"; then
    exit 0
  else
    status=$?
    [ "$status" -eq 1 ] && printf '%s\n' 'applicable=false' && exit 0
    exit "$status"
  fi
fi

if [ "${1:-}" = profile ]; then
  for ecosystem in javascript rust python; do
    if should_run "$ecosystem"; then
      printf '%s=true\n' "$ecosystem"
    else
      status=$?
      [ "$status" -eq 1 ] && printf '%s=false\n' "$ecosystem" && continue
      exit "$status"
    fi
  done
  exit 0
fi

mode="${1:-audit}"
if [ "$mode" = audit ] && [ -n "${2:-}" ]; then mode="$2"; fi
case "$mode" in
  audit|all|javascript|rust|python) ;;
  *)
    echo "usage: $0 [audit|should_run] [javascript|rust|python|all]" >&2
    exit 2
    ;;
esac

audits=0

wait_for_parallel() {
  local status=0 pid
  for pid in "$@"; do
    if ! wait "$pid"; then status=1; fi
  done
  return "$status"
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
  else echo none
  fi
}

audit_javascript() {
  if ! has_javascript_dependencies; then
    echo "Skipping JavaScript/TypeScript audit (dependency inputs not found)"
    return
  fi
  audit_args=(--audit-level=high)
  if [ -f .github/security-audit-allowlist.txt ]; then
    while IFS= read -r advisory; do
      [[ -z "$advisory" || "$advisory" == \#* ]] && continue
      audit_args+=(--ignore "$advisory")
    done < .github/security-audit-allowlist.txt
  fi
  case "$(package_manager)" in
    bun) audits=$((audits + 1)); bun audit "${audit_args[@]}" ;;
    pnpm) audits=$((audits + 1)); corepack pnpm audit --audit-level high ;;
    yarn) audits=$((audits + 1)); corepack yarn npm audit --all --recursive ;;
    npm) audits=$((audits + 1)); npm audit --audit-level=high ;;
  esac
}

audit_rust() {
  if [ -f Cargo.toml ]; then
    audits=$((audits + 1))
    if ! command -v cargo-audit >/dev/null 2>&1; then cargo install cargo-audit --locked --quiet; fi
    cargo audit
  else
    echo "Skipping Rust audit (Cargo.toml not found)"
  fi
}

audit_python() {
  if [ -f requirements.txt ] || [ -f requirements-dev.txt ] || [ -f pyproject.toml ]; then
    audits=$((audits + 1))
    if command -v uv >/dev/null 2>&1; then
      local uv_audit_cache="${REPO_FOUNDRY_UV_AUDIT_CACHE:-$HOME/.cache/uv-audit}"
      pip_audit() { UV_CACHE_DIR="$uv_audit_cache" uv tool run --from pip-audit pip-audit "$@"; }
    else
      python -m pip install --disable-pip-version-check --quiet pip-audit
      pip_audit() { python -m pip_audit "$@"; }
    fi
    pids=()
    if [ -f requirements.txt ]; then pip_audit -r requirements.txt & pids+=("$!"); fi
    if [ -f requirements-dev.txt ]; then pip_audit -r requirements-dev.txt & pids+=("$!"); fi
    if [ -f pyproject.toml ] && [ ! -f requirements.txt ] && [ ! -f requirements-dev.txt ]; then
      pip_audit & pids+=("$!")
    fi
    wait_for_parallel "${pids[@]}"
  else
    echo "Skipping Python audit (Python dependency manifest not found)"
  fi
}

case "$mode" in
  audit|all)
    run_parallel audit_javascript audit_rust audit_python
    ;;
  javascript) audit_javascript ;;
  rust) audit_rust ;;
  python) audit_python ;;
esac

if [ "$audits" -eq 0 ] && ! has_dependency_manifest; then
  echo "No supported dependency manifests found; nothing to audit"
fi

exit 0
