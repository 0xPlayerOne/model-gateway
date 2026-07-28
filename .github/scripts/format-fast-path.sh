#!/usr/bin/env bash
set -euo pipefail

# Bootstrap only the formatter for Bun projects whose repository-owned format
# script directly invokes Prettier. The workflow keeps a full-install fallback
# because formatter configs may import arbitrary repository dependencies.

set_output() {
  printf '%s=%s\n' "$1" "$2" >> "${GITHUB_OUTPUT:-/dev/null}"
}

set_output hit false

[ -f package.json ] || exit 0
[ -f bun.lock ] || [ -f bun.lockb ] || exit 0

project_profile="$(node <<'NODE'
const fs = require('node:fs');
const packageJson = JSON.parse(fs.readFileSync('package.json', 'utf8'));
process.stdout.write(Array.isArray(packageJson.workspaces) ||
  (packageJson.workspaces && typeof packageJson.workspaces === 'object') ? 'workspace' : 'standalone');
NODE
)"
lock_file='bun.lock'
[ -f "$lock_file" ] || lock_file='bun.lockb'
lock_bytes="$(wc -c < "$lock_file")"
# A temporary npm formatter install has fixed overhead. For small standalone
# projects, the normal Bun install is usually faster; workspace graphs and
# larger lockfiles still benefit materially from avoiding their full tree.
if [ "$project_profile" = standalone ] && [ "$lock_bytes" -le 524288 ]; then
  exit 0
fi

format_script="$(node <<'NODE'
const fs = require('node:fs');
const packageJson = JSON.parse(fs.readFileSync('package.json', 'utf8'));
console.log(packageJson.scripts?.['format:check'] || packageJson.scripts?.format || '');
NODE
)"
if ! grep -Eiq '(^|[[:space:];&|])prettier([[:space:]]|$)' <<<"$format_script" ||
  grep -Eiq '(^|[[:space:];&|])turbo([[:space:]]|$)' <<<"$format_script"; then
  exit 0
fi

prettier_version="$(node <<'NODE'
const fs = require('node:fs');
const packageJson = JSON.parse(fs.readFileSync('package.json', 'utf8'));
for (const group of [packageJson.devDependencies, packageJson.dependencies, packageJson.optionalDependencies]) {
  const value = group?.prettier;
  const match = typeof value === 'string' && value.match(/(\d+\.\d+\.\d+)/);
  if (match) {
    console.log(match[1]);
    break;
  }
}
NODE
)"
[ -n "$prettier_version" ] || exit 0

tool_home="${RUNNER_TEMP:-/tmp}/repo-foundry-format"
install_log="$(mktemp)"
cleanup() { rm -f "$install_log"; }
trap cleanup EXIT

if ! npm install --prefix "$tool_home" --no-save --ignore-scripts --no-audit --no-fund \
  "prettier@${prettier_version}" >"$install_log" 2>&1; then
  echo 'Format Probe: formatter bootstrap unavailable; using full install'
  exit 0
fi

tool_bin="$tool_home/node_modules/.bin"
if [ ! -x "$tool_bin/prettier" ]; then
  echo 'Format Probe: formatter binary unavailable; using full install'
  exit 0
fi

echo "$tool_bin" >> "${GITHUB_PATH:-/dev/null}"
printf 'REPO_FOUNDRY_FORMAT_FAST=true\n' >> "${GITHUB_ENV:-/dev/null}"
echo "Format Probe: using Prettier ${prettier_version} without project install"
set_output hit true
