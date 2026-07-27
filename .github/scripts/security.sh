#!/usr/bin/env bash
set -euo pipefail

audits=0

package_manager() {
  if [ -f bun.lock ] || [ -f bun.lockb ]; then echo bun
  elif [ -f pnpm-lock.yaml ]; then echo pnpm
  elif [ -f yarn.lock ]; then echo yarn
  elif [ -f package-lock.json ]; then echo npm
  else echo none
  fi
}

if [ -f package.json ]; then
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
else
  echo "Skipping JavaScript/TypeScript audit (package.json not found)"
fi

if [ -f Cargo.toml ]; then
  audits=$((audits + 1))
  if ! command -v cargo-audit >/dev/null 2>&1; then cargo install cargo-audit --locked --quiet; fi
  cargo audit
else
  echo "Skipping Rust audit (Cargo.toml not found)"
fi

if [ -f requirements.txt ] || [ -f requirements-dev.txt ] || [ -f pyproject.toml ]; then
  audits=$((audits + 1))
  python -m pip install --disable-pip-version-check --quiet pip-audit
  if [ -f requirements.txt ]; then python -m pip_audit -r requirements.txt; fi
  if [ -f requirements-dev.txt ]; then python -m pip_audit -r requirements-dev.txt; fi
  if [ -f pyproject.toml ] && [ ! -f requirements.txt ] && [ ! -f requirements-dev.txt ]; then python -m pip_audit; fi
else
  echo "Skipping Python audit (Python dependency manifest not found)"
fi

if [ "$audits" -eq 0 ]; then echo "No supported dependency manifests found; nothing to audit"; fi
