#!/usr/bin/env bash
set -euo pipefail

git diff --cached --check
changed=$(git diff --cached --name-only)

has_script() {
  [ -f package.json ] && node -e 'const p=require("./package.json"); process.exit(p.scripts?.[process.argv[1]] ? 0 : 1)' "$1"
}

package_manager() {
  if [ -f bun.lock ] || [ -f bun.lockb ]; then echo bun
  elif [ -f pnpm-lock.yaml ]; then echo pnpm
  elif [ -f yarn.lock ]; then echo yarn
  else echo npm
  fi
}

run_lint_staged() {
  if ! has_script lint-staged; then return 1; fi
  case "$(package_manager)" in
    bun) bun run lint-staged ;;
    pnpm) corepack pnpm run lint-staged ;;
    yarn) corepack yarn run lint-staged ;;
    npm) npm run lint-staged ;;
  esac
}

if printf '%s\n' "$changed" | grep -Eq '\.(js|jsx|ts|tsx|json|md|mdx|yml|yaml)$'; then
  if ! run_lint_staged; then
    bash .github/scripts/ci.sh format
    bash .github/scripts/ci.sh lint
  fi
fi

if printf '%s\n' "$changed" | grep -Eq '\.rs$|(^|/)Cargo\.toml$'; then
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
fi

python_files=()
python_config=false
while IFS= read -r file; do
  case "$file" in
    *.py)
      [ -f "$file" ] && python_files+=("$file")
      ;;
    pyproject.toml|requirements.txt|requirements-dev.txt)
      python_config=true
      ;;
  esac
done <<< "$changed"

if [ "$python_config" = true ] || [ "${#python_files[@]}" -gt 0 ]; then
  ruff_targets=(.)
  [ "$python_config" = true ] || ruff_targets=("${python_files[@]}")
  if [ -x .venv/bin/ruff ]; then
    .venv/bin/ruff format --check "${ruff_targets[@]}"
    .venv/bin/ruff check "${ruff_targets[@]}"
  elif command -v ruff >/dev/null 2>&1; then
    ruff format --check "${ruff_targets[@]}"
    ruff check "${ruff_targets[@]}"
  else
    echo "Python files changed but Ruff is not installed; run the template setup first." >&2
    exit 1
  fi
fi
