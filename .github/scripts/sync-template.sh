#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: sync-template.sh --source PATH_OR_URL [options]

Synchronize the repository-owned baseline without replacing project-specific
README files, mise tool selections, or additional workflows.

Options:
  --profile NAME    auto, application, monorepo, or minimal
  --languages LIST  auto or comma-separated: typescript,rust,python,solidity
  --features LIST   all or comma-separated standard features
  --package-manager NAME  auto, bun, pnpm, yarn, or npm
  --config PATH  Use a .github/code-foundry.yml configuration file
  --runtime-repository OWNER/REPO  Reusable workflow runtime repository
  --runtime-ref REF  Reusable workflow runtime tag or branch
  --license NAME     preserve, agpl-3.0-or-later, mit, or none
  --license-file PATH  Use an exact custom license file
  --check           Preview changes (default)
  --apply           Apply changes
  --prune           Remove disabled standard workflows
  --force           Replace protected standard docs/templates
EOF
}

source_ref="main"
mode="check"
source=""
config_file="${REPO_FOUNDRY_CONFIG:-}"
temp_dir=""
profile="${REPO_FOUNDRY_PROFILE:-auto}"
languages="${REPO_FOUNDRY_LANGUAGES:-auto}"
features="${REPO_FOUNDRY_FEATURES:-all}"
prune=false
prune_set=false
force=false
languages_set=false
profile_set=false
features_set=false
[ -n "${REPO_FOUNDRY_LANGUAGES:-}" ] && languages_set=true
[ -n "${REPO_FOUNDRY_PROFILE:-}" ] && profile_set=true
[ -n "${REPO_FOUNDRY_FEATURES:-}" ] && features_set=true
package_manager="${REPO_FOUNDRY_PACKAGE_MANAGER:-}"
package_manager_set=false
[ -n "${REPO_FOUNDRY_PACKAGE_MANAGER:-}" ] && package_manager_set=true
runtime_repository="${REPO_FOUNDRY_RUNTIME_REPOSITORY:-}"
runtime_ref="${REPO_FOUNDRY_RUNTIME_REF:-}"
runtime_repository_set=false
[ -n "${REPO_FOUNDRY_RUNTIME_REPOSITORY:-}" ] && runtime_repository_set=true
runtime_ref_set=false
[ -n "${REPO_FOUNDRY_RUNTIME_REF:-}" ] && runtime_ref_set=true
template_ref=""
release_type="${REPO_FOUNDRY_RELEASE_TYPE:-auto}"
npm_publish="${REPO_FOUNDRY_NPM_PUBLISH:-false}"
prune_standard="${REPO_FOUNDRY_PRUNE_STANDARD:-false}"
runner="${REPO_FOUNDRY_RUNNER:-}"
unit_runner="${REPO_FOUNDRY_UNIT_RUNNER:-}"
ci_runner="${REPO_FOUNDRY_CI_RUNNER:-}"
test_runner="${REPO_FOUNDRY_TEST_RUNNER:-}"
security_runner="${REPO_FOUNDRY_SECURITY_RUNNER:-}"
codeql_runner="${REPO_FOUNDRY_CODEQL_RUNNER:-}"
pr_runner="${REPO_FOUNDRY_PR_RUNNER:-}"
release_runner="${REPO_FOUNDRY_RELEASE_RUNNER:-}"
cache_packages="${REPO_FOUNDRY_CACHE_PACKAGES:-}"
cache_build="${REPO_FOUNDRY_CACHE_BUILD:-}"
coverage_minimum="${REPO_FOUNDRY_COVERAGE_MINIMUM:-}"
turbo_remote="${REPO_FOUNDRY_TURBO_REMOTE:-}"
license="${REPO_FOUNDRY_LICENSE:-preserve}"
license_file="${REPO_FOUNDRY_LICENSE_FILE:-}"
release_type_set=false
npm_publish_set=false
license_set=false
license_file_set=false
[ -n "${REPO_FOUNDRY_RELEASE_TYPE:-}" ] && release_type_set=true
[ -n "${REPO_FOUNDRY_NPM_PUBLISH:-}" ] && npm_publish_set=true
[ -n "${REPO_FOUNDRY_PRUNE_STANDARD:-}" ] && prune_set=true
[ -n "${REPO_FOUNDRY_LICENSE:-}" ] && license_set=true
[ -n "${REPO_FOUNDRY_LICENSE_FILE:-}" ] && license_file_set=true
node_version="24.18.0"
bun_version="1.3.14"
python_version="3.13"
rust_version="1.97.1"
uv_version="0.11.32"
ruff_version="0.16.0"
gitignore_backup=""
custom_ignores=""

valid_languages="typescript rust python solidity"
valid_features="ci codeql security test draft-pr release-pr release dependabot"
valid_profiles="auto application monorepo minimal"
valid_release_types="auto node python rust simple none"

contains_word() {
  case " $1 " in *" $2 "*) return 0 ;; *) return 1 ;; esac
}

normalize_csv() {
  printf '%s' "$1" | tr ',' ' ' | awk '{$1=$1; print}'
}

validate_list() {
  local kind="$1" value="$2" valid="$3" item
  [ "$value" = auto ] || [ "$value" = all ] || {
    for item in $(normalize_csv "$value"); do
      contains_word "$valid" "$item" || { echo "Unsupported $kind: $item" >&2; exit 2; }
    done
  }
}

workflow_enabled() {
  local workflow="$1" list
  [ "$features" = all ] && return 0
  list="$(normalize_csv "$features")"
  contains_word "$list" "$workflow"
}

cleanup() {
  if [ -n "$temp_dir" ]; then rm -rf "$temp_dir"; fi
  if [ -n "$gitignore_backup" ]; then rm -f "$gitignore_backup"; fi
  if [ -n "$custom_ignores" ]; then rm -f "$custom_ignores"; fi
}
trap cleanup EXIT

while [ "$#" -gt 0 ]; do
  case "$1" in
    --source) source="${2:?missing source path or URL}"; shift 2 ;;
    --ref) source_ref="${2:?missing ref}"; shift 2 ;;
    --config) config_file="${2:?missing config path}"; shift 2 ;;
    --profile) profile="${2:?missing profile}"; profile_set=true; shift 2 ;;
    --languages) languages="${2:?missing language list}"; languages_set=true; shift 2 ;;
    --features) features="${2:?missing feature list}"; features_set=true; shift 2 ;;
    --package-manager) package_manager="${2:?missing package manager}"; package_manager_set=true; shift 2 ;;
    --runtime-repository) runtime_repository="${2:?missing runtime repository}"; runtime_repository_set=true; shift 2 ;;
    --runtime-ref) runtime_ref="${2:?missing runtime ref}"; runtime_ref_set=true; shift 2 ;;
    --license) license="${2:?missing license}"; license_set=true; shift 2 ;;
    --license-file) license_file="${2:?missing license file}"; license_file_set=true; shift 2 ;;
    --check) mode="check"; shift ;;
    --apply) mode="apply"; shift ;;
    --prune) prune=true; prune_set=true; shift ;;
    --force) force=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[ -n "$source" ] || { usage >&2; exit 2; }
if [ -n "$config_file" ]; then
  [ -f "$config_file" ] || { printf 'Configuration file not found: %s\n' "$config_file" >&2; exit 1; }
  mkdir -p .github
  if [ "$(cd -- "$(dirname -- "$config_file")" && pwd)/$(basename -- "$config_file")" != "$(pwd)/.github/code-foundry.yml" ]; then
    cp "$config_file" .github/code-foundry.yml
  fi
fi
config_path=.github/code-foundry.yml
[ -f "$config_path" ] || config_path=.github/template.yml
if [ -f "$config_path" ]; then
  config_value() {
    awk -F': ' -v key="$1" '$1 == key { value=$2; sub(/[[:space:]]+#.*/, "", value); gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); print value; exit }' "$config_path"
  }
  if [ "$profile_set" = false ]; then
    configured_profile="$(config_value profile)"
    [ -n "$configured_profile" ] && profile="$configured_profile"
  fi
  if [ "$languages_set" = false ]; then
    configured_languages="$(config_value languages)"
    [ -n "$configured_languages" ] && languages="$configured_languages"
  fi
  if [ "$features_set" = false ]; then
    configured_features="$(config_value features)"
    [ -n "$configured_features" ] && features="$configured_features"
  fi
  if [ "$package_manager_set" = false ]; then
    package_manager="$(config_value package_manager)"
  fi
  if [ "$runtime_repository_set" = false ]; then
    runtime_repository="$(config_value runtime_repository)"
  fi
  if [ "$runtime_ref_set" = false ]; then
    runtime_ref="$(config_value runtime_ref)"
  fi
  template_ref="$(config_value template)"
  if [ "$release_type_set" != true ]; then
    configured_release_type="$(config_value release_type)"
    [ -n "$configured_release_type" ] && release_type="$configured_release_type"
  fi
  if [ "$npm_publish_set" != true ]; then
    configured_npm_publish="$(config_value npm_publish)"
    [ -n "$configured_npm_publish" ] && npm_publish="$configured_npm_publish"
  fi
  if [ "$license_set" != true ]; then
    configured_license="$(config_value license)"
    [ -n "$configured_license" ] && license="$configured_license"
  fi
  if [ "$license_file_set" = false ]; then
    license_file="$(config_value license_file)"
  fi
  if [ "$prune_set" = false ]; then
    configured_prune="$(config_value prune_standard)"
    [ -n "$configured_prune" ] && prune_standard="$configured_prune"
  fi
  [ -n "$runner" ] || runner="$(config_value runner)"
  [ -n "$unit_runner" ] || unit_runner="$(config_value unit_runner)"
  [ -n "$ci_runner" ] || ci_runner="$(config_value ci_runner)"
  [ -n "$test_runner" ] || test_runner="$(config_value test_runner)"
  [ -n "$security_runner" ] || security_runner="$(config_value security_runner)"
  [ -n "$codeql_runner" ] || codeql_runner="$(config_value codeql_runner)"
  [ -n "$pr_runner" ] || pr_runner="$(config_value pr_runner)"
  [ -n "$release_runner" ] || release_runner="$(config_value release_runner)"
  [ -n "$cache_packages" ] || cache_packages="$(config_value cache_packages)"
  [ -n "$cache_build" ] || cache_build="$(config_value cache_build)"
  [ -n "$coverage_minimum" ] || coverage_minimum="$(config_value coverage_minimum)"
  [ -n "$turbo_remote" ] || turbo_remote="$(config_value turbo_remote)"
fi
[ -n "$package_manager" ] || package_manager=auto
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
[ "$prune_standard" = true ] || [ "$prune_standard" = false ] || {
  printf 'prune_standard must be true or false: %s\n' "$prune_standard" >&2
  exit 2
}
[ "$prune_standard" = true ] && prune=true
if [ -z "$runtime_repository" ]; then
  source_repository="$source"
  if [ -d "$source" ]; then
    source_repository="$(git -C "$source" remote get-url origin 2>/dev/null || true)"
  fi
  runtime_repository="$(printf '%s\n' "$source_repository" | sed -nE 's#.*github\.com[:/]([^/]+/[^/.]+)(\.git)?$#\1#p')"
fi
[ -n "$runtime_repository" ] || runtime_repository="0xPlayerOne/code-foundry"
case "$runtime_repository" in
  */*) ;;
  *) printf 'Runtime repository must be OWNER/REPO: %s\n' "$runtime_repository" >&2; exit 2 ;;
esac
case "$package_manager" in
  auto|bun|pnpm|yarn|npm) ;;
  *) printf 'Unsupported package manager: %s\n' "$package_manager" >&2; exit 2 ;;
esac
validate_list language "$languages" "$valid_languages"
validate_list feature "$features" "$valid_features"
contains_word "$valid_profiles" "$profile" || {
  printf 'Unsupported profile: %s\n' "$profile" >&2
  exit 2
}
contains_word "$valid_release_types" "$release_type" || {
  printf 'Unsupported release type: %s\n' "$release_type" >&2
  exit 2
}
case "$license" in
  preserve|agpl-3.0-or-later|mit|custom|none) ;;
  *) printf 'Unsupported license: %s\n' "$license" >&2; exit 2 ;;
esac
[ -z "$license_file" ] || license=custom
for configured_runner in "$runner" "$unit_runner" "$ci_runner" "$test_runner" "$security_runner" "$codeql_runner" "$pr_runner" "$release_runner"; do
  case "$configured_runner" in
    ''|*[!A-Za-z0-9._/-]*) printf 'Runner contains unsupported characters: %s\n' "$configured_runner" >&2; exit 2 ;;
  esac
done

if [ -d "$source" ] && [ -f "$source/.github/scripts/sync-template.sh" ]; then
  template_root="$source"
else
  command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 1; }
  temp_dir="$(mktemp -d)"
  git clone --quiet --depth 1 --branch "$source_ref" "$source" "$temp_dir/template-repo"
  template_root="$temp_dir/template-repo"
fi

# Prefer an explicit CLI/environment value, then the target's saved contract,
# then the source template's contract, and finally the stable public runtime.
source_config="$template_root/.github/code-foundry.yml"
[ -f "$source_config" ] || source_config="$template_root/.github/template.yml"
if [ -z "$runtime_ref" ] && [ -f "$source_config" ]; then
  runtime_ref="$(awk -F': ' '/^runtime_ref:/ {print $2; exit}' "$source_config")"
fi
[ -n "$runtime_ref" ] || runtime_ref="v0.20.2"
case "$runtime_ref" in
  ''|*[!A-Za-z0-9._/-]*) printf 'Runtime ref contains unsupported characters: %s\n' "$runtime_ref" >&2; exit 2 ;;
esac

# Resolve the effective profile after the template is available. This gives
# CLI flags and GitHub repository variables priority over template defaults,
# while making automatic detection concrete in the generated profile.
if [ -f "$template_root/.github/scripts/profile.sh" ]; then
  profile_output="$(
    REPO_FOUNDRY_PROFILE="$profile" \
    REPO_FOUNDRY_LANGUAGES="$languages" \
    REPO_FOUNDRY_FEATURES="$features" \
    REPO_FOUNDRY_PACKAGE_MANAGER="$package_manager" \
    REPO_FOUNDRY_RUNTIME_REPOSITORY="$runtime_repository" \
    REPO_FOUNDRY_RUNTIME_REF="$runtime_ref" \
    REPO_FOUNDRY_RELEASE_TYPE="$release_type" \
    REPO_FOUNDRY_NPM_PUBLISH="$npm_publish" \
    bash "$template_root/.github/scripts/profile.sh" detect --root "$PWD"
  )"
  profile="$(printf '%s\n' "$profile_output" | awk -F= '$1 == "profile" {print substr($0, index($0, "=") + 1)}')"
  languages="$(printf '%s\n' "$profile_output" | awk -F= '$1 == "languages" {print substr($0, index($0, "=") + 1)}')"
  package_manager="$(printf '%s\n' "$profile_output" | awk -F= '$1 == "package_manager" {print substr($0, index($0, "=") + 1)}')"
  release_type="$(printf '%s\n' "$profile_output" | awk -F= '$1 == "release_type" {print substr($0, index($0, "=") + 1)}')"
  npm_publish="$(printf '%s\n' "$profile_output" | awk -F= '$1 == "npm_publish" {print substr($0, index($0, "=") + 1)}')"
fi

if [ -f "$template_root/package.json" ]; then
  template_version="$(awk -F'"' '/"version"[[:space:]]*:/ {print $4; exit}' "$template_root/package.json")"
  [ -n "$template_version" ] && template_ref="code-foundry@$template_version"
fi

files=(
  .editorconfig
  .gitattributes
  .gitignore
  release-please-config.json
  .githooks/pre-commit
  AGENTS.md
  LICENSE
  NOTICE
  ruff.toml
  .prettierrc
  .github/CODEOWNERS
  .github/code-foundry.yml.example
  .github/CODE_OF_CONDUCT.md
  .github/CONTRIBUTING.md
  .github/PULL_REQUEST_TEMPLATE.md
  .github/SECURITY.md
  .github/dependabot.yml
  .github/ISSUE_TEMPLATE/bug_report.yml
  .github/ISSUE_TEMPLATE/config.yml
  .github/ISSUE_TEMPLATE/feature_request.yml
  .github/scripts/profile.sh
  .github/scripts/bootstrap.sh
  # Keep the small local hook runner and its changed-file helper; the full
  # CI/security implementations used by Actions are loaded from the runtime.
  .github/scripts/changed-files.sh
  .github/scripts/ci.sh
  .github/scripts/doctor.sh
  .github/scripts/pre-commit.sh
  .github/scripts/sync-template.sh
  .github/scripts/init-repo.sh
  .github/scripts/sync-codeowners.sh
  .github/scripts/sync-protection.sh
  .github/workflows/ci.yml
  .github/workflows/codeql.yml
  .github/workflows/draft-pr.yml
  .github/workflows/release-pr.yml
  .github/workflows/release.yml
  .github/workflows/security.yml
  .github/workflows/test.yml
)

filtered_files=()
for file in "${files[@]}"; do
  case "$file" in
    .github/dependabot.yml) workflow_enabled dependabot && filtered_files+=("$file") ;;
    .github/workflows/ci.yml) workflow_enabled ci && filtered_files+=("$file") ;;
    .github/workflows/codeql.yml) workflow_enabled codeql && filtered_files+=("$file") ;;
    .github/workflows/security.yml) workflow_enabled security && filtered_files+=("$file") ;;
    .github/workflows/test.yml) workflow_enabled test && filtered_files+=("$file") ;;
    .github/workflows/draft-pr.yml) workflow_enabled draft-pr && filtered_files+=("$file") ;;
    .github/workflows/release-pr.yml) workflow_enabled release-pr && filtered_files+=("$file") ;;
    .github/workflows/release.yml) workflow_enabled release && filtered_files+=("$file") ;;
    *) filtered_files+=("$file") ;;
  esac
done
files=("${filtered_files[@]}")

# Workflows outside the standard baseline are repository-owned extensions.
# The sync operation never deletes or replaces them; surface them explicitly
# so maintainers can verify custom deployment, indexing, or security flows.
standard_workflow() {
  case "$1" in
    ci.yml|codeql.yml|draft-pr.yml|release-pr.yml|release.yml|security.yml|test.yml) return 0 ;;
    reusable-draft-pr.yml|reusable-release-pr.yml) return 0 ;;
    *) return 1 ;;
  esac
}

protected_standard_file() {
  case "$1" in
    AGENTS.md|.github/CODE_OF_CONDUCT.md|.github/CONTRIBUTING.md|.github/PULL_REQUEST_TEMPLATE.md|.github/SECURITY.md|.github/ISSUE_TEMPLATE/*)
      return 0 ;;
    *) return 1 ;;
  esac
}

custom_workflows=()
if [ -d .github/workflows ]; then
  while IFS= read -r workflow; do
    workflow="${workflow#./}"
    workflow="${workflow#.github/workflows/}"
    standard_workflow "$workflow" || custom_workflows+=("$workflow")
  done < <(find .github/workflows -maxdepth 1 -type f -print | sort)
fi
if [ "${#custom_workflows[@]}" -gt 0 ]; then
  printf 'Preserving custom workflows: %s\n' "${custom_workflows[*]}"
fi

changed=0
for file in "${files[@]}"; do
  template_file="$template_root/$file"
  # npm renames .gitignore to .npmignore when installing a package. Treat the
  # renamed file as the same template asset so packaged initialization works.
  if [ "$file" = .gitignore ] && [ ! -f "$template_file" ] && [ -f "$template_root/.npmignore" ]; then
    template_file="$template_root/.npmignore"
  fi
  if [ ! -f "$template_file" ]; then
    echo "Template file missing: $file" >&2
    exit 1
  fi
  if [ "$force" != true ] && [ -f "$file" ] && protected_standard_file "$file"; then
    printf 'Preserving existing authored file %s (use --force to refresh).\n' "$file"
    continue
  fi
  if { [ "$file" = LICENSE ] || [ "$file" = NOTICE ]; } && [ "$license" = preserve ] && [ -f "$file" ]; then
    continue
  fi
  if { [ "$file" = LICENSE ] || [ "$file" = NOTICE ]; } && [ "$license" = none ]; then
    continue
  fi
  if [ "$file" = .github/CODEOWNERS ] && [ -f "$file" ]; then
    continue
  fi
  if [ ! -f "$file" ] || ! cmp -s "$template_file" "$file"; then
    changed=$((changed + 1))
    if [ "$mode" = "check" ]; then
      printf 'Would sync %s\n' "$file"
    else
      mkdir -p "$(dirname "$file")"
      if [ "$file" = .gitignore ] && [ -f "$file" ]; then
        # Keep repository-specific ignore rules while refreshing the shared
        # baseline. This prevents a template sync from hiding generated files
        # or local tooling that only one project uses.
        gitignore_backup="$(mktemp)"
        custom_ignores="$(mktemp)"
        cp "$file" "$gitignore_backup"
        cp "$template_file" "$file"
        awk 'NR == FNR { seen[$0] = 1; next } /^# Repository-specific rules$/ { next } NF && !seen[$0] { print }' \
          "$template_file" "$gitignore_backup" > "$custom_ignores"
        if [ -s "$custom_ignores" ]; then
          {
            printf '\n# Repository-specific rules\n'
            cat "$custom_ignores"
          } >> "$file"
        fi
        rm -f "$gitignore_backup" "$custom_ignores"
        gitignore_backup=""
        custom_ignores=""
      else
        cp "$template_file" "$file"
      fi
      case "$file" in
        .githooks/*|.github/scripts/*) chmod +x "$file" ;;
      esac
      printf 'Synced %s\n' "$file"
    fi
  fi
done

# Reusable workflow callers must use a literal repository/ref. Render the
# selected runtime repository while leaving custom workflows untouched. Read
# the source's configured runtime first so forks remain self-contained.
source_runtime_repository=""
source_runtime_ref=""
source_config="$template_root/.github/code-foundry.yml"
[ -f "$source_config" ] || source_config="$template_root/.github/template.yml"
if [ -f "$source_config" ]; then
  source_runtime_repository="$(awk -F': ' '/^runtime_repository:/ {print $2; exit}' "$source_config")"
  source_runtime_ref="$(awk -F': ' '/^runtime_ref:/ {print $2; exit}' "$source_config")"
fi
[ -n "$source_runtime_repository" ] || source_runtime_repository="0xPlayerOne/code-foundry"
[ -n "$source_runtime_ref" ] || source_runtime_ref="$runtime_ref"
for file in .github/workflows/ci.yml .github/workflows/test.yml .github/workflows/security.yml .github/workflows/codeql.yml .github/workflows/draft-pr.yml .github/workflows/release-pr.yml .github/workflows/release.yml; do
  [ -f "$file" ] || continue
  if grep -qF "$source_runtime_repository@" "$file" || grep -q 'runtime-ref:' "$file"; then
    changed=$((changed + 1))
    if [ "$mode" = "check" ]; then
      printf 'Would render runtime repository in %s\n' "$file"
    else
      runner_value="$runner"
      case "$file" in
        .github/workflows/ci.yml) runner_value="$ci_runner" ;;
        .github/workflows/test.yml) runner_value="$test_runner" ;;
        .github/workflows/security.yml) runner_value="$security_runner" ;;
        .github/workflows/codeql.yml) runner_value="$codeql_runner" ;;
        .github/workflows/draft-pr.yml|.github/workflows/release-pr.yml) runner_value="$pr_runner" ;;
        .github/workflows/release.yml) runner_value="$release_runner" ;;
      esac
      rendered_workflow="$(mktemp)"
      sed -E \
        -e "s#${source_runtime_repository}@[^[:space:]]+#${runtime_repository}@${runtime_ref}#g" \
        -e "s#runtime-ref: [^[:space:]]+#runtime-ref: ${runtime_ref}#g" \
        -e "s#^      runner: [^[:space:]]+#      runner: ${runner_value}#" \
        -e "s#^      unit-runner: [^[:space:]]+#      unit-runner: ${unit_runner}#" \
        "$file" > "$rendered_workflow"
      mv "$rendered_workflow" "$file"
      printf 'Rendered runtime repository in %s\n' "$file"
    fi
  fi
done

write_license() {
  local owner license_source
  [ "$license" != preserve ] || return 0
  [ "$license" != none ] || return 0
  if [ -n "$license_file" ]; then
    [ -f "$license_file" ] || { printf 'License file not found: %s\n' "$license_file" >&2; exit 1; }
    license_source="$license_file"
  else
    case "$license" in
      agpl-3.0-or-later) license_source="$template_root/LICENSE" ;;
      mit) license_source="$template_root/.github/licenses/MIT.txt" ;;
    esac
    [ -f "$license_source" ] || { printf 'License template missing: %s\n' "$license_source" >&2; exit 1; }
  fi
  changed=$((changed + 1))
  if [ "$mode" = check ]; then
    printf 'Would generate LICENSE (%s)\n' "$license"
    return
  fi
  owner="${REPO_FOUNDRY_LICENSE_OWNER:-$(git config user.name 2>/dev/null || true)}"
  [ -n "$owner" ] || owner="Project contributors"
  sed "s/PROJECT_OWNER/$owner/g; s/CURRENT_YEAR/$(date +%Y)/g" "$license_source" > LICENSE
  {
    printf 'Copyright (C) %s %s\n\n' "$(date +%Y)" "$owner"
    printf 'This repository is distributed under %s.\n' "$license"
  } > NOTICE
  printf 'Generated LICENSE and NOTICE for %s.\n' "$license"
}

write_license

# Release Please owns changelog updates after initialization. Preserve an
# existing project history, but give new repositories the expected file.
if [ ! -f CHANGELOG.md ]; then
  changed=$((changed + 1))
  if [ "$mode" = "check" ]; then
    printf '%s\n' 'Would initialize CHANGELOG.md'
  else
    cat > CHANGELOG.md <<'EOF'
# Changelog

All notable changes to this project are documented here.
EOF
    printf '%s\n' 'Initialized CHANGELOG.md'
  fi
fi

detect_languages() {
  local detected=()
  if [ -f package.json ] || git ls-files -- '*.js' '*.jsx' '*.ts' '*.tsx' | grep -q .; then
    detected+=(typescript)
  fi
  if [ -f Cargo.toml ] || git ls-files -- '*.rs' | grep -q .; then
    detected+=(rust)
  fi
  if [ -f pyproject.toml ] || [ -f requirements.txt ] || [ -f requirements-dev.txt ] ||
    git ls-files -- '*.py' ':!.github/**' | grep -q .; then
    detected+=(python)
  fi
  if find . -path './.git' -prune -o -type f -name '*.sol' -print | grep -q .; then
    detected+=(solidity)
  fi
  printf '%s\n' "${detected[*]}"
}

initialize_mise() {
  local selected_languages="$1"
  local language selected_package_manager
  if [ -f .mise.toml ]; then
    return
  fi
  if [ "$selected_languages" = auto ]; then
    selected_languages="$(detect_languages)"
  fi
  selected_package_manager="$package_manager"
  if [ "$selected_package_manager" = auto ]; then
    if [ -f bun.lock ] || [ -f bun.lockb ]; then selected_package_manager=bun
    elif [ -f pnpm-lock.yaml ]; then selected_package_manager=pnpm
    elif [ -f yarn.lock ]; then selected_package_manager=yarn
    elif [ -f package-lock.json ]; then selected_package_manager=npm
    else selected_package_manager=bun
    fi
  fi
  changed=$((changed + 1))
  if [ "$mode" = check ]; then
    printf '%s\n' 'Would initialize .mise.toml'
    return
  fi
  {
    printf '%s\n' '[tools]'
    for language in $(normalize_csv "$selected_languages"); do
      case "$language" in
        typescript|solidity)
          printf 'node = "%s"\n' "$node_version"
          if [ "$selected_package_manager" = bun ]; then
            printf 'bun = "%s"\n' "$bun_version"
          fi
          ;;
        python)
          printf 'python = "%s"\n' "$python_version"
          printf 'uv = "%s"\n' "$uv_version"
          printf 'ruff = "%s"\n' "$ruff_version"
          ;;
        rust)
          printf 'rust = { version = "%s", components = ["rustfmt", "clippy"] }\n' "$rust_version"
          ;;
      esac
    done | awk '!seen[$0]++'
    printf '\n%s\n' '[settings]'
    printf '%s\n' 'experimental = true'
  } > .mise.toml
  printf '%s\n' 'Initialized .mise.toml'
}

initialize_mise "$languages"

initialize_mise_lock() {
  if [ ! -f .mise.toml ] || [ -f mise.lock ]; then
    return
  fi
  if ! command -v mise >/dev/null 2>&1; then
    return
  fi
  changed=$((changed + 1))
  if [ "$mode" = check ]; then
    printf '%s\n' 'Would initialize mise.lock'
    return
  fi
  if MISE_TRUSTED_CONFIG_PATHS="$PWD" mise lock >/dev/null 2>&1; then
    printf '%s\n' 'Initialized mise.lock'
  else
    printf '%s\n' 'Warning: mise.lock could not be generated; CI will resolve pinned tools normally.' >&2
  fi
}

initialize_mise_lock

if [ -x .github/scripts/sync-codeowners.sh ]; then
  if [ "$mode" = "check" ]; then
    bash .github/scripts/sync-codeowners.sh --check
  else
    bash .github/scripts/sync-codeowners.sh --apply
  fi
fi

# Release Please's simple strategy needs a version file. Initialize one only
# for Solidity-only repositories, where no package manifest owns the version.
if [ ! -f version.txt ] && find . -path './.git' -prune -o -type f -name '*.sol' -print | grep -q . &&
  [ ! -f package.json ] && [ ! -f Cargo.toml ] && [ ! -f pyproject.toml ]; then
  changed=$((changed + 1))
  if [ "$mode" = "check" ]; then
    printf '%s\n' 'Would initialize version.txt'
  else
    printf '%s\n' '0.1.0' > version.txt
    printf '%s\n' 'Initialized version.txt'
  fi
fi

if [ "$mode" = "apply" ]; then
  git config core.hooksPath .githooks
  mkdir -p .github
  {
    printf 'version: 1\n'
    if [ -n "$template_ref" ]; then
      printf 'template: %s\n' "$template_ref"
    fi
    printf 'profile: %s\n' "$profile"
    printf 'languages: %s\n' "$languages"
    printf 'features: %s\n' "$features"
    if [ -n "$package_manager" ]; then
      printf 'package_manager: %s\n' "$package_manager"
    fi
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
    printf 'prune_standard: %s\n' "$prune_standard"
    printf 'cache_packages: %s\n' "$cache_packages"
    printf 'cache_build: %s\n' "$cache_build"
    printf 'coverage_minimum: %s\n' "$coverage_minimum"
    printf 'turbo_remote: %s\n' "$turbo_remote"
    printf 'release_type: %s\n' "$release_type"
    printf 'npm_publish: %s\n' "$npm_publish"
    printf 'license: %s\n' "$license"
    if [ -n "$license_file" ]; then
      printf 'license_file: %s\n' "$license_file"
    fi
  } > .github/code-foundry.yml
  if [ "$config_path" = .github/template.yml ] && [ -f .github/template.yml ]; then
    rm .github/template.yml
    printf '%s\n' 'Migrated .github/template.yml to .github/code-foundry.yml.'
  fi
fi

if [ "$prune" = true ]; then
  for workflow in ci codeql security test draft-pr release-pr release; do
    file=".github/workflows/$workflow.yml"
    if [ -f "$file" ] && ! workflow_enabled "$workflow"; then
      changed=$((changed + 1))
      if [ "$mode" = "check" ]; then
        printf 'Would remove disabled standard workflow %s\n' "$file"
      else
        rm "$file"
        printf 'Removed disabled standard workflow %s\n' "$file"
      fi
    fi
  done
  if [ -f .github/dependabot.yml ] && ! workflow_enabled dependabot; then
    changed=$((changed + 1))
    if [ "$mode" = "check" ]; then
      printf 'Would remove disabled standard configuration .github/dependabot.yml\n'
    else
      rm .github/dependabot.yml
      printf '%s\n' 'Removed disabled standard configuration .github/dependabot.yml'
    fi
  fi
fi

printf '%s\n' "$changed baseline file(s) differ."
