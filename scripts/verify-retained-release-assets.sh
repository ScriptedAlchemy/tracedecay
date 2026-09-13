#!/usr/bin/env bash
# Verifies GitHub release assets against their build attestations, optionally
# downloading them first. One authority for the download+verify loop the
# release workflows previously open-coded per step, so the signer-workflow and
# source-ref flags cannot drift between copies.
#
# Modes (exactly one):
#   --asset-names FILE --download-dir DIR
#       Download each asset named in FILE (one per line, blanks skipped) from
#       the release into DIR, then verify it.
#   --files FILE...
#       Verify already-downloaded files. Must be last; consumes the remaining
#       arguments.
#
# Required flags:
#   --tag TAG                  release tag; the source ref is refs/tags/TAG
#   --repo OWNER/REPO
#   --signer-workflow PATH     e.g. OWNER/REPO/.github/workflows/release.yml
#   --source-digest SHA        commit the tag must attest to
#
# Requires an authenticated `gh` (GH_TOKEN or ambient credentials).
set -euo pipefail

usage() {
  echo "usage: $0 --tag TAG --repo OWNER/REPO --signer-workflow PATH --source-digest SHA (--asset-names FILE --download-dir DIR | --files FILE...)" >&2
  exit 2
}

tag=""
repo=""
signer_workflow=""
source_digest=""
asset_names=""
download_dir=""
files=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) tag="$2"; shift 2 ;;
    --repo) repo="$2"; shift 2 ;;
    --signer-workflow) signer_workflow="$2"; shift 2 ;;
    --source-digest) source_digest="$2"; shift 2 ;;
    --asset-names) asset_names="$2"; shift 2 ;;
    --download-dir) download_dir="$2"; shift 2 ;;
    --files) shift; files=("$@"); break ;;
    *) usage ;;
  esac
done

[[ -n "$tag" && -n "$repo" && -n "$signer_workflow" && -n "$source_digest" ]] || usage

verify_asset() {
  gh attestation verify "$1" \
    --repo "$repo" \
    --signer-workflow "$signer_workflow" \
    --source-ref "refs/tags/${tag}" \
    --source-digest "$source_digest" \
    --deny-self-hosted-runners >/dev/null
}

if [[ -n "$asset_names" ]]; then
  [[ -n "$download_dir" && ${#files[@]} -eq 0 ]] || usage
  while IFS= read -r asset; do
    [[ -z "$asset" ]] && continue
    gh release download "$tag" --repo "$repo" \
      --pattern "$asset" --dir "$download_dir"
    verify_asset "$download_dir/$asset"
  done < "$asset_names"
else
  [[ ${#files[@]} -gt 0 ]] || usage
  for file in "${files[@]}"; do
    verify_asset "$file"
  done
fi
