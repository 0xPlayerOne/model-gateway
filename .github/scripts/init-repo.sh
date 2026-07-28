#!/usr/bin/env bash
set -euo pipefail

source="${REPO_FOUNDRY_SOURCE:-https://github.com/${GITHUB_REPOSITORY_OWNER:-OWNER}/code-foundry.git}"
ref="main"
profile="${REPO_FOUNDRY_PROFILE:-auto}"
protection=false
dry_run=false
prune=false
force=false
languages="${REPO_FOUNDRY_LANGUAGES:-auto}"
features="${REPO_FOUNDRY_FEATURES:-all}"
package_manager="${REPO_FOUNDRY_PACKAGE_MANAGER:-auto}"
runtime_repository="${REPO_FOUNDRY_RUNTIME_REPOSITORY:-}"
runtime_ref="${REPO_FOUNDRY_RUNTIME_REF:-}"
bootstrap=true
release_type="${REPO_FOUNDRY_RELEASE_TYPE:-auto}"
npm_publish="${REPO_FOUNDRY_NPM_PUBLISH:-false}"
prune_standard="${REPO_FOUNDRY_PRUNE_STANDARD:-false}"
license="${REPO_FOUNDRY_LICENSE:-agpl-3.0-or-later}"
license_file="${REPO_FOUNDRY_LICENSE_FILE:-}"
runner="${REPO_FOUNDRY_RUNNER:-ubuntu-latest}"
unit_runner="${REPO_FOUNDRY_UNIT_RUNNER:-ubuntu-slim}"
ci_runner="${REPO_FOUNDRY_CI_RUNNER:-ubuntu-latest}"
test_runner="${REPO_FOUNDRY_TEST_RUNNER:-ubuntu-latest}"
security_runner="${REPO_FOUNDRY_SECURITY_RUNNER:-ubuntu-slim}"
codeql_runner="${REPO_FOUNDRY_CODEQL_RUNNER:-ubuntu-latest}"
pr_runner="${REPO_FOUNDRY_PR_RUNNER:-ubuntu-slim}"
release_runner="${REPO_FOUNDRY_RELEASE_RUNNER:-ubuntu-slim}"
cache_packages="${REPO_FOUNDRY_CACHE_PACKAGES:-auto}"
cache_build="${REPO_FOUNDRY_CACHE_BUILD:-auto}"
coverage_minimum="${REPO_FOUNDRY_COVERAGE_MINIMUM:-80}"
turbo_remote="${REPO_FOUNDRY_TURBO_REMOTE:-auto}"

profile_set=false
languages_set=false
features_set=false
package_manager_set=false
runtime_repository_set=false
runtime_ref_set=false
release_type_set=false
npm_publish_set=false
license_set=false
[ -n "${REPO_FOUNDRY_PROFILE:-}" ] && profile_set=true
[ -n "${REPO_FOUNDRY_LANGUAGES:-}" ] && languages_set=true
[ -n "${REPO_FOUNDRY_FEATURES:-}" ] && features_set=true
[ -n "${REPO_FOUNDRY_PACKAGE_MANAGER:-}" ] && package_manager_set=true
[ -n "${REPO_FOUNDRY_RUNTIME_REPOSITORY:-}" ] && runtime_repository_set=true
[ -n "${REPO_FOUNDRY_RUNTIME_REF:-}" ] && runtime_ref_set=true
[ -n "${REPO_FOUNDRY_RELEASE_TYPE:-}" ] && release_type_set=true
[ -n "${REPO_FOUNDRY_NPM_PUBLISH:-}" ] && npm_publish_set=true
[ -n "${REPO_FOUNDRY_LICENSE:-}" ] && license_set=true
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
  --source PATH_OR_URL       Template source (default: REPO_FOUNDRY_SOURCE or GitHub owner)
  --ref REF                  Template branch or tag (default: main)
  --profile NAME             auto, application, monorepo, or minimal
  --languages LIST           auto or comma-separated: typescript,rust,python,solidity
  --features LIST            all or comma-separated optional features:
                             ci,codeql,security,test,draft-pr,release-pr,release,dependabot
  --package-manager NAME     auto, bun, pnpm, yarn, or npm
  --runtime-repository OWNER/REPO  Reusable workflow runtime repository (auto from source)
  --runtime-ref REF           Reusable workflow runtime tag or branch
  --release-type NAME        auto, node, python, rust, or simple
  --license NAME             agpl-3.0-or-later, mit, preserve, or none
  --license-file PATH        Use an exact custom license file
  --npm-publish              Enable npm publication in the release workflow
  --dry-run                  Preview changes without writing files
  --force                    Replace protected standard docs/templates
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
    --profile) profile="${2:?missing profile}"; profile_set=true; shift 2 ;;
    --languages) languages="${2:?missing language list}"; languages_set=true; shift 2 ;;
    --features) features="${2:?missing feature list}"; features_set=true; shift 2 ;;
    --package-manager) package_manager="${2:?missing package manager}"; package_manager_set=true; shift 2 ;;
    --runtime-repository) runtime_repository="${2:?missing runtime repository}"; runtime_repository_set=true; shift 2 ;;
    --runtime-ref) runtime_ref="${2:?missing runtime ref}"; runtime_ref_set=true; shift 2 ;;
    --release-type) release_type="${2:?missing release type}"; release_type_set=true; shift 2 ;;
    --license) license="${2:?missing license}"; license_set=true; shift 2 ;;
    --license-file) license_file="${2:?missing license file}"; shift 2 ;;
    --npm-publish) npm_publish=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    --force) force=true; shift ;;
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

config_path=.github/code-foundry.yml
if [ -f "$config_path" ]; then
  config_value() {
    awk -F': ' -v key="$1" '$1 == key { value=$2; sub(/[[:space:]]+#.*/, "", value); gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); print value; exit }' "$config_path"
  }
  if [ "$profile_set" = false ]; then profile="$(config_value profile)"; fi
  if [ "$languages_set" = false ]; then languages="$(config_value languages)"; fi
  if [ "$features_set" = false ]; then features="$(config_value features)"; fi
  if [ "$package_manager_set" = false ]; then package_manager="$(config_value package_manager)"; fi
  if [ "$runtime_repository_set" = false ]; then runtime_repository="$(config_value runtime_repository)"; fi
  if [ "$runtime_ref_set" = false ]; then runtime_ref="$(config_value runtime_ref)"; fi
  if [ "$release_type_set" = false ]; then release_type="$(config_value release_type)"; fi
  if [ "$npm_publish_set" = false ]; then npm_publish="$(config_value npm_publish)"; fi
  if [ "$license_set" = false ]; then license="$(config_value license)"; fi
  [ -n "$license_file" ] || license_file="$(config_value license_file)"
  prune_standard="$(config_value prune_standard)"
  [ -n "${REPO_FOUNDRY_RUNNER:-}" ] || runner="$(config_value runner)"
  [ -n "${REPO_FOUNDRY_UNIT_RUNNER:-}" ] || unit_runner="$(config_value unit_runner)"
  [ -n "${REPO_FOUNDRY_CI_RUNNER:-}" ] || ci_runner="$(config_value ci_runner)"
  [ -n "${REPO_FOUNDRY_TEST_RUNNER:-}" ] || test_runner="$(config_value test_runner)"
  [ -n "${REPO_FOUNDRY_SECURITY_RUNNER:-}" ] || security_runner="$(config_value security_runner)"
  [ -n "${REPO_FOUNDRY_CODEQL_RUNNER:-}" ] || codeql_runner="$(config_value codeql_runner)"
  [ -n "${REPO_FOUNDRY_PR_RUNNER:-}" ] || pr_runner="$(config_value pr_runner)"
  [ -n "${REPO_FOUNDRY_RELEASE_RUNNER:-}" ] || release_runner="$(config_value release_runner)"
  [ -n "${REPO_FOUNDRY_CACHE_PACKAGES:-}" ] || cache_packages="$(config_value cache_packages)"
  [ -n "${REPO_FOUNDRY_CACHE_BUILD:-}" ] || cache_build="$(config_value cache_build)"
  [ -n "${REPO_FOUNDRY_COVERAGE_MINIMUM:-}" ] || coverage_minimum="$(config_value coverage_minimum)"
  [ -n "${REPO_FOUNDRY_TURBO_REMOTE:-}" ] || turbo_remote="$(config_value turbo_remote)"
  [ -n "$runner" ] || runner=ubuntu-latest
  [ -n "$unit_runner" ] || unit_runner=ubuntu-slim
  [ -n "$ci_runner" ] || ci_runner="$runner"
  [ -n "$test_runner" ] || test_runner="$runner"
  [ -n "$security_runner" ] || security_runner=ubuntu-slim
  [ -n "$codeql_runner" ] || codeql_runner="$runner"
  [ -n "$pr_runner" ] || pr_runner=ubuntu-slim
  [ -n "$release_runner" ] || release_runner=ubuntu-slim
  [ -n "$cache_packages" ] || cache_packages=auto
  [ -n "$cache_build" ] || cache_build=auto
  [ -n "$coverage_minimum" ] || coverage_minimum=80
  [ -n "$turbo_remote" ] || turbo_remote=auto
  [ -n "$prune_standard" ] || prune_standard=false
  [ "$prune_standard" = true ] && prune=true
fi

case "$package_manager" in
  auto|none|bun|pnpm|yarn|npm) ;;
  *) printf 'Unsupported package manager: %s\n' "$package_manager" >&2; exit 2 ;;
esac
case "$release_type" in
  auto|node|python|rust|simple|none) ;;
  *) printf 'Unsupported release type: %s\n' "$release_type" >&2; exit 2 ;;
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
  --profile "$profile"
  --languages "$languages"
  --features "$features"
  --package-manager "$package_manager"
  --license "$license"
)
if [ -n "$runtime_repository" ]; then sync_args+=(--runtime-repository "$runtime_repository"); fi
if [ -n "$runtime_ref" ]; then sync_args+=(--runtime-ref "$runtime_ref"); fi
if [ -n "$license_file" ]; then sync_args+=(--license-file "$license_file"); fi
if [ "$dry_run" = true ]; then sync_args+=(--check); else sync_args+=(--apply); fi
if [ "$prune" = true ]; then sync_args+=(--prune); fi
if [ "$force" = true ]; then sync_args+=(--force); fi
REPO_FOUNDRY_INIT=true bash "$sync_script" "${sync_args[@]}"

if [ "$dry_run" = true ]; then
  printf '%s\n' 'Dry run complete; no files were changed.'
  exit 0
fi

mkdir -p .github
config_value() {
  awk -F': ' -v key="$1" '$1 == key { value=$2; sub(/[[:space:]]+#.*/, "", value); gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); print value; exit }' .github/code-foundry.yml
}
profile="$(config_value profile 2>/dev/null || true)"
languages="$(config_value languages 2>/dev/null || true)"
features="$(config_value features 2>/dev/null || true)"
package_manager="$(config_value package_manager 2>/dev/null || true)"
runtime_repository="$(config_value runtime_repository 2>/dev/null || true)"
runtime_ref="$(config_value runtime_ref 2>/dev/null || true)"
release_type="$(config_value release_type 2>/dev/null || true)"
npm_publish="$(config_value npm_publish 2>/dev/null || true)"
license="$(config_value license 2>/dev/null || true)"
{
  printf 'version: 1\n'
  printf 'profile: %s\n' "$profile"
  printf 'languages: %s\n' "$languages"
  printf 'features: %s\n' "$features"
  printf 'package_manager: %s\n' "$package_manager"
  printf 'runtime_repository: %s\n' "$runtime_repository"
  printf 'runtime_ref: %s\n' "$runtime_ref"
  printf 'runner: %s\n' "$runner"
  printf 'unit_runner: %s\n' "$unit_runner"
  printf 'ci_runner: %s\n' "$ci_runner"
  printf 'test_runner: %s\n' "$test_runner"
  printf 'security_runner: %s\n' "$security_runner"
  printf 'codeql_runner: %s\n' "$codeql_runner"
  printf 'pr_runner: %s\n' "$pr_runner"
  printf 'release_runner: %s\n' "$release_runner"
  printf 'release_type: %s\n' "$release_type"
  printf 'npm_publish: %s\n' "$npm_publish"
  printf 'license: %s\n' "$license"
  if [ -n "$license_file" ]; then
    printf 'license_file: %s\n' "$license_file"
  fi
  printf 'prune_standard: %s\n' "$prune_standard"
  printf 'cache_packages: %s\n' "$cache_packages"
  printf 'cache_build: %s\n' "$cache_build"
  printf 'coverage_minimum: %s\n' "$coverage_minimum"
  printf 'turbo_remote: %s\n' "$turbo_remote"
} > .github/code-foundry.yml

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
