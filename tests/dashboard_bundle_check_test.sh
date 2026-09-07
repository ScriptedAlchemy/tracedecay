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
