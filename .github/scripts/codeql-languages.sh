#!/usr/bin/env bash
set -euo pipefail

languages=()
if find .github/workflows -type f \( -name '*.yml' -o -name '*.yaml' \) -print -quit | grep -q .; then languages+=(actions); fi
configured=""
if [ -f .github/template.yml ]; then
  configured="$(awk -F': ' '/^languages:/ {print $2; exit}' .github/template.yml)"
fi

if [ "$configured" = auto ] || [ "$configured" = all ] || [ -z "$configured" ]; then
  if git ls-files -- '*.ts' '*.tsx' '*.js' '*.jsx' 'package.json' 'tsconfig*.json' | grep -q .; then languages+=(javascript-typescript); fi
  if git ls-files -- '*.py' 'pyproject.toml' 'requirements*.txt' 'setup.py' ':!.github/**' | grep -q .; then languages+=(python); fi
  if git ls-files -- '*.rs' 'Cargo.toml' 'Cargo.lock' | grep -q .; then languages+=(rust); fi
else
  case ",$configured," in *,typescript,*) languages+=(javascript-typescript) ;; esac
  case ",$configured," in *,python,*) languages+=(python) ;; esac
  case ",$configured," in *,rust,*) languages+=(rust) ;; esac
fi

json=$(printf '%s\n' "${languages[@]}" | jq -Rsc 'split("\n") | map(select(length > 0) | {language: ., name: (if . == "javascript-typescript" then "TypeScript" elif . == "actions" then "Actions" else (. | ascii_upcase[0:1] + .[1:]) end), "build-mode": "none"})')
printf 'languages=%s\n' "$json" >> "$GITHUB_OUTPUT"
