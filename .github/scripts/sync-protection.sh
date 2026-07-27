#!/usr/bin/env bash
set -euo pipefail

branch="main"
repo=""
mode="check"

usage() {
  printf '%s\n' 'Usage: sync-protection.sh --repo OWNER/REPO [--branch main] [--apply]'
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) repo="${2:?missing OWNER/REPO}"; shift 2 ;;
    --branch) branch="${2:?missing branch}"; shift 2 ;;
    --apply) mode="apply"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[ -n "$repo" ] || { usage >&2; exit 2; }
command -v gh >/dev/null 2>&1 || { echo "gh is required" >&2; exit 1; }

is_private="$(gh repo view "$repo" --json isPrivate --jq '.isPrivate')"
contexts=(Format Lint Type-Check Build Unit Integration E2E Smoke 'Dependency Audit' Detect)

if [ "$is_private" != true ]; then
  contexts+=("Analyze (Actions)")
  if git ls-files -- '*.ts' '*.tsx' '*.js' '*.jsx' package.json tsconfig\*.json | grep -q .; then contexts+=("Analyze (TypeScript)"); fi
  if git ls-files -- '*.py' pyproject.toml requirements\*.txt setup.py | grep -q .; then contexts+=("Analyze (Python)"); fi
  if git ls-files -- '*.rs' Cargo.toml Cargo.lock | grep -q .; then contexts+=("Analyze (Rust)"); fi
fi

existing="$(gh api "repos/$repo/branches/$branch/protection" --jq '.required_status_checks.contexts[]?' 2>/dev/null || true)"
preserved=()
while IFS= read -r context; do
  [ -n "$context" ] || continue
  case "$context" in
    'CI / '*|'Test / '*|'Security / '*|'CodeQL / '*) ;;
    *) preserved+=("$context") ;;
  esac
done <<< "$existing"

payload="$(printf '%s\n' "${preserved[@]}" "${contexts[@]}" | jq -Rsc 'split("\n") | map(select(length > 0)) | unique | {strict: true, contexts: .}')"

if [ "$mode" = check ]; then
  printf '%s\n' "$payload" | jq .
  exit 0
fi

gh api --method PUT "repos/$repo/branches/$branch/protection/required_status_checks" \
  --input - <<< "$payload" >/dev/null
printf 'Updated required checks for %s:%s\n' "$repo" "$branch"
