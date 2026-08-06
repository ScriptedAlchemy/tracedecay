#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <base-ref> <head-ref> [--allow-extra-files]" >&2
  exit 2
}

[[ $# -eq 2 || $# -eq 3 ]] || usage
base_ref=$1
head_ref=$2
allow_extra_files=false
if [[ $# -eq 3 ]]; then
  [[ $3 == "--allow-extra-files" ]] || usage
  allow_extra_files=true
fi

git rev-parse --verify --quiet "${base_ref}^{commit}" >/dev/null || {
  echo "release PR integrity: invalid base ref: $base_ref" >&2
  exit 2
}
git rev-parse --verify --quiet "${head_ref}^{commit}" >/dev/null || {
  echo "release PR integrity: invalid head ref: $head_ref" >&2
  exit 2
}

checked_out_head=$(git rev-parse HEAD)
requested_head=$(git rev-parse "${head_ref}^{commit}")
if [[ $checked_out_head != "$requested_head" ]]; then
  echo "release PR integrity: checkout HEAD must match head ref $head_ref" >&2
  exit 2
fi

tracked_ignored=$(git ls-files --cached --ignored --exclude-standard)
if [[ -n $tracked_ignored ]]; then
  echo "release PR integrity: tracked files must not also be ignored:" >&2
  printf '%s\n' "$tracked_ignored" >&2
  echo "Remove the matching ignore rule before release automation copies the repository." >&2
  exit 1
fi

unexpected=()
destructive_metadata=()
while IFS=$'\t' read -r status path _; do
  [[ -n ${status:-} ]] || continue
  case "$path" in
    .release-please-manifest.json | CHANGELOG.md | Cargo.lock | Cargo.toml | version.txt)
      if [[ $status != M ]]; then
        destructive_metadata+=("$status $path")
      fi
      ;;
    *) unexpected+=("$status $path") ;;
  esac
done < <(git diff --name-status --no-renames "${base_ref}...${head_ref}")

if ((${#destructive_metadata[@]})); then
  echo "release PR integrity: release metadata may only be modified, not added, deleted, or type-changed:" >&2
  printf '  %s\n' "${destructive_metadata[@]}" >&2
  exit 1
fi

if ((${#unexpected[@]})) && [[ $allow_extra_files != true ]]; then
  echo "release PR integrity: release PR contains changes outside release metadata files:" >&2
  printf '  %s\n' "${unexpected[@]}" >&2
  echo "Apply the release-extra-files-approved label only after reviewing every listed path." >&2
  exit 1
fi

if ((${#unexpected[@]})); then
  echo "release PR integrity: explicitly approved extra paths:" >&2
  printf '  %s\n' "${unexpected[@]}" >&2
fi
