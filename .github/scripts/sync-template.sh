#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: sync-template.sh --source PATH_OR_URL [--ref REF] [--check|--apply]

Synchronize the repository-owned baseline without replacing project-specific
README files, mise tool selections, or additional workflows.
EOF
}

source_ref="main"
mode="check"
source=""
temp_dir=""

cleanup() {
  if [ -n "$temp_dir" ]; then rm -rf "$temp_dir"; fi
}
trap cleanup EXIT

while [ "$#" -gt 0 ]; do
  case "$1" in
    --source) source="${2:?missing source path or URL}"; shift 2 ;;
    --ref) source_ref="${2:?missing ref}"; shift 2 ;;
    --check) mode="check"; shift ;;
    --apply) mode="apply"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[ -n "$source" ] || { usage >&2; exit 2; }

if [ -d "$source/.git" ]; then
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

changed=0
for file in "${files[@]}"; do
  if [ ! -f "$template_root/$file" ]; then
    echo "Template file missing: $file" >&2
    exit 1
  fi
  if [ ! -f "$file" ] || ! cmp -s "$template_root/$file" "$file"; then
    changed=$((changed + 1))
    if [ "$mode" = "check" ]; then
      printf 'Would sync %s\n' "$file"
    else
      mkdir -p "$(dirname "$file")"
      cp "$template_root/$file" "$file"
      case "$file" in
        .githooks/*|.github/scripts/*) chmod +x "$file" ;;
      esac
      printf 'Synced %s\n' "$file"
    fi
  fi
done

if [ "$mode" = "apply" ]; then
  git config core.hooksPath .githooks
fi

printf '%s\n' "$changed baseline file(s) differ."
