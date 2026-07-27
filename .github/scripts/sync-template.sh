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
gitignore_backup=""
custom_ignores=""

valid_languages="typescript rust python solidity"
valid_features="ci codeql security test draft-pr release-pr release dependabot"

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
  package_manager="$(awk -F': ' '/^package_manager:/ {print $2; exit}' .github/template.yml)"
fi
validate_list language "$languages" "$valid_languages"
validate_list feature "$features" "$valid_features"

if [ -d "$source" ] && [ -f "$source/.github/scripts/sync-template.sh" ]; then
  template_root="$source"
else
  command -v git >/dev/null 2>&1 || { echo "git is required" >&2; exit 1; }
  temp_dir="$(mktemp -d)"
  git clone --quiet --depth 1 --branch "$source_ref" "$source" "$temp_dir/template-repo"
  template_root="$temp_dir/template-repo"
fi

files=(
  .editorconfig
  .gitattributes
  .gitignore
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
  .github/scripts/ci.sh
  .github/scripts/codeql-languages.sh
  .github/scripts/doctor.sh
  .github/scripts/security.sh
  .github/scripts/sitecustomize.py
  .github/scripts/sync-template.sh
  .github/scripts/init-repo.sh
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

optional_files=(.mise.toml)

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

for file in "${optional_files[@]}"; do
  if [ ! -f "$file" ]; then
    changed=$((changed + 1))
    if [ "$mode" = "check" ]; then
      printf 'Would initialize %s\n' "$file"
    else
      cp "$template_root/$file" "$file"
      printf 'Initialized %s\n' "$file"
    fi
  fi
done

if [ "$mode" = "apply" ]; then
  git config core.hooksPath .githooks
  mkdir -p .github
  {
    printf 'version: 1\n'
    printf 'languages: %s\n' "$languages"
    printf 'features: %s\n' "$features"
    if [ -n "$package_manager" ]; then
      printf 'package_manager: %s\n' "$package_manager"
    fi
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
