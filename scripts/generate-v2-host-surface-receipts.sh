#!/usr/bin/env bash
set -euo pipefail

readonly GENERATOR_VERSION="1.0.0"
readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd -P)"
readonly FIXTURE_ROOT="tests/fixtures/v2/privacy"
readonly OUTPUT_ROOT="target/v2-privacy/receipts"
readonly HOST_OUTPUT_ROOT="${OUTPUT_ROOT}/hosts"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
command -v sha256sum >/dev/null || { echo "sha256sum is required" >&2; exit 1; }

cd -- "${REPO_ROOT}"
mkdir -p -- "${HOST_OUTPUT_ROOT}"

readonly SURFACES=(
  host-canonical-sources
  host-rendered-trees
  host-component-archives
  host-marketplace-artifacts
  host-owned-config-backups
  host-hook-stdin
  host-probe-diagnostics
)

canonical_digest() {
  printf '%s' "$1" | sha256sum | cut -d' ' -f1
}

manifest_entries="$(mktemp "${OUTPUT_ROOT}/.host-entries.XXXXXX")"
trap 'rm -f -- "${manifest_entries}"' EXIT

for surface_id in "${SURFACES[@]}"; do
  fixture_path="${FIXTURE_ROOT}/${surface_id}.json"
  receipt_path="${HOST_OUTPUT_ROOT}/${surface_id}.json"
  test -f "${fixture_path}" || { echo "missing fixture: ${fixture_path}" >&2; exit 1; }

  actual_surface_id="$(jq -er '.surface_id' "${fixture_path}")"
  fixture_id="$(jq -er '.fixture_id' "${fixture_path}")"
  metadata_only="$(jq -er '.metadata_only' "${fixture_path}")"
  receipt_ref="$(jq -er '.receipt_ref' "${fixture_path}")"
  test "${actual_surface_id}" = "${surface_id}" || { echo "surface mismatch: ${fixture_path}" >&2; exit 1; }
  test "${metadata_only}" = "true" || { echo "fixture is not metadata-only: ${fixture_path}" >&2; exit 1; }
  test "${receipt_ref}" = "${receipt_path}" || { echo "receipt path mismatch: ${fixture_path}" >&2; exit 1; }

  input_sha256="$(sha256sum -- "${fixture_path}" | cut -d' ' -f1)"
  payload="$(jq -cnS \
    --arg surface_id "${surface_id}" \
    --arg fixture_id "${fixture_id}" \
    --arg input_path "${fixture_path}" \
    --arg input_sha256 "${input_sha256}" \
    --arg generator_version "${GENERATOR_VERSION}" \
    --argjson coverage "$(jq -cS '.coverage' "${fixture_path}")" \
    '{
      schema:"tracedecay.v2.host-surface-receipt/v1",
      generator_version:$generator_version,
      surface_id:$surface_id,
      fixture_id:$fixture_id,
      input:{relative_path:$input_path,sha256:$input_sha256},
      coverage:{state:"complete",classes:$coverage},
      artifact_digest_scope:"canonical-json-payload-excluding-artifact-digest"
    }')"
  artifact_digest="$(canonical_digest "${payload}")"
  jq -cS --arg artifact_digest "sha256:${artifact_digest}" \
    '. + {artifact_digest:$artifact_digest}' <<<"${payload}" >"${receipt_path}.tmp"
  mv -- "${receipt_path}.tmp" "${receipt_path}"

  jq -cS '{
    surface_id,
    fixture_id,
    input_sha256:.input.sha256,
    coverage_state:.coverage.state,
    receipt_relative_path:("target/v2-privacy/receipts/hosts/" + .surface_id + ".json"),
    receipt_artifact_digest:.artifact_digest
  }' "${receipt_path}" >>"${manifest_entries}"
done

entries="$(jq -csS 'sort_by(.surface_id)' "${manifest_entries}")"
input_set_digest="$(canonical_digest "$(jq -cS '[.[] | {surface_id,input_sha256}]' <<<"${entries}")")"
manifest_payload="$(jq -cnS \
  --arg generator_version "${GENERATOR_VERSION}" \
  --arg input_set_digest "sha256:${input_set_digest}" \
  --argjson entries "${entries}" \
  '{
    schema:"tracedecay.v2.generated-derivative-surface-manifest/v1",
    generator_version:$generator_version,
    surface_id:"PR2B-GENERATED-DERIVATIVES",
    coverage:{state:"complete",surface_count:($entries|length)},
    input_set_digest:$input_set_digest,
    receipts:$entries,
    artifact_digest_scope:"canonical-json-payload-excluding-artifact-digest"
  }')"
manifest_digest="$(canonical_digest "${manifest_payload}")"
jq -cS --arg artifact_digest "sha256:${manifest_digest}" \
  '. + {artifact_digest:$artifact_digest}' <<<"${manifest_payload}" \
  >"${OUTPUT_ROOT}/generated-derivative-surfaces.json.tmp"
mv -- "${OUTPUT_ROOT}/generated-derivative-surfaces.json.tmp" \
  "${OUTPUT_ROOT}/generated-derivative-surfaces.json"
