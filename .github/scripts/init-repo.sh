#!/usr/bin/env bash
set -euo pipefail

source="https://github.com/0xPlayerOne/repo-foundry.git"
ref="main"
protection=false
dry_run=false
prune=false
languages="auto"
features="all"
package_manager="auto"
bootstrap=true
tool_dir=""

cleanup() {
  if [ -n "$tool_dir" ]; then rm -rf "$tool_dir"; fi
}
trap cleanup EXIT

usage() {
  cat <<'EOF'
Usage: init-repo.sh [options]

Initialize or synchronize a repository from the shared baseline.

Options:
  --source PATH_OR_URL       Template source (default: 0xPlayerOne/template-repo)
  --ref REF                  Template branch or tag (default: main)
  --languages LIST           auto or comma-separated: typescript,rust,python,solidity
  --features LIST            all or comma-separated optional features:
                             ci,codeql,security,test,draft-pr,release-pr,release,dependabot
  --package-manager NAME     auto, bun, pnpm, yarn, or npm
  --dry-run                  Preview changes without writing files
  --prune                    Remove disabled standard workflows (never custom workflows)
  --protection               Synchronize main branch required checks
  --no-bootstrap             Skip mise/hooks/doctor bootstrap after init
  -h, --help                 Show this help

Examples:
  bash .github/scripts/init-repo.sh --languages typescript,python
  bash .github/scripts/init-repo.sh --languages rust --features ci,codeql,security,test
  bash .github/scripts/init-repo.sh --features all --dry-run
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --source) source="${2:?missing source path or URL}"; shift 2 ;;
    --ref) ref="${2:?missing ref}"; shift 2 ;;
    --languages) languages="${2:?missing language list}"; shift 2 ;;
    --features) features="${2:?missing feature list}"; shift 2 ;;
    --package-manager) package_manager="${2:?missing package manager}"; shift 2 ;;
    --dry-run) dry_run=true; shift ;;
    --prune) prune=true; shift ;;
    --protection) protection=true; shift ;;
    --no-bootstrap) bootstrap=false; shift ;;
    -h|--help) usage; exit 0 ;;
    *)
      printf 'Unknown option: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

case "$package_manager" in
  auto|bun|pnpm|yarn|npm) ;;
  *) printf 'Unsupported package manager: %s\n' "$package_manager" >&2; exit 2 ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" 2>/dev/null && pwd || true)"
if [ -f "$script_dir/sync-template.sh" ]; then
  sync_script="$script_dir/sync-template.sh"
elif [ -d "$source" ] && [ -f "$source/.github/scripts/sync-template.sh" ]; then
  sync_script="$source/.github/scripts/sync-template.sh"
else
  command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 1; }
  tool_dir="$(mktemp -d)"
  git clone --quiet --depth 1 --branch "$ref" "$source" "$tool_dir/template-repo"
  sync_script="$tool_dir/template-repo/.github/scripts/sync-template.sh"
fi

sync_args=(
  --source "$source"
  --ref "$ref"
  --languages "$languages"
  --features "$features"
)
if [ "$dry_run" = true ]; then sync_args+=(--check); else sync_args+=(--apply); fi
if [ "$prune" = true ]; then sync_args+=(--prune); fi
bash "$sync_script" "${sync_args[@]}"

if [ "$dry_run" = true ]; then
  printf '%s\n' 'Dry run complete; no files were changed.'
  exit 0
fi

mkdir -p .github
cat > .github/template.yml <<EOF
version: 1
languages: $languages
features: $features
package_manager: $package_manager
EOF

if [ "$bootstrap" = false ]; then
  printf '%s\n' 'Bootstrap skipped.'
  exit 0
fi

bash .github/scripts/bootstrap.sh

if [ "$protection" = true ]; then
  remote="$(git remote get-url origin 2>/dev/null || true)"
  repo="$(printf '%s\n' "$remote" | sed -E 's#.*github.com[:/]([^/]+/[^/.]+)(\.git)?$#\1#')"
  [ -n "$repo" ] || { echo 'Could not determine GitHub repository from origin' >&2; exit 1; }
  bash .github/scripts/sync-protection.sh --repo "$repo" --apply
fi
