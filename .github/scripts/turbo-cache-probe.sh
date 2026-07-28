#!/usr/bin/env bash
set -euo pipefail

# A Turbo remote-cache hit can be resolved from package manifests alone. This
# probe lets CI skip the full dependency install when every executable task is
# already cached. It is deliberately conservative: only Bun projects with a
# pinned Turbo version are eligible, and synthetic dependency-only tasks are
# excluded from the hit decision.

task="${1:?task name required}"

set_output() {
  printf '%s=%s\n' "$1" "$2" >> "${GITHUB_OUTPUT:-/dev/null}"
}

set_output hit false

[ -n "${TURBO_TOKEN:-}" ] || exit 0
[ -n "${TURBO_TEAM:-}" ] || exit 0
[ -f package.json ] || exit 0
[ -f bun.lock ] || [ -f bun.lockb ] || exit 0

turbo_version="$(node <<'NODE'
const fs = require('node:fs');
const packageJson = JSON.parse(fs.readFileSync('package.json', 'utf8'));
const groups = [
  packageJson.devDependencies,
  packageJson.dependencies,
  packageJson.optionalDependencies,
  packageJson.pnpm?.overrides,
];
for (const group of groups) {
  const value = group?.turbo;
  const match = typeof value === 'string' && value.match(/(\d+\.\d+\.\d+)/);
  if (match) {
    console.log(match[1]);
    break;
  }
}
NODE
)"
[ -n "$turbo_version" ] || exit 0

turbo_home="${RUNNER_TEMP:-/tmp}/repo-foundry-turbo"
probe_file="$(mktemp)"
install_file="$(mktemp)"
result_file="$(mktemp)"
cleanup() { rm -f "$probe_file" "$install_file" "$result_file"; }
trap cleanup EXIT

if ! npm install --prefix "$turbo_home" --no-save --ignore-scripts --no-audit --no-fund \
  "turbo@${turbo_version}" >"$install_file" 2>&1; then
  echo "Turbo Probe: CLI unavailable; using full install"
  exit 0
fi

turbo_bin="$turbo_home/node_modules/.bin/turbo"
if [ ! -x "$turbo_bin" ]; then
  echo "Turbo Probe: CLI unavailable; using full install"
  exit 0
fi

if ! "$turbo_bin" run "$task" \
  --dry=json --cache=remote:r --output-logs=none >"$probe_file" 2>&1; then
  echo "Turbo Probe: CLI unavailable; using full install"
  exit 0
fi

node - "$probe_file" >"$result_file" <<'NODE'
const fs = require('node:fs');
const file = process.argv[2];
const text = fs.readFileSync(file, 'utf8');
const start = text.indexOf('{');
if (start < 0) {
  console.log('invalid-json');
  process.exit(0);
}
let report;
try {
  report = JSON.parse(text.slice(start));
} catch {
  console.log('invalid-json');
  process.exit(0);
}
const runnable = (report.tasks || []).filter((entry) =>
  entry.command !== '<NONEXISTENT>' && entry.resolvedTaskDefinition?.cache !== false,
);
const hits = runnable.filter((entry) => entry.cache?.status === 'HIT').length;
const hit = runnable.length > 0 && hits === runnable.length;
console.log(`tasks=${report.tasks?.length || 0} runnable=${runnable.length} hits=${hits} hit=${hit}`);
NODE
probe_result="$(cat "$result_file")"
echo "Turbo Probe: $probe_result"
if ! grep -q 'hit=true' <<<"$probe_result"; then
  echo "Turbo Probe: remote cache incomplete; using full install"
  exit 0
fi

turbo_bin_dir="${turbo_bin%/*}"
echo "$turbo_bin_dir" >> "${GITHUB_PATH:-/dev/null}"
printf 'REPO_FOUNDRY_TURBO_REMOTE_ONLY=true\n' >> "${GITHUB_ENV:-/dev/null}"
set_output hit true
