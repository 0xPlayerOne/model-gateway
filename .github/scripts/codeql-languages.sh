#!/usr/bin/env bash
set -euo pipefail

languages=()
changed_files=""

# Scheduled and manually dispatched scans are always full scans. For pushes
# and pull requests, a shallow commit diff lets the matrix skip analyzers for
# languages untouched by the change while keeping every matrix check visible.
case "${GITHUB_EVENT_NAME:-}" in
  schedule|workflow_dispatch) ;;
  *)
    base_sha="${CODEQL_BASE_SHA:-}"
    if [ -n "$base_sha" ] && [ "$base_sha" != "0000000000000000000000000000000000000000" ]; then
      if ! git cat-file -e "$base_sha^{commit}" 2>/dev/null; then
        git fetch --no-tags --filter=blob:none --depth=1 origin "$base_sha" >/dev/null 2>&1 || true
      fi
      changed_files="$(git diff --name-only "$base_sha" "${GITHUB_SHA:-HEAD}" 2>/dev/null || true)"
    fi
    [ -n "$changed_files" ] || changed_files="$(git diff --name-only HEAD^ HEAD 2>/dev/null || true)"
    ;;
esac

if find .github/workflows -type f \( -name '*.yml' -o -name '*.yaml' \) -print -quit | grep -q .; then languages+=(actions); fi
configured=""
if [ -f .github/template.yml ]; then
  configured="$(awk -F': ' '/^languages:/ {print $2; exit}' .github/template.yml)"
fi

if [ "$configured" = auto ] || [ "$configured" = all ] || [ -z "$configured" ]; then
  tracked_files="$(git ls-files)"
  if printf '%s\n' "$tracked_files" | grep -Eq '(^|/)([^/]+\.(ts|tsx|js|jsx)|package\.json|tsconfig[^/]*\.json)$'; then
    languages+=(javascript-typescript)
  fi
  if printf '%s\n' "$tracked_files" | awk '!/^\.github\// && /(^|\/)([^\/]+\.py|pyproject\.toml|requirements[^\/]*\.txt|setup\.py)$/ { found=1; exit } END { exit !found }'; then
    languages+=(python)
  fi
  if printf '%s\n' "$tracked_files" | grep -Eq '(^|/)([^/]+\.rs|Cargo\.toml|Cargo\.lock)$'; then
    languages+=(rust)
  fi
else
  case ",$configured," in *,typescript,*) languages+=(javascript-typescript) ;; esac
  case ",$configured," in *,python,*) languages+=(python) ;; esac
  case ",$configured," in *,rust,*) languages+=(rust) ;; esac
fi

changed_json="$(printf '%s\n' "$changed_files" | jq -Rsc 'split("\n") | map(select(length > 0))')"
json=$(printf '%s\n' "${languages[@]}" | jq -Rsc --argjson changed "$changed_json" '
  split("\n")
  | map(select(length > 0) | {
      language: .,
      name: (if . == "javascript-typescript" then "TypeScript" elif . == "actions" then "Actions" else (. | ascii_upcase[0:1] + .[1:]) end),
      "build-mode": "none",
      changed: (
        if ($changed | length) == 0 then true
        elif . == "actions" then any($changed[]; test("(^|/)(\\.github/workflows/|action\\.yml$)"))
        elif . == "javascript-typescript" then any($changed[]; test("(^|/)(.*\\.(js|jsx|mjs|cjs|ts|tsx|mts|cts)|package\\.json|tsconfig[^/]*\\.json|((bun|pnpm|yarn|package-lock)\\.lock))$"))
        elif . == "python" then any($changed[]; test("(^|/)(.*\\.py|pyproject\\.toml|requirements[^/]*\\.txt|setup\\.py|Pipfile\\.lock|poetry\\.lock)$"))
        elif . == "rust" then any($changed[]; test("(^|/)(.*\\.rs|Cargo\\.toml|Cargo\\.lock)$"))
        else true
        end
      )
    })
')
printf 'languages=%s\n' "$json" >> "$GITHUB_OUTPUT"

# Expose stable, shell-friendly outputs for explicit analyzer jobs. Keeping
# these separate from the JSON matrix lets workflow-level conditions avoid
# allocating a runner for languages that are absent or unchanged.
for codeql_language in actions javascript-typescript python rust; do
  output_prefix="$codeql_language"
  case "$codeql_language" in
    javascript-typescript) output_prefix="javascript" ;;
  esac

  entry="$(jq -c --arg language "$codeql_language" '.[] | select(.language == $language)' <<< "$json")"
  if [ -n "$entry" ]; then
    printf '%s_available=true\n' "$output_prefix" >> "$GITHUB_OUTPUT"
    printf '%s_changed=%s\n' "$output_prefix" "$(jq -r '.changed' <<< "$entry")" >> "$GITHUB_OUTPUT"
    printf '%s_build_mode=%s\n' "$output_prefix" "$(jq -r '."build-mode"' <<< "$entry")" >> "$GITHUB_OUTPUT"
  else
    printf '%s_available=false\n' "$output_prefix" >> "$GITHUB_OUTPUT"
    printf '%s_changed=false\n' "$output_prefix" >> "$GITHUB_OUTPUT"
    printf '%s_build_mode=none\n' "$output_prefix" >> "$GITHUB_OUTPUT"
  fi
done
