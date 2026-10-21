#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=../scripts/lib/gate-test.sh
. "$(dirname "${BASH_SOURCE[0]}")/../scripts/lib/gate-test.sh"

checker="$GATE_REPO_ROOT/scripts/check-dashboard-bundle.py"

# Each rejection case: a directory name, the substring the checker must print,
# and the builder that stages that flavor of broken bundle. Table-driven so a
# new rejection reason is one row, not another copy of the run-and-assert pair.
build_missing() {
  : # deliberately never created
}

build_stub() {
  mkdir -p "$1"
  printf '<!doctype html><div id="root"></div>\n' >"$1/index.html"
}

build_missing_asset() {
  mkdir -p "$1"
  printf '<script src="/static/js/app.js"></script>\n' >"$1/index.html"
}

build_empty_asset() {
  mkdir -p "$1/static/js"
  printf '<script src="/static/js/app.js"></script>\n' >"$1/index.html"
  : >"$1/static/js/app.js"
}

build_placeholder() {
  mkdir -p "$1/static/js"
  printf '<script src="/static/js/app.js"></script>\n' >"$1/index.html"
  printf 'console.log("placeholder");\n' >"$1/static/js/app.js"
}

rejected=0
while IFS='|' read -r name builder expected; do
  [[ -n $name ]] || continue
  bundle="$GATE_SCRATCH/$name"
  "$builder" "$bundle"
  gate_run python3 "$checker" "$bundle"
  gate_expect_failure "$name"
  gate_output_contains "$name" "$expected"
  rejected=$((rejected + 1))
done <<'CASES'
missing|build_missing|bundle directory is missing
stub|build_stub|does not load a local JavaScript asset
missing-asset|build_missing_asset|referenced asset is missing
empty-asset|build_empty_asset|referenced asset is empty
placeholder|build_placeholder|JavaScript payload is placeholder-sized
CASES

# A here-doc that stops feeding the loop would turn every rejection case above
# into a silent no-op, so assert the table was actually walked.
[[ $rejected -eq 5 ]] ||
  gate_fail "expected 5 rejection cases to run, ran $rejected"

valid="$GATE_SCRATCH/valid"
mkdir -p "$valid/static/js" "$valid/static/css"
cat >"$valid/index.html" <<'HTML'
<!doctype html>
<html>
  <head>
    <link rel="stylesheet" href="/static/css/app.css">
    <script defer src="/static/js/app.js"></script>
  </head>
  <body><div id="root"></div></body>
</html>
HTML
printf 'body { color: #fff; }\n' >"$valid/static/css/app.css"
python3 - "$valid/static/js/app.js" <<'PY'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text(
    'document.querySelector("#root").textContent = "ready";\n' + ("// built\n" * 100),
    encoding="utf-8",
)
PY
gate_run python3 "$checker" "$valid"
gate_expect_success "valid bundle"

# The producer owns gzip preparation. Its canonical inventory must validate and
# produce the same digest when reused by a fresh native build; altered mappings
# or payload bytes must be refused before staging.
python3 - "$valid" <<'PY'
import gzip
import json
import pathlib
import sys

bundle = pathlib.Path(sys.argv[1])
source = "static/js/app.js"
encoded = source + ".gz"
(bundle / encoded).write_bytes(gzip.compress((bundle / source).read_bytes(), mtime=0))
nested_source = "static/js/app/part.js"
nested_encoded = nested_source + ".gz"
(bundle / nested_source).parent.mkdir()
(bundle / nested_source).write_bytes((bundle / source).read_bytes())
(bundle / nested_encoded).write_bytes(gzip.compress((bundle / nested_source).read_bytes(), mtime=0))
(bundle / "asset-manifest.json").write_text(json.dumps({
    "allFiles": ["index.html", "static/css/app.css", source, encoded, nested_source, nested_encoded],
    "gzip": {nested_source: nested_encoded, source: encoded},
}), encoding="utf-8")
PY
gate_run python3 "$checker" "$valid" --print-digest
gate_expect_success "prepared gzip bundle"

python3 - "$checker" "$valid" <<'PY'
import hashlib
import json
import pathlib
import runpy
import sys

bundle_digest = runpy.run_path(sys.argv[1])["bundle_digest"]
bundle = pathlib.Path(sys.argv[2])
manifest = bundle / "asset-manifest.json"
prepared = json.loads(manifest.read_text(encoding="utf-8"))
prepared_digest = bundle_digest(bundle)
inventory_digest = hashlib.sha256(b"tracedecay-dashboard-bundle-v1\0")
for relative in sorted(prepared["allFiles"]):
    contents = (bundle / relative).read_bytes()
    inventory_digest.update(relative.encode("utf-8") + b"\0")
    inventory_digest.update(len(contents).to_bytes(8, "little"))
    inventory_digest.update(contents)
identity_digest = inventory_digest.hexdigest()
inventory_digest.update(b"\0tracedecay-dashboard-gzip-v1\0")
inventory_digest.update(len(prepared["gzip"]).to_bytes(8, "little"))
for source, encoded in sorted(prepared["gzip"].items()):
    payload_path = encoded.encode("utf-8")
    inventory_digest.update(source.encode("utf-8") + b"\0")
    inventory_digest.update(len(payload_path).to_bytes(8, "little"))
    inventory_digest.update(payload_path)
assert prepared_digest == inventory_digest.hexdigest(), "digest must cover gzip selection"

# Input order and equivalent relative spellings cannot change the selection.
reordered = {
    "allFiles": list(reversed(prepared["allFiles"])),
    "gzip": {"./" + source: "./" + encoded for source, encoded in reversed(list(prepared["gzip"].items()))},
}
manifest.write_text(json.dumps(reordered), encoding="utf-8")
assert bundle_digest(bundle) == prepared_digest, "mapping digest must be canonical"

# Removing only the selection changes the embedded routes and representation.
# The original identity-only digest contract must remain unchanged.
for mapping in [{"gzip": {}}, {}]:
    manifest.write_text(json.dumps({"allFiles": prepared["allFiles"], **mapping}), encoding="utf-8")
    assert bundle_digest(bundle) == identity_digest, "identity-only digest changed"
    assert bundle_digest(bundle) != prepared_digest, "mapping-only changes must change the digest"
manifest.write_text(json.dumps(prepared), encoding="utf-8")
PY

python3 - "$valid/asset-manifest.json" <<'PY'
import json
import pathlib
import sys

manifest = pathlib.Path(sys.argv[1])
value = json.loads(manifest.read_text(encoding="utf-8"))
value["gzip"]["static/js/app.js"] = "static/css/app.css"
manifest.write_text(json.dumps(value), encoding="utf-8")
PY
gate_run python3 "$checker" "$valid"
gate_expect_failure "unrelated gzip payload"
gate_output_contains "unrelated gzip payload" "invalid gzip mapping"

python3 - "$valid" <<'PY'
import gzip
import json
import pathlib
import sys

bundle = pathlib.Path(sys.argv[1])
manifest = bundle / "asset-manifest.json"
value = json.loads(manifest.read_text(encoding="utf-8"))
value["gzip"]["static/js/app.js"] = "static/js/app.js.gz"
manifest.write_text(json.dumps(value), encoding="utf-8")
(bundle / "static/js/app.js.gz").write_bytes(gzip.compress(b"different payload", mtime=0))
PY
gate_run python3 "$checker" "$valid"
gate_expect_failure "mismatched gzip bytes"
gate_output_contains "mismatched gzip bytes" "gzip payload does not match"
