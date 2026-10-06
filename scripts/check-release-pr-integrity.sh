#!/usr/bin/env bash
# Validates a release PR: only release metadata changed, and Cargo.lock matches
# the bumped manifests. Before merging a release PR by hand, run it with the PR
# head checked out: scripts/check-release-pr-integrity.sh origin/master HEAD
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
    .release-please-manifest.json | CHANGELOG.md | Cargo.lock | Cargo.toml | crates/tracedecay-cli/Cargo.toml | crates/tracedecay/BUILD.bazel | crates/tracedecay-cli/BUILD.bazel | crates/tracedecay-project/BUILD.bazel | server.json | version.txt)
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

# Release-please rewrites the release branch on every master push, dropping
# the lockfile commit, and every `--locked` build fails once a lagging lock
# merges. Compare each local package's manifest version with its lock entry.
python3 - <<'PY'
import glob
import sys
import tomllib
from pathlib import Path


def load(path):
    return tomllib.loads(Path(path).read_text())


root = load("Cargo.toml")
workspace = root.get("workspace", {})
workspace_version = workspace.get("package", {}).get("version")
packages = [root["package"]] if "package" in root else []
for pattern in workspace.get("members", []):
    for member in sorted(glob.glob(pattern)):
        packages.append(load(Path(member, "Cargo.toml"))["package"])

locked = {
    entry["name"]: entry["version"]
    for entry in load("Cargo.lock").get("package", [])
    if "source" not in entry
}
stale = []
for package in packages:
    version = package.get("version", "0.0.0")
    if version == {"workspace": True}:
        version = workspace_version
    if locked.get(package["name"]) != version:
        stale.append(f"{package['name']}: Cargo.toml {version}, Cargo.lock {locked.get(package['name'], 'missing')}")

if stale:
    print("release PR integrity: Cargo.lock disagrees with the manifest versions:", file=sys.stderr)
    print("\n".join(f"  {line}" for line in stale), file=sys.stderr)
    print("Refresh Cargo.lock; on a release branch run scripts/update-release-pr-lockfile.sh.", file=sys.stderr)
    sys.exit(1)
PY
