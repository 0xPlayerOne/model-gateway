#!/usr/bin/env bash
set -euo pipefail

source="https://github.com/0xPlayerOne/template-repo.git"
ref="main"
protection=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --source) source="${2:?missing source path or URL}"; shift 2 ;;
    --ref) ref="${2:?missing ref}"; shift 2 ;;
    --protection) protection=true; shift ;;
    -h|--help)
      printf '%s\n' 'Usage: init-repo.sh [--source PATH_OR_URL] [--ref REF] [--protection]'
      exit 0
      ;;
    *)
      printf 'Unknown option: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

bash .github/scripts/sync-template.sh --source "$source" --ref "$ref" --apply
bash .github/scripts/bootstrap.sh

if [ "$protection" = true ]; then
  remote="$(git remote get-url origin 2>/dev/null || true)"
  repo="$(printf '%s\n' "$remote" | sed -E 's#.*github.com[:/]([^/]+/[^/.]+)(\.git)?$#\1#')"
  [ -n "$repo" ] || { echo 'Could not determine GitHub repository from origin' >&2; exit 1; }
  bash .github/scripts/sync-protection.sh --repo "$repo" --apply
fi
