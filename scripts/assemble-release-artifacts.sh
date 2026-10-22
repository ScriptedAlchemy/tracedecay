#!/usr/bin/env bash
# Join new outputs with attested retained assets without replacing published bytes.
# Usage: assemble-release-artifacts.sh TAG SOURCE_SHA REPO PROFILE SIGNER_WORKFLOW BUILD_REQUIRED
set -euo pipefail
[[ $# == 6 ]] || { echo "expected tag, source SHA, repository, profile, signer workflow and build-required" >&2; exit 2; }
tag=$1 source_sha=$2 repo=$3 profile=$4 signer_workflow=$5 build_required=$6
case "$profile" in
  stable) prefix=tracedecay ;;
  beta) prefix=tracedecay-beta ;;
  *) echo "unknown release profile: $profile" >&2; exit 2 ;;
esac
[[ "$build_required" == true || "$build_required" == false ]]
retained_dir="$(mktemp -d)"
trap 'rm -rf -- "$retained_dir"' EXIT
mkdir binaries mcpbs
mkdir inventories new-inventories

gh api "repos/${repo}/releases/tags/${tag}" --jq '.assets[].name' > "$retained_dir/remote-names"
python3 .release-automation/scripts/plan-release-recovery.py \
  --manifest .github/release-targets.json --tag "$tag" --profile "$profile" \
  --asset-names "$retained_dir/remote-names" \
  --retained-output "$retained_dir/names" --github-output "$retained_dir/planner-output"
mkdir "$retained_dir/assets"
.release-automation/scripts/verify-retained-release-assets.sh \
  --tag "$tag" --repo "$repo" --signer-workflow "$signer_workflow" \
  --source-digest "$source_sha" \
  --signer-ref "refs/tags/${tag}" --signer-ref refs/heads/master \
  --asset-names "$retained_dir/names" --download-dir "$retained_dir/assets"
while IFS= read -r asset; do
  [[ -z "$asset" ]] && continue
  if [[ "$asset" == *.mcpb ]]; then
    cp "$retained_dir/assets/$asset" "mcpbs/$asset"
  elif [[ "$asset" == *.spdx.json ]]; then
    cp "$retained_dir/assets/$asset" "inventories/$asset"
  else
    cp "$retained_dir/assets/$asset" "binaries/$asset"
  fi
done < "$retained_dir/names"
if [[ "$build_required" == true ]]; then
  shopt -s nullglob
  for artifact in new-binaries/"${prefix}"-*; do
    destination="binaries/$(basename "$artifact")"
    [[ -e "$destination" ]] || cp "$artifact" "$destination"
  done
  for artifact in new-mcpbs/*.mcpb; do
    destination="mcpbs/$(basename "$artifact")"
    [[ -e "$destination" ]] || cp "$artifact" "$destination"
  done
  for scope in new-inventory-scopes/*.json; do
    [[ -f "$scope" ]] || continue
    platform="$(jq -r '.platform' "$scope")"
    archive_format="$(jq -r --arg platform "$platform" \
      '.include[] | select(.name == $platform) | .archive' .github/release-targets.json)"
    archive="${prefix}-${tag}-${platform}.${archive_format}"
    mcpb="${prefix}-${tag}-${platform}.mcpb"
    inventory="${prefix}-${tag}-${platform}.spdx.json"
    if [[ -e "inventories/$inventory" ]]; then
      continue
    fi
    python3 .release-automation/scripts/generate-release-inventory.py document \
      --source-root . \
      --scope "$scope" \
      --archive "binaries/$archive" \
      --mcpb "mcpbs/$mcpb" \
      --tag "$tag" \
      --source-sha "$source_sha" \
      --source-epoch "$(git show -s --format=%ct "$source_sha")" \
      --repository "$repo" \
      --output "new-inventories/$inventory"
    cp "new-inventories/$inventory" "inventories/$inventory"
  done
fi
python3 .release-automation/scripts/check-release-artifacts.py \
  --manifest .github/release-targets.json --tag "$tag" --profile "$profile" \
  --binaries binaries --mcpbs mcpbs --inventories inventories
for bundle in mcpbs/*.mcpb; do
  platform="${bundle#mcpbs/"${prefix}-${tag}-"}"
  platform="${platform%.mcpb}"
  python3 scripts/build-mcpb.py verify \
    --bundle "$bundle" --version "${tag#v}" --platform "$platform"
done
for inventory in inventories/*.spdx.json; do
  platform="${inventory#inventories/${prefix}-${tag}-}"
  platform="${platform%.spdx.json}"
  archive_format="$(jq -r --arg platform "$platform" \
    '.include[] | select(.name == $platform) | .archive' .github/release-targets.json)"
  python3 .release-automation/scripts/generate-release-inventory.py verify \
    --inventory "$inventory" \
    --archive "binaries/${prefix}-${tag}-${platform}.${archive_format}" \
    --mcpb "mcpbs/${prefix}-${tag}-${platform}.mcpb"
done
