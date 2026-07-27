#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: sync-template.sh --source PATH_OR_URL [options]

Synchronize the repository-owned baseline without replacing project-specific
README files, mise tool selections, or additional workflows.

Options:
  --languages LIST  auto or comma-separated: typescript,rust,python,solidity
  --features LIST   all or comma-separated standard features
  --package-manager NAME  auto, bun, pnpm, yarn, or npm
  --check           Preview changes (default)
  --apply           Apply changes
  --prune           Remove disabled standard workflows
EOF
}

source_ref="main"
mode="check"
source=""
temp_dir=""
languages="auto"
features="all"
prune=false
languages_set=false
features_set=false
package_manager=""
package_manager_set=false
template_ref=""
release_type="auto"
npm_publish="false"
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
    --languages) languages="${2:?missing language list}"; languages_set=true; shift 2 ;;
    --features) features="${2:?missing feature list}"; features_set=true; shift 2 ;;
    --package-manager) package_manager="${2:?missing package manager}"; package_manager_set=true; shift 2 ;;
    --check) mode="check"; shift ;;
    --apply) mode="apply"; shift ;;
    --prune) prune=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[ -n "$source" ] || { usage >&2; exit 2; }
if [ -f .github/template.yml ]; then
  if [ "$languages_set" = false ]; then
    configured_languages="$(awk -F': ' '/^languages:/ {print $2; exit}' .github/template.yml)"
    [ -n "$configured_languages" ] && languages="$configured_languages"
  fi
  if [ "$features_set" = false ]; then
    configured_features="$(awk -F': ' '/^features:/ {print $2; exit}' .github/template.yml)"
    [ -n "$configured_features" ] && features="$configured_features"
  fi
  if [ "$package_manager_set" = false ]; then
    package_manager="$(awk -F': ' '/^package_manager:/ {print $2; exit}' .github/template.yml)"
  fi
  template_ref="$(awk -F': ' '/^template:/ {print $2; exit}' .github/template.yml)"
  configured_release_type="$(awk -F': ' '/^release_type:/ {print $2; exit}' .github/template.yml)"
  configured_npm_publish="$(awk -F': ' '/^npm_publish:/ {print $2; exit}' .github/template.yml)"
  [ -n "$configured_release_type" ] && release_type="$configured_release_type"
  [ -n "$configured_npm_publish" ] && npm_publish="$configured_npm_publish"
fi
[ -n "$package_manager" ] || package_manager=auto
case "$package_manager" in
  auto|bun|pnpm|yarn|npm) ;;
  *) printf 'Unsupported package manager: %s\n' "$package_manager" >&2; exit 2 ;;
esac
validate_list language "$languages" "$valid_languages"
validate_list feature "$features" "$valid_features"
contains_word "$valid_release_types" "$release_type" || {
  printf 'Unsupported release type: %s\n' "$release_type" >&2
  exit 2
}

if [ -d "$source" ] && [ -f "$source/.github/scripts/sync-template.sh" ]; then
  template_root="$source"
else
  command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 1; }
  temp_dir="$(mktemp -d)"
  git clone --quiet --depth 1 --branch "$source_ref" "$source" "$temp_dir/template-repo"
  template_root="$temp_dir/template-repo"
fi

if [ -f "$template_root/package.json" ]; then
  template_version="$(awk -F'"' '/"version"[[:space:]]*:/ {print $4; exit}' "$template_root/package.json")"
  [ -n "$template_version" ] && template_ref="repo-foundry@$template_version"
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
  .github/template.yml.example
  .github/CODE_OF_CONDUCT.md
  .github/CONTRIBUTING.md
  .github/PULL_REQUEST_TEMPLATE.md
  .github/SECURITY.md
  .github/dependabot.yml
  .github/ISSUE_TEMPLATE/bug_report.yml
  .github/ISSUE_TEMPLATE/config.yml
  .github/ISSUE_TEMPLATE/feature_request.yml
  .github/actions/setup/action.yml
  .github/scripts/bootstrap.sh
  .github/scripts/changed-files.sh
  .github/scripts/ci.sh
  .github/scripts/codeql-languages.sh
  .github/scripts/doctor.sh
  .github/scripts/security.sh
  .github/scripts/sitecustomize.py
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
    printf 'languages: %s\n' "$languages"
    printf 'features: %s\n' "$features"
    if [ -n "$package_manager" ]; then
      printf 'package_manager: %s\n' "$package_manager"
    fi
    printf 'release_type: %s\n' "$release_type"
    printf 'npm_publish: %s\n' "$npm_publish"
  } > .github/template.yml
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
fi

printf '%s\n' "$changed baseline file(s) differ."
