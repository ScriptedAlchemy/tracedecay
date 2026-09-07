#!/usr/bin/env bash
# Exercise the production MCP catalog, not a hand-maintained tool inventory.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat >&2 <<'EOF'
Usage: scripts/tool-sweep.sh [--bin PATH] [--out DIR]
                             [--whole-run-deadline-ms MILLIS]

Exercises every tool, resource, and prompt negotiated from the supplied
release binary. Tools use their declared dispatch deadlines; the whole run has
one cancellable deadline and always writes consolidated JSON/JUnit artifacts.
Available mutations run only in fresh, hermetic fixtures through a registered
real producer/consumer/rollback journey. A mutation without such a journey is
reported as failed rather than skipped.
EOF
}

BIN="${TRACEDECAY_BIN:-$REPO_ROOT/target/release/tracedecay}"
OUT=""
WHOLE_RUN_DEADLINE_MS="${TRACEDECAY_SWEEP_WHOLE_RUN_DEADLINE_MS:-1800000}"

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
    --whole-run-deadline-ms)
      (($# >= 2)) || { usage; exit 2; }
      WHOLE_RUN_DEADLINE_MS="$2"
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

[[ "$WHOLE_RUN_DEADLINE_MS" =~ ^[1-9][0-9]*$ ]] || {
  echo "error: --whole-run-deadline-ms must be positive" >&2
  exit 2
}
[[ -x "$BIN" ]] || { echo "error: tracedecay binary is not executable: $BIN" >&2; exit 2; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

if [[ -z "$OUT" ]]; then
  OUT="$REPO_ROOT/target/tool-sweep/run-$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi
TMPDIR="${TMPDIR:-$OUT/tmp}"
mkdir -p "$OUT" "$TMPDIR"
OUT="$(cd "$OUT" && pwd)"
TMPDIR="$(cd "$TMPDIR" && pwd)"
export TMPDIR TMP="$TMPDIR" TEMP="$TMPDIR"

export PYTHONDONTWRITEBYTECODE=1
set +e
python3 "$REPO_ROOT/tests/tool_sweep_suite/orchestrator.py" \
  --repo "$REPO_ROOT" \
  --bin "$BIN" \
  --out "$OUT" \
  --whole-run-deadline-ms "$WHOLE_RUN_DEADLINE_MS" \
  > >(tee "$OUT/sweep.stdout.log") \
  2> >(tee "$OUT/sweep.stderr.log" >&2)
status=$?
set -e

echo "MCP catalog sweep artifacts: $OUT"
exit "$status"
