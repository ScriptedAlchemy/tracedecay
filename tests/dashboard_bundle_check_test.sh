#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
checker="$repo_root/scripts/check-dashboard-bundle.py"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

expect_rejected() {
  local name=$1
  local expected=$2
  shift 2

  if output=$("$@" 2>&1); then
    echo "$name: invalid dashboard bundle was accepted" >&2
    exit 1
  fi
  if [[ $output != *"$expected"* ]]; then
    echo "$name: expected error containing '$expected', got:" >&2
    echo "$output" >&2
    exit 1
  fi
}

expect_rejected \
  "missing bundle" \
  "bundle directory is missing" \
  python3 "$checker" "$fixture/missing"

mkdir -p "$fixture/stub"
printf '<!doctype html><div id="root"></div>\n' >"$fixture/stub/index.html"
expect_rejected \
  "stub index" \
  "does not load a local JavaScript asset" \
  python3 "$checker" "$fixture/stub"

mkdir -p "$fixture/missing-asset"
printf '<script src="/static/js/app.js"></script>\n' >"$fixture/missing-asset/index.html"
expect_rejected \
  "missing referenced asset" \
  "referenced asset is missing" \
  python3 "$checker" "$fixture/missing-asset"

mkdir -p "$fixture/empty-asset/static/js"
printf '<script src="/static/js/app.js"></script>\n' >"$fixture/empty-asset/index.html"
: >"$fixture/empty-asset/static/js/app.js"
expect_rejected \
  "empty referenced asset" \
  "referenced asset is empty" \
  python3 "$checker" "$fixture/empty-asset"

mkdir -p "$fixture/placeholder/static/js"
printf '<script src="/static/js/app.js"></script>\n' >"$fixture/placeholder/index.html"
printf 'console.log("placeholder");\n' >"$fixture/placeholder/static/js/app.js"
expect_rejected \
  "placeholder JavaScript" \
  "JavaScript payload is placeholder-sized" \
  python3 "$checker" "$fixture/placeholder"

mkdir -p "$fixture/valid/static/js" "$fixture/valid/static/css"
cat >"$fixture/valid/index.html" <<'HTML'
<!doctype html>
<html>
  <head>
    <link rel="stylesheet" href="/static/css/app.css">
    <script defer src="/static/js/app.js"></script>
  </head>
  <body><div id="root"></div></body>
</html>
HTML
printf 'body { color: #fff; }\n' >"$fixture/valid/static/css/app.css"
python3 - "$fixture/valid/static/js/app.js" <<'PY'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text(
    'document.querySelector("#root").textContent = "ready";\n' + ("// built\n" * 100),
    encoding="utf-8",
)
PY
python3 "$checker" "$fixture/valid"
