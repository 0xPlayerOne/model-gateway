#!/usr/bin/env bash
set -euo pipefail

source="https://github.com/0xPlayerOne/template-repo.git"
ref="main"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --source) source="${2:?missing source path or URL}"; shift 2 ;;
    --ref) ref="${2:?missing ref}"; shift 2 ;;
    -h|--help)
      printf '%s\n' 'Usage: init-repo.sh [--source PATH_OR_URL] [--ref REF]'
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
