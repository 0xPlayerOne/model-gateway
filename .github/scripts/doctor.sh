#!/usr/bin/env bash
set -euo pipefail

errors=0

error() {
  printf 'ERROR: %s\n' "$1" >&2
  errors=$((errors + 1))
}

warn() {
  printf 'WARN: %s\n' "$1" >&2
}

if [ ! -f .mise.toml ]; then
  error ".mise.toml is missing"
fi

if [ "$(git config --get core.hooksPath || true)" != ".githooks" ]; then
  warn "Git hooks are not enabled; run bash .github/scripts/bootstrap.sh"
fi

if [ -f package.json ]; then
  node -e 'JSON.parse(require("fs").readFileSync("package.json", "utf8"))'
  lockfiles=0
  for lockfile in bun.lock bun.lockb pnpm-lock.yaml yarn.lock package-lock.json; do
    if [ -f "$lockfile" ]; then lockfiles=$((lockfiles + 1)); fi
  done
  if [ "$lockfiles" -eq 0 ]; then
    error "package.json exists but no supported lockfile was found"
  elif [ "$lockfiles" -gt 1 ]; then
    error "multiple JavaScript lockfiles found; keep one package manager"
  fi
  if node -e 'const p=require("./package.json"); process.exit(p.packageManager ? 0 : 1)' 2>/dev/null; then
    declared="$(node -p 'require("./package.json").packageManager.split("@")[0]')"
    actual=""
    [ -f bun.lock ] || [ -f bun.lockb ] && actual="bun"
    [ -f pnpm-lock.yaml ] && actual="pnpm"
    [ -f yarn.lock ] && actual="yarn"
    [ -f package-lock.json ] && actual="npm"
    [ -n "$actual" ] && [ "$declared" != "$actual" ] && error "packageManager ($declared) does not match $actual lockfile"
  fi
  if git ls-files -- '*.ts' '*.tsx' '*.js' '*.jsx' | grep -q .; then
    if ! node -e 'const p=require("./package.json"); const s=p.scripts||{}; process.exit(s.test||s["test:unit"]||s["test:integration"] ? 0 : 1)' 2>/dev/null; then
      warn "JavaScript/TypeScript sources found but no test or test:unit/test:integration script is defined"
    fi
  fi
fi

if [ -f Cargo.toml ]; then
  command -v cargo >/dev/null 2>&1 || error "Cargo is required for this repository"
  cargo metadata --no-deps --format-version 1 >/dev/null
fi

if [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f requirements-dev.txt ]; then
  command -v python >/dev/null 2>&1 || error "Python is required for this repository"
fi

for workflow in ci.yml codeql.yml security.yml test.yml draft-pr.yml release-pr.yml release.yml; do
  [ -f ".github/workflows/$workflow" ] || error "missing standard workflow: $workflow"
done

for script in ci.sh codeql-languages.sh security.sh doctor.sh bootstrap.sh sync-template.sh init-repo.sh; do
  [ -x ".github/scripts/$script" ] || error "missing executable script: .github/scripts/$script"
done

if [ "$errors" -gt 0 ]; then
  printf '%s\n' "Repository doctor found $errors error(s)." >&2
  exit 1
fi

printf '%s\n' "Repository doctor passed."
