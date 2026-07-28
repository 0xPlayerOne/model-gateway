#!/usr/bin/env bash
set -euo pipefail

mode=check
while [ "$#" -gt 0 ]; do
  case "$1" in
    --check) mode=check; shift ;;
    --apply) mode=apply; shift ;;
    -h|--help)
      printf '%s\n' 'Usage: sync-codeowners.sh [--check|--apply]'
      exit 0
      ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
done

codeowners_file=.github/CODEOWNERS
mkdir -p .github
remote="$(git remote get-url origin 2>/dev/null || true)"
repo="$(printf '%s\n' "$remote" | sed -E 's#.*github.com[:/]([^/]+/[^/.]+)(\.git)?$#\1#')"
owners=()

if [ -n "$repo" ] && command -v gh >/dev/null 2>&1; then
  owner_json="$(gh repo view "$repo" --json owner 2>/dev/null || true)"
  owner_login="$(jq -r '.owner.login // empty' <<< "$owner_json" 2>/dev/null || true)"
  owner_type="$(jq -r '.owner.type // empty' <<< "$owner_json" 2>/dev/null || true)"
  if [ "$owner_type" = User ] && [ -n "$owner_login" ]; then
    owners+=("@$owner_login")
  fi
  admins="$(gh api --paginate "repos/$repo/collaborators?per_page=100" \
    --jq '.[] | select(.permissions.admin == true) | .login' 2>/dev/null || true)"
  while IFS= read -r admin; do
    [ -n "$admin" ] && owners+=("@$admin")
  done <<< "$admins"
fi

if [ "${#owners[@]}" -gt 0 ]; then
  owner_line="$(printf '%s\n' "${owners[@]}" | awk '!seen[$0]++' | paste -sd ' ' -)"
else
  owner_line='@OWNER'
  printf '%s\n' 'GitHub owner/admin discovery unavailable; retaining @OWNER placeholder.' >&2
fi

existing_file=""
if [ -f "$codeowners_file" ]; then
  existing_file="$(mktemp)"
  cp "$codeowners_file" "$existing_file"
fi

if [ "$mode" = check ]; then
  if [ -f "$codeowners_file" ] && grep -q "^\* $owner_line$" "$codeowners_file"; then
    printf '%s\n' 'CODEOWNERS is current.'
  else
    printf 'Would update %s with %s\n' "$codeowners_file" "$owner_line"
  fi
  [ -n "$existing_file" ] && unlink "$existing_file"
  exit 0
fi

tmp_file="$(mktemp)"
{
  printf '%s\n' '# Managed by code-foundry. Repository owners/admins are discovered during initialization.'
  printf '* %s\n' "$owner_line"
  if [ -n "$existing_file" ]; then
    awk '
      /^# Managed by code-foundry\./ { managed=1; next }
      managed && /^\* / { managed=0; next }
      managed { next }
      /^\* @(OWNER|[^[:space:]]+)/ { next }
      { print }
    ' "$existing_file"
  fi
} > "$tmp_file"
mv "$tmp_file" "$codeowners_file"
[ -n "$existing_file" ] && unlink "$existing_file"
printf 'Updated %s with %s\n' "$codeowners_file" "$owner_line"
