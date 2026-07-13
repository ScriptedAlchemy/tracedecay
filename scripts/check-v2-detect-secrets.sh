#!/usr/bin/env bash
set -euo pipefail

readonly REQUIRED_VERSION="1.5.0"
readonly BASELINE=".secrets.baseline"
readonly RECEIPT="target/v2-privacy/receipts/detect-secrets-1.5.0.json"
readonly SURFACE_ID="PR2B-CANDIDATE-TREE"

die() {
  printf 'check-v2-detect-secrets: %s\n' "$*" >&2
  exit 1
}

[[ $# -eq 0 || $# -eq 2 ]] || die "usage: $0 [reviewed-base candidate]"

for executable in detect-secrets git python3 sha256sum tar; do
  command -v "$executable" >/dev/null 2>&1 || die "missing required executable: $executable"
done

actual_version="$(detect-secrets --version 2>/dev/null)" || die "unable to read detect-secrets version"
actual_version="${actual_version#detect-secrets }"
[[ "$actual_version" == "$REQUIRED_VERSION" ]] || \
  die "detect-secrets version mismatch: expected $REQUIRED_VERSION, got $actual_version"

[[ -f "$BASELINE" ]] || die "missing baseline: $BASELINE"
python3 - "$BASELINE" "$REQUIRED_VERSION" <<'PY' || exit 1
import json
import sys

path, required_version = sys.argv[1:]
try:
    with open(path, encoding="utf-8") as handle:
        baseline = json.load(handle)
except (OSError, json.JSONDecodeError) as error:
    raise SystemExit(f"check-v2-detect-secrets: invalid baseline JSON: {error}")

if baseline.get("version") != required_version:
    raise SystemExit("check-v2-detect-secrets: baseline version mismatch")
if not baseline.get("plugins_used"):
    raise SystemExit("check-v2-detect-secrets: baseline has no enabled plugins")
if baseline.get("results") != {}:
    raise SystemExit("check-v2-detect-secrets: baseline contains findings")
PY

candidate="${2:-HEAD}"
candidate="$(git rev-parse --verify "${candidate}^{commit}" 2>/dev/null)" || die "invalid candidate commit"
reviewed_base_json="null"
if [[ $# -eq 2 ]]; then
  reviewed_base="$(git rev-parse --verify "${1}^{commit}" 2>/dev/null)" || die "invalid reviewed base commit"
  git merge-base --is-ancestor "$reviewed_base" "$candidate" || die "reviewed base is not an ancestor of candidate"
  reviewed_base_json="\"$reviewed_base\""
fi

tmp_dir="$(mktemp -d)" || die "unable to create temporary directory"
trap 'rm -rf "$tmp_dir"' EXIT
mkdir -p "$tmp_dir/tree"
git archive "$candidate" | tar -xf - -C "$tmp_dir/tree" || die "unable to materialize candidate tree"
cp "$BASELINE" "$tmp_dir/baseline.json"

(
  cd "$tmp_dir/tree"
  detect-secrets scan --all-files --baseline "$tmp_dir/baseline.json" . >/dev/null
) || die "detect-secrets scan failed"

python3 - "$BASELINE" "$tmp_dir/baseline.json" <<'PY' || exit 1
import json
import sys

def load(path):
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"check-v2-detect-secrets: invalid scanner output: {error}")

expected, scanned = map(load, sys.argv[1:])
results = scanned.get("results")
if not isinstance(results, dict):
    raise SystemExit("check-v2-detect-secrets: scanner omitted results")
if any(results.values()):
    count = sum(len(findings) for findings in results.values())
    raise SystemExit(f"check-v2-detect-secrets: {count} finding(s); candidate content suppressed")

for document in (expected, scanned):
    document.pop("generated_at", None)
if expected != scanned:
    raise SystemExit("check-v2-detect-secrets: stale baseline")
PY

config_sum="$(sha256sum "$BASELINE")" || die "unable to hash baseline"
config_hex="${config_sum%% *}"
config_digest="sha256:$config_hex"
scan_hex="$(python3 - "$tmp_dir/baseline.json" <<'PY'
import hashlib
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    report = json.load(handle)
report.pop("generated_at", None)
canonical = json.dumps(report, sort_keys=True, separators=(",", ":")).encode()
print(hashlib.sha256(canonical).hexdigest())
PY
)" || die "unable to hash scanner report"
mkdir -p "$(dirname "$RECEIPT")"
python3 - "$RECEIPT" "$config_digest" "$reviewed_base_json" "$candidate" "$SURFACE_ID" "$scan_hex" <<'PY'
import hashlib
import json
import os
import sys
import tempfile

receipt_path, config_digest, reviewed_base_json, candidate, surface_id, scan_hex = sys.argv[1:]
payload = {
    "schema_version": 1,
    "tool_name": "detect-secrets",
    "tool_version": "1.5.0",
    "config_digest": config_digest,
    "reviewed_base_commit": json.loads(reviewed_base_json),
    "candidate_commit": candidate,
    "scanned_surface_ids": [surface_id],
    "coverage_state": "complete",
    "finding_count": 0,
}
reviewed_base = payload["reviewed_base_commit"] or ""
evidence = "\n".join((
    payload["tool_version"],
    config_digest.removeprefix("sha256:"),
    reviewed_base,
    candidate,
    scan_hex,
)).encode()
payload["artifact_digest"] = "sha256:" + hashlib.sha256(evidence).hexdigest()

directory = os.path.dirname(receipt_path)
fd, temporary = tempfile.mkstemp(prefix=".detect-secrets-", dir=directory, text=True)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(temporary, receipt_path)
except BaseException:
    try:
        os.unlink(temporary)
    except FileNotFoundError:
        pass
    raise
PY

printf 'detect-secrets %s: 0 findings across %s\n' "$REQUIRED_VERSION" "$SURFACE_ID"
