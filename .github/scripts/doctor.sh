#!/usr/bin/env bash
set -euo pipefail

errors=0

configured_features="all"
if [ -f .github/template.yml ]; then
  configured_features="$(awk -F': ' '/^features:/ {print $2; exit}' .github/template.yml)"
  [ -n "$configured_features" ] || configured_features="all"
fi

feature_enabled() {
  [ "$configured_features" = all ] && return 0
  case " $(printf '%s' "$configured_features" | tr ',' ' ') " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

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
    if node -e 'const p=require("./package.json"); const groups=[p.dependencies,p.devDependencies,p.optionalDependencies,p.peerDependencies]; process.exit(groups.some((g)=>g && Object.keys(g).length) ? 0 : 1)' 2>/dev/null; then
      error "package.json exists but no supported lockfile was found"
    else
      printf '%s\n' "INFO: package.json has no dependencies; a lockfile is optional"
    fi
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
  if [ -f bunfig.toml ] && node -e 'const p=require("./package.json"); process.exit(p.scripts?.["test:coverage"] ? 0 : 1)' 2>/dev/null; then
    if ! grep -q 'coverageThreshold' bunfig.toml; then
      if [ -x .github/scripts/ci.sh ]; then
        printf '%s\n' "INFO: shared CI enforces the Bun aggregate coverage threshold"
      else
        error "Bun coverage is enabled by test:coverage but no coverage policy is configured"
      fi
    fi
  fi
fi

if [ -f Cargo.toml ]; then
  command -v cargo >/dev/null 2>&1 || error "Cargo is required for this repository"
  cargo metadata --no-deps --format-version 1 >/dev/null
fi

if [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f requirements-dev.txt ]; then
  if ! command -v python >/dev/null 2>&1 && [ ! -x .venv/bin/python ]; then
    error "Python is required for this repository"
  fi
fi

for workflow in ci codeql security test draft-pr release-pr release; do
  if feature_enabled "$workflow"; then
    [ -f ".github/workflows/$workflow.yml" ] || error "missing enabled workflow: $workflow.yml"
  fi
done

for script in ci.sh codeql-languages.sh security.sh doctor.sh bootstrap.sh sync-template.sh init-repo.sh sync-protection.sh sync-codeowners.sh; do
  [ -x ".github/scripts/$script" ] || error "missing executable script: .github/scripts/$script"
done

if [ "$errors" -gt 0 ]; then
  printf '%s\n' "Repository doctor found $errors error(s)." >&2
  exit 1
fi

printf '%s\n' "Repository doctor passed."
