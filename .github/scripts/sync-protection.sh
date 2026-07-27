#!/usr/bin/env bash
set -euo pipefail

branch="main"
repo=""
mode="check"
configured_features="all"
configured_languages="auto"

if [ -f .github/template.yml ]; then
  configured_features="$(awk -F': ' '/^features:/ {print $2; exit}' .github/template.yml)"
  configured_languages="$(awk -F': ' '/^languages:/ {print $2; exit}' .github/template.yml)"
  [ -n "$configured_features" ] || configured_features="all"
  [ -n "$configured_languages" ] || configured_languages="auto"
fi

feature_enabled() {
  [ "$configured_features" = all ] && return 0
  case " $(printf '%s' "$configured_features" | tr ',' ' ') " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

language_enabled() {
  [ "$configured_languages" = auto ] || [ "$configured_languages" = all ] && return 0
  case " $(printf '%s' "$configured_languages" | tr ',' ' ') " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

usage() {
  printf '%s\n' 'Usage: sync-protection.sh --repo OWNER/REPO [--branch main] [--check] [--apply]'
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) repo="${2:?missing OWNER/REPO}"; shift 2 ;;
    --branch) branch="${2:?missing branch}"; shift 2 ;;
    --check) mode="check"; shift ;;
    --apply) mode="apply"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[ -n "$repo" ] || { usage >&2; exit 2; }
command -v gh >/dev/null 2>&1 || { echo "gh is required" >&2; exit 1; }

is_private="$(gh repo view "$repo" --json isPrivate --jq '.isPrivate')"
code_security_status="disabled"
if [ "$is_private" = true ]; then
  code_security_status="$(gh api "repos/$repo" --jq '.security_and_analysis.code_security.status // "disabled"' 2>/dev/null || printf 'disabled')"
fi
contexts=()
if feature_enabled ci; then contexts+=(Format Lint Type-Check Build); fi
if feature_enabled test; then contexts+=(Unit Integration E2E Smoke); fi
if feature_enabled security; then
  contexts+=(Profile 'Dependency Audit (JavaScript)' 'Dependency Audit (Rust)' 'Dependency Audit (Python)')
fi
if feature_enabled codeql; then contexts+=(Detect); fi

if { [ "$is_private" != true ] || [ "$code_security_status" = enabled ]; } && feature_enabled codeql; then
  contexts+=("Analyze (Actions)")
  if language_enabled typescript && git ls-files -- '*.ts' '*.tsx' '*.js' '*.jsx' package.json tsconfig\*.json | grep -q .; then contexts+=("Analyze (TypeScript)"); fi
  if language_enabled python && git ls-files -- '*.py' pyproject.toml requirements\*.txt setup.py ':!.github/**' | grep -q .; then contexts+=("Analyze (Python)"); fi
  if language_enabled rust && git ls-files -- '*.rs' Cargo.toml Cargo.lock | grep -q .; then contexts+=("Analyze (Rust)"); fi
fi

protection="$(gh api "repos/$repo/branches/$branch/protection" 2>/dev/null || true)"
if ! jq -e 'type == "object" and (.required_status_checks | type == "object")' >/dev/null 2>&1 <<< "$protection"; then
  if [ "$mode" = apply ]; then
    echo "Cannot read branch protection for $repo:$branch (repository plan or permissions may not allow it)." >&2
    exit 1
  fi
  existing=""
else
  existing="$(jq -r '.required_status_checks.contexts[]?' <<< "$protection")"
fi
preserved=()
while IFS= read -r context; do
  [ -n "$context" ] || continue
  case "$context" in
    'Slither / Analyze') continue ;; # removed from the standard workflow set
    # Remove both the current concise job names and older workflow-prefixed
    # names before rebuilding the standard set. This prevents stale required
    # checks from remaining required after a repository changes visibility or
    # template version.
    'Dependency Audit'|'Dependency Audit ('*|'Profile'|'Test Profile'|\
    'Format'|'Lint'|'Type-Check'|'Build'|'Unit'|'Integration'|'E2E'|'Smoke'|\
    'Detect'|'Analyze'|'Analyze ('*|\
    'CI / '*|'Test / '*|'Security / '*|'CodeQL / '*) ;;
    *) preserved+=("$context") ;;
  esac
done <<< "$existing"

payload="$(printf '%s\n' "${preserved[@]-}" "${contexts[@]}" | jq -Rsc 'split("\n") | map(select(length > 0)) | unique | {strict: true, contexts: .}')"

if [ "$mode" = check ]; then
  printf '%s\n' "$payload" | jq .
  exit 0
fi

current="$protection"
full_payload="$(jq --argjson checks "$payload" '
  {
    required_status_checks: $checks,
    enforce_admins: .enforce_admins.enabled,
    required_pull_request_reviews: (
      if .required_pull_request_reviews == null then null else {
        dismiss_stale_reviews: .required_pull_request_reviews.dismiss_stale_reviews,
        require_code_owner_reviews: .required_pull_request_reviews.require_code_owner_reviews,
        required_approving_review_count: .required_pull_request_reviews.required_approving_review_count,
        require_last_push_approval: .required_pull_request_reviews.require_last_push_approval
      } end
    ),
    restrictions: (
      if .restrictions == null then null else {
        users: [.restrictions.users[].login],
        teams: [.restrictions.teams[].slug],
        apps: [.restrictions.apps[].slug]
      } end
    ),
    required_linear_history: .required_linear_history.enabled,
    allow_force_pushes: .allow_force_pushes.enabled,
    allow_deletions: .allow_deletions.enabled,
    block_creations: .block_creations.enabled,
    required_conversation_resolution: .required_conversation_resolution.enabled,
    lock_branch: .lock_branch.enabled,
    allow_fork_syncing: .allow_fork_syncing.enabled
  }
' <<< "$current")"

gh api --method PUT "repos/$repo/branches/$branch/protection" \
  --input - <<< "$full_payload" >/dev/null
printf 'Updated required checks for %s:%s\n' "$repo" "$branch"
