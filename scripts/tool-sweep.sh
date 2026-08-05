#!/usr/bin/env bash
# Catalog-complete production MCP sweep. Inventory and deadlines come only
# from the release binary's negotiated tools/list metadata.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat >&2 <<'EOF'
Usage: scripts/tool-sweep.sh [--bin PATH] [--out DIR] [--shards COUNT]

Audits every production MCP tool advertised by the supplied release binary.
Reads share one isolated daemon; each mutation with a verified reversible
journey gets its own disposable daemon, profile, and fixture. An advertised
mutation without such a journey is an explicit failed result. Reports include
consolidated JSON/JUnit plus per-phase logs and timings.
EOF
}

BIN="${TRACEDECAY_BIN:-$REPO_ROOT/target/release/tracedecay}"
OUT=""
SHARDS=4

while (($# > 0)); do
  case "$1" in
    --bin)
      (($# >= 2)) || { usage; exit 2; }
      BIN="$2"
      shift 2
      ;;
    --out)
      (($# >= 2)) || { usage; exit 2; }
      OUT="$2"
      shift 2
      ;;
    --shards)
      (($# >= 2)) || { usage; exit 2; }
      SHARDS="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage
      exit 2
      ;;
  esac
done

[[ "$SHARDS" =~ ^[1-9][0-9]*$ ]] || { echo "error: --shards must be positive" >&2; exit 2; }
[[ -x "$BIN" ]] || { echo "error: tracedecay binary is not executable: $BIN" >&2; exit 2; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

if [[ -z "$OUT" ]]; then
  OUT="$REPO_ROOT/target/tool-sweep/run-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

export PYTHONDONTWRITEBYTECODE=1

set +e
python3 "$REPO_ROOT/tests/tool_sweep_suite/orchestrator.py" \
  --repo "$REPO_ROOT" \
  --bin "$BIN" \
  --out "$OUT" \
  --shards "$SHARDS" \
  > >(tee "$OUT/sweep.stdout.log") \
  2> >(tee "$OUT/sweep.stderr.log" >&2)
status=$?
set -e

echo "MCP tool sweep artifacts: $OUT"
exit "$status"
