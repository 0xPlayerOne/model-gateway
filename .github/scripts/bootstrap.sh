#!/usr/bin/env bash
set -euo pipefail

git config core.hooksPath .githooks

if command -v mise >/dev/null 2>&1; then
  mise install
else
  printf '%s\n' "mise is not installed; install it from https://mise.jdx.dev/ and rerun this script." >&2
  exit 1
fi

bash .github/scripts/doctor.sh
